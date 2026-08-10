//! Capability-authenticated client for the in-bundle native broker.
//!
//! The broker is the only component allowed to materialize a gateway bearer.
//! Rust receives descriptor file handles, never an auth literal, and replaces
//! the in-memory descriptor generation whenever the broker rotates it.

use std::fmt;
use std::io::Read as _;
#[cfg(feature = "test-support")]
use std::os::fd::OwnedFd;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::Hmac;
use hmac::Mac as _;
use rand::random;
use serde::Deserialize;
use serde::Serialize;
use serde::de::DeserializeOwned;
use sha2::Sha256;
use thiserror::Error;
use url::Url;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::AccountProjectionError;
use crate::ContextualConnectionsSnapshot;
use crate::ContextualUsageSnapshot;
use crate::GATEWAY_AUTH_FD_ENV;
use crate::GATEWAY_ENDPOINT_FD_ENV;
use crate::NATIVE_BROKER_CAPABILITY_FD_ENV;
use crate::NATIVE_BROKER_SOCKET_ENV;
#[cfg(feature = "test-support")]
use crate::catalog::CatalogVerificationKeySet;
use crate::release_manifest_sha256_from_environment;

const BROKER_SCHEMA_VERSION: u16 = 1;
const MAX_BROKER_FRAME_BYTES: usize = 16 * 1024;
const MAX_ACCOUNT_READ_RESPONSE_BYTES: usize = 128 * 1024;
const MAX_ACCOUNT_READ_PAYLOAD_BYTES: usize = 94 * 1024;
const MAX_GATEWAY_TOKEN_BYTES: u64 = 8 * 1024;
const MAX_GATEWAY_ENDPOINT_BYTES: u64 = 4 * 1024;
const GATEWAY_ENDPOINT_SCHEMA_VERSION: u16 = 1;
const WHISPLY_GATEWAY_ORIGIN_HOST: &str = "proxy.whisply.net";
const REFRESH_SKEW_MS: i64 = 60_000;
const NATIVE_BROKER_SOCKET_FILENAME: &str = "v1.sock";
const NATIVE_BROKER_DIRECTORY: &str = ".native-broker";

type HmacSha256 = Hmac<Sha256>;

/// Broker operations are deliberately fixed. A dynamic string may never open
/// a new credential or account-control route.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrokerOperation {
    Hello,
    AuthStatus,
    AuthLoginBegin,
    AuthLoginComplete,
    AuthRefresh,
    AuthLogout,
    AuthSwitch,
    RuntimeDescriptors,
    AccountUsageRead,
    AccountConnectionsRead,
    HostControlsSnapshot,
    HostControlsApply,
    HostControlsExecute,
}

impl BrokerOperation {
    fn as_str(self) -> &'static str {
        match self {
            Self::Hello => "broker.hello",
            Self::AuthStatus => "auth.status",
            Self::AuthLoginBegin => "auth.login.begin",
            Self::AuthLoginComplete => "auth.login.complete",
            Self::AuthRefresh => "auth.refresh",
            Self::AuthLogout => "auth.logout",
            Self::AuthSwitch => "auth.switch",
            Self::RuntimeDescriptors => "runtime.descriptors",
            Self::AccountUsageRead => "account.usage.read",
            Self::AccountConnectionsRead => "account.connections.read",
            Self::HostControlsSnapshot => "host.controls.snapshot",
            Self::HostControlsApply => "host.controls.apply",
            Self::HostControlsExecute => "host.controls.execute",
        }
    }

    const fn max_response_frame_bytes(self) -> usize {
        match self {
            Self::AccountUsageRead | Self::AccountConnectionsRead => {
                MAX_ACCOUNT_READ_RESPONSE_BYTES
            }
            Self::HostControlsSnapshot | Self::HostControlsApply | Self::HostControlsExecute => {
                32 * 1024
            }
            _ => MAX_BROKER_FRAME_BYTES,
        }
    }
}

/// Native broker failures exposed without credential, path, or account data.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BrokerErrorCode {
    Unauthenticated,
    Expired,
    StaleEpoch,
    InvalidClient,
    ManifestMismatch,
    DescriptorInvalid,
    Unavailable,
    LoginRequired,
}

impl fmt::Display for BrokerErrorCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::Unauthenticated => "unauthenticated",
            Self::Expired => "expired",
            Self::StaleEpoch => "staleEpoch",
            Self::InvalidClient => "invalidClient",
            Self::ManifestMismatch => "manifestMismatch",
            Self::DescriptorInvalid => "descriptorInvalid",
            Self::Unavailable => "unavailable",
            Self::LoginRequired => "loginRequired",
        };
        formatter.write_str(value)
    }
}

/// Metadata returned by the native broker. Account epochs and descriptor
/// generations are opaque UUIDs, not account identifiers or ordered counters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrokerResponseMetadata {
    pub account_epoch: Option<String>,
    pub descriptor_generation: Option<String>,
    pub expires_at_ms: Option<i64>,
}

/// The account-safe status projection used by CLI and app-server status UI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrokerLoginStatus {
    pub authenticated: bool,
    pub opaque_account_key: Option<String>,
    pub account_epoch: Option<String>,
}

/// The native-owned browser login handoff. The authorization URL is intended
/// only for a browser opener; callers must not log or persist it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrokerLoginBegin {
    pub login_handle: String,
    pub authorization_url: String,
}

/// The runtime-facing half of the fixed host-controls protocol. These values
/// can operate only against a live, broker-registered Mac app owner; callers
/// never receive a socket path, owner lease, reverse capability, or durable
/// settings fallback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostControlsPatch {
    ComputerUseMaster {
        enabled: bool,
    },
    ComputerUseRoute {
        route: HostControlsComputerUseRoute,
        enabled: bool,
    },
    BuiltinCapability {
        capability: HostControlsBuiltinCapability,
        enabled: bool,
    },
    Thread {
        session_id: String,
        directory: Option<HostControlsThreadDirectory>,
        permission_mode: Option<HostControlsPermissionMode>,
        prefer_screen: Option<bool>,
    },
    AudioAssist {
        mode: Option<HostControlsAudioAssistMode>,
        transcript_task_suggestions_enabled: Option<bool>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostControlsComputerUseRoute {
    NativeApps,
    ExistingBrowser,
    SpawnedBrowser,
    LocalFiles,
}

impl HostControlsComputerUseRoute {
    fn as_str(self) -> &'static str {
        match self {
            Self::NativeApps => "native_apps",
            Self::ExistingBrowser => "existing_browser",
            Self::SpawnedBrowser => "spawned_browser",
            Self::LocalFiles => "local_files",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostControlsBuiltinCapability {
    Browser,
    Chrome,
    ComputerUse,
    Documents,
    Pdf,
    Spreadsheets,
    Presentations,
}

impl HostControlsBuiltinCapability {
    fn as_str(self) -> &'static str {
        match self {
            Self::Browser => "browser",
            Self::Chrome => "chrome",
            Self::ComputerUse => "computer_use",
            Self::Documents => "documents",
            Self::Pdf => "pdf",
            Self::Spreadsheets => "spreadsheets",
            Self::Presentations => "presentations",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostControlsPermissionMode {
    Ask,
    Auto,
    FullAccess,
}

impl HostControlsPermissionMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::Auto => "auto",
            Self::FullAccess => "full_access",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostControlsAudioAssistMode {
    Manual,
    Automatic,
}

impl HostControlsAudioAssistMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Automatic => "automatic",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostControlsThreadDirectory {
    NoDirectory,
    Directory { canonical_path: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostControlsCommand {
    StopComputerUse {
        session_id: String,
    },
    RequestInteractive {
        action: HostControlsInteractiveAction,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostControlsInteractiveAction {
    ComputerUseAccessibilityPermission,
    ComputerUseScreenRecordingPermission,
    ChooseThreadDirectory,
    ChooseTrustedApplication,
    ChooseWindow,
    SystemSettings,
}

impl HostControlsInteractiveAction {
    fn as_str(self) -> &'static str {
        match self {
            Self::ComputerUseAccessibilityPermission => "computer_use_accessibility_permission",
            Self::ComputerUseScreenRecordingPermission => {
                "computer_use_screen_recording_permission"
            }
            Self::ChooseThreadDirectory => "choose_thread_directory",
            Self::ChooseTrustedApplication => "choose_trusted_application",
            Self::ChooseWindow => "choose_window",
            Self::SystemSettings => "system_settings",
        }
    }
}

/// A currently active gateway descriptor generation. `Debug` deliberately
/// redacts the bearer and no public field exposes it as serializable data.
#[derive(Clone, PartialEq, Eq)]
pub struct GatewayDescriptorSnapshot {
    api_base_url: String,
    bearer: Zeroizing<String>,
    account_epoch: Option<String>,
    descriptor_generation: Option<String>,
    expires_at_ms: Option<i64>,
}

impl fmt::Debug for GatewayDescriptorSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GatewayDescriptorSnapshot")
            .field("api_base_url", &self.api_base_url)
            .field("bearer", &"[REDACTED]")
            .field("account_epoch", &self.account_epoch)
            .field("descriptor_generation", &self.descriptor_generation)
            .field("expires_at_ms", &self.expires_at_ms)
            .finish()
    }
}

impl GatewayDescriptorSnapshot {
    /// The fixed release-owned API base URL.
    pub fn api_base_url(&self) -> &str {
        &self.api_base_url
    }

    /// Returns bearer bytes only for immediate request construction. Callers
    /// must not persist, log, or forward the result outside gateway headers.
    pub fn bearer(&self) -> &str {
        &self.bearer
    }

    pub fn account_epoch(&self) -> Option<&str> {
        self.account_epoch.as_deref()
    }

    pub fn descriptor_generation(&self) -> Option<&str> {
        self.descriptor_generation.as_deref()
    }

    pub fn expires_at_ms(&self) -> Option<i64> {
        self.expires_at_ms
    }

    fn from_parts(
        bearer_bytes: &[u8],
        endpoint_bytes: &[u8],
        metadata: BrokerResponseMetadata,
    ) -> Result<Self, ManagedGatewayError> {
        let bearer = parse_bearer_descriptor(bearer_bytes)?;
        let api_base_url = parse_gateway_endpoint_descriptor(endpoint_bytes)?;
        let BrokerResponseMetadata {
            account_epoch,
            descriptor_generation,
            expires_at_ms,
        } = metadata;
        match (
            account_epoch.as_deref(),
            descriptor_generation.as_deref(),
            expires_at_ms,
        ) {
            (None, None, None) => {}
            (Some(account_epoch), Some(descriptor_generation), Some(expires_at_ms)) => {
                validate_account_epoch(account_epoch)?;
                validate_descriptor_generation(descriptor_generation)?;
                if expires_at_ms <= now_unix_ms()? {
                    return Err(ManagedGatewayError::InvalidDescriptor);
                }
            }
            _ => return Err(ManagedGatewayError::InvalidDescriptor),
        }
        Ok(Self {
            api_base_url,
            bearer: Zeroizing::new(bearer),
            account_epoch,
            descriptor_generation,
            expires_at_ms,
        })
    }

    #[cfg(test)]
    fn for_test(token: &str, api_base_url: &str) -> Self {
        Self::for_test_with(
            token,
            api_base_url,
            Some("63ff45f8-1a7d-49c0-a892-9df2ca3e0b59"),
            Some(i64::MAX),
        )
    }

    #[cfg(test)]
    fn for_test_with(
        token: &str,
        api_base_url: &str,
        descriptor_generation: Option<&str>,
        expires_at_ms: Option<i64>,
    ) -> Self {
        Self {
            api_base_url: api_base_url.to_string(),
            bearer: Zeroizing::new(token.to_string()),
            account_epoch: descriptor_generation
                .map(|_| "5ce47c6b-387a-4f92-9dd9-0bdb1082bc07".to_string()),
            descriptor_generation: descriptor_generation.map(ToString::to_string),
            expires_at_ms,
        }
    }
}

/// Whether this process received a complete managed Whisply session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManagedGatewaySessionAvailability {
    Available,
    Missing,
    Invalid,
}

/// Native-broker protocol failures. They intentionally avoid rendering a
/// secret, callback URL, capability, or raw broker response.
#[derive(Clone, Debug, Error)]
pub enum BrokerError {
    #[error("Whisply native broker is unavailable on this platform")]
    UnsupportedPlatform,
    #[error("Whisply native broker launch descriptors are invalid")]
    InvalidDescriptor,
    #[error("Whisply native broker capability descriptor is invalid")]
    InvalidCapabilityDescriptor,
    #[error("Whisply native broker socket does not meet local ownership requirements")]
    UnsafeSocket,
    #[error("Whisply native broker peer identity could not be verified")]
    InvalidPeer,
    #[error("Whisply native broker message is invalid or oversized")]
    InvalidMessage,
    #[error("Whisply native broker response did not match its request")]
    ResponseMismatch,
    #[error("Whisply native broker request could not be serialized")]
    Serialize,
    #[error("Whisply native broker is unavailable")]
    Io,
    #[error("Whisply native broker rejected the request: {0}")]
    Rejected(BrokerErrorCode),
    #[error("Whisply release manifest identity is unavailable")]
    MissingReleaseManifest,
}

/// Managed gateway failures that callers may show as a generic account or
/// gateway-state error without exposing trusted descriptor detail.
#[derive(Clone, Debug, Error)]
pub enum ManagedGatewayError {
    #[error("Whisply gateway descriptors are missing")]
    MissingDescriptors,
    #[error("Whisply gateway descriptors are incomplete")]
    IncompleteDescriptors,
    #[error("Whisply gateway descriptor is invalid")]
    InvalidDescriptor,
    #[error("Whisply gateway descriptor could not be read")]
    DescriptorIo,
    #[error("Whisply gateway refresh is unavailable")]
    Broker(#[from] BrokerError),
}

/// Capability-authenticated native broker client. Its capability is retained
/// in zeroizing memory only so every refresh can prove the same runtime launch.
pub struct NativeBrokerClient {
    socket_path: PathBuf,
    capability: Zeroizing<[u8; 32]>,
    release_manifest_sha256: String,
}

// Launch capabilities arrive through a one-shot inherited pipe. Every
// component in a runtime process must therefore share one parsed client rather
// than racing to consume that pipe independently.
static PROCESS_NATIVE_BROKER_CLIENT: OnceLock<
    Result<Option<Arc<NativeBrokerClient>>, BrokerError>,
> = OnceLock::new();

impl fmt::Debug for NativeBrokerClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeBrokerClient")
            .field("socket_path", &"[REDACTED_LOCAL_PATH]")
            .field("capability", &"[REDACTED]")
            .field("release_manifest_sha256", &self.release_manifest_sha256)
            .finish()
    }
}

impl NativeBrokerClient {
    /// Builds a client from inherited launch descriptors. The release hash is
    /// injected only by the verified launcher, never user config or CLI input.
    pub fn from_environment() -> Result<Option<Arc<Self>>, BrokerError> {
        PROCESS_NATIVE_BROKER_CLIENT
            .get_or_init(Self::initialize_from_environment)
            .clone()
    }

    fn initialize_from_environment() -> Result<Option<Arc<Self>>, BrokerError> {
        let Some(socket_path) = std::env::var_os(NATIVE_BROKER_SOCKET_ENV) else {
            return Ok(None);
        };
        let release_manifest_sha256 = release_manifest_sha256_from_environment()
            .ok_or(BrokerError::MissingReleaseManifest)?;
        let capability_fd = inherited_fd(NATIVE_BROKER_CAPABILITY_FD_ENV)
            .map_err(|_| BrokerError::InvalidCapabilityDescriptor)?
            .ok_or(BrokerError::InvalidCapabilityDescriptor)?;
        let capability = read_exact_inherited_capability(capability_fd)?;
        let socket_path = PathBuf::from(socket_path);
        validate_broker_socket_path(&socket_path)?;
        Ok(Some(Arc::new(Self {
            socket_path,
            capability: Zeroizing::new(capability),
            release_manifest_sha256,
        })))
    }

    /// Performs the signed runtime hello required before other broker use.
    pub fn hello(&self) -> Result<BrokerResponseMetadata, BrokerError> {
        self.call_without_descriptors(
            BrokerOperation::Hello,
            None,
            &HelloPayload {
                client_kind: "runtime",
                release_manifest_sha256: &self.release_manifest_sha256,
                runtime_capability_base64_url: None,
            },
        )
        .map(|response| response.metadata)
    }

    pub fn status(&self) -> Result<BrokerLoginStatus, BrokerError> {
        let response =
            self.call_without_descriptors(BrokerOperation::AuthStatus, None, &Empty {})?;
        let payload: StatusPayload = deserialize_payload(&response.payload)?;
        validate_status_payload(&payload)?;
        Ok(BrokerLoginStatus {
            authenticated: payload.authenticated,
            opaque_account_key: payload.opaque_account_key,
            account_epoch: payload.account_epoch,
        })
    }

    pub fn login_begin(&self) -> Result<BrokerLoginBegin, BrokerError> {
        let response = self.call_without_descriptors(
            BrokerOperation::AuthLoginBegin,
            None,
            &LoginBeginPayload {
                completion: "browser_callback",
            },
        )?;
        let payload: LoginBeginResponsePayload = deserialize_payload(&response.payload)?;
        if payload.method != "browser_callback"
            || !is_opaque_uuid(&payload.login_handle)
            || !valid_browser_url(&payload.authorization_url)
        {
            return Err(BrokerError::InvalidMessage);
        }
        Ok(BrokerLoginBegin {
            login_handle: payload.login_handle,
            authorization_url: payload.authorization_url,
        })
    }

    /// Completes a browser callback. The callback URL is sensitive IPC-only
    /// material and is never included in errors or return values.
    pub fn login_complete(
        &self,
        login_handle: &str,
        callback_url: &str,
        account_epoch: Option<&str>,
    ) -> Result<BrokerResponseMetadata, BrokerError> {
        if !is_opaque_uuid(login_handle) || !valid_callback_url(callback_url) {
            return Err(BrokerError::InvalidMessage);
        }
        self.call_without_descriptors(
            BrokerOperation::AuthLoginComplete,
            account_epoch,
            &LoginCompletePayload {
                login_handle,
                callback_url,
            },
        )
        .map(|response| response.metadata)
    }

    pub fn logout(
        &self,
        account_epoch: Option<&str>,
    ) -> Result<BrokerResponseMetadata, BrokerError> {
        self.call_without_descriptors(BrokerOperation::AuthLogout, account_epoch, &Empty {})
            .map(|response| response.metadata)
    }

    pub fn switch_account(
        &self,
        opaque_account_key: &str,
        account_epoch: Option<&str>,
    ) -> Result<BrokerResponseMetadata, BrokerError> {
        if !valid_opaque_account_key(opaque_account_key) {
            return Err(BrokerError::InvalidMessage);
        }
        self.call_without_descriptors(
            BrokerOperation::AuthSwitch,
            account_epoch,
            &SwitchPayload { opaque_account_key },
        )
        .map(|response| response.metadata)
    }

    /// Reads the current account's bounded cost-authoritative Usage projection
    /// through the native broker. This never reaches the Website directly and
    /// never exposes the broker's session bearer to Rust.
    pub fn account_usage(
        &self,
        binding: &BrokerLoginStatus,
    ) -> Result<ContextualUsageSnapshot, AccountProjectionError> {
        let (opaque_account_key, account_epoch) = account_read_binding(binding)?;
        let response = self
            .call_without_descriptors(
                BrokerOperation::AccountUsageRead,
                Some(account_epoch),
                &AccountReadPayload { opaque_account_key },
            )
            .map_err(|_| AccountProjectionError::Unavailable)?;
        validate_account_read_response_epoch(&response, account_epoch)?;
        validate_account_read_payload_size(
            &response.payload,
            AccountProjectionError::InvalidUsage,
        )?;
        let snapshot: ContextualUsageSnapshot = serde_json::from_slice(&response.payload)
            .map_err(|_| AccountProjectionError::InvalidUsage)?;
        snapshot.validate()?;
        Ok(snapshot)
    }

    /// Reads the full bounded Website connections projection through the
    /// native broker. This is deliberately broker-only: Rust has neither an
    /// account bearer nor a direct Website fallback route.
    pub fn account_connections(
        &self,
        binding: &BrokerLoginStatus,
    ) -> Result<ContextualConnectionsSnapshot, AccountProjectionError> {
        let (opaque_account_key, account_epoch) = account_read_binding(binding)?;
        let response = self
            .call_without_descriptors(
                BrokerOperation::AccountConnectionsRead,
                Some(account_epoch),
                &AccountReadPayload { opaque_account_key },
            )
            .map_err(|_| AccountProjectionError::Unavailable)?;
        validate_account_read_response_epoch(&response, account_epoch)?;
        validate_account_read_payload_size(
            &response.payload,
            AccountProjectionError::InvalidConnections,
        )?;
        let snapshot: ContextualConnectionsSnapshot = serde_json::from_slice(&response.payload)
            .map_err(|_| AccountProjectionError::InvalidConnections)?;
        snapshot.validate()?;
        Ok(snapshot)
    }

    /// Returns the active Mac app's runtime-only host-controls snapshot. A
    /// missing, expired, or closing app owner is deliberately indistinguish-
    /// able from an unavailable broker to avoid exposing app presence or a
    /// stale settings cache.
    pub fn host_controls_snapshot(
        &self,
        account_epoch: Option<&str>,
        session_id: Option<&str>,
    ) -> Result<serde_json::Value, BrokerError> {
        if let Some(session_id) = session_id {
            validate_host_control_session_id(session_id)?;
        }
        let response = self.call_without_descriptors(
            BrokerOperation::HostControlsSnapshot,
            account_epoch,
            &HostControlsSnapshotPayload { session_id },
        )?;
        decode_host_controls_result(&response.payload)
    }

    /// Applies exactly one ordinary overlay-equivalent control patch. The
    /// broker and active app both reject all other state, including Arm,
    /// Exam, invisibility, and Undetected controls.
    pub fn host_controls_apply(
        &self,
        account_epoch: Option<&str>,
        expected_revision: u64,
        patch: HostControlsPatch,
    ) -> Result<serde_json::Value, BrokerError> {
        let patch = HostControlsPatchPayload::try_from(patch)?;
        let response = self.call_without_descriptors(
            BrokerOperation::HostControlsApply,
            account_epoch,
            &HostControlsApplyPayload {
                expected_revision,
                patch,
            },
        )?;
        decode_host_controls_result(&response.payload)
    }

    /// Executes the only runtime host-control commands: a session-qualified
    /// Computer Use stop or a non-bypassable interactive-action result.
    pub fn host_controls_execute(
        &self,
        account_epoch: Option<&str>,
        command: HostControlsCommand,
    ) -> Result<serde_json::Value, BrokerError> {
        let command = HostControlsCommandPayload::try_from(command)?;
        let response = self.call_without_descriptors(
            BrokerOperation::HostControlsExecute,
            account_epoch,
            &HostControlsExecutePayload { command },
        )?;
        decode_host_controls_result(&response.payload)
    }

    fn request_descriptors(
        &self,
        operation: BrokerOperation,
        account_epoch: Option<&str>,
    ) -> Result<(BrokerResponseMetadata, [std::os::fd::OwnedFd; 2]), BrokerError> {
        debug_assert!(matches!(
            operation,
            BrokerOperation::AuthRefresh | BrokerOperation::RuntimeDescriptors
        ));
        let response = self.call(operation, account_epoch, &Empty {})?;
        validate_broker_descriptor_payload(&response.payload, &self.socket_path)?;
        let descriptor_fds: [std::os::fd::OwnedFd; 2] = response
            .fds
            .try_into()
            .map_err(|_| BrokerError::InvalidMessage)?;
        if response.metadata.descriptor_generation.is_none()
            || response.metadata.expires_at_ms.is_none()
            || response.metadata.account_epoch.is_none()
        {
            return Err(BrokerError::InvalidMessage);
        }
        Ok((response.metadata, descriptor_fds))
    }

    fn call_without_descriptors<T: Serialize>(
        &self,
        operation: BrokerOperation,
        account_epoch: Option<&str>,
        payload: &T,
    ) -> Result<BrokerCallResponse, BrokerError> {
        let response = self.call(operation, account_epoch, payload)?;
        if !response.fds.is_empty() {
            return Err(BrokerError::InvalidMessage);
        }
        Ok(response)
    }

    fn call<T: Serialize>(
        &self,
        operation: BrokerOperation,
        account_epoch: Option<&str>,
        payload: &T,
    ) -> Result<BrokerCallResponse, BrokerError> {
        if let Some(account_epoch) = account_epoch {
            validate_account_epoch(account_epoch).map_err(|_| BrokerError::InvalidMessage)?;
        }
        let payload_bytes = serde_json::to_vec(payload).map_err(|_| BrokerError::Serialize)?;
        let payload_base64_url = URL_SAFE_NO_PAD.encode(payload_bytes);
        let request_id = Uuid::new_v4().to_string();
        let nonce = URL_SAFE_NO_PAD.encode(random::<[u8; 24]>());
        let issued_at_ms = now_unix_ms().map_err(|_| BrokerError::InvalidMessage)?;
        let capability_proof_base64_url = self.proof(
            &request_id,
            operation,
            account_epoch,
            &nonce,
            issued_at_ms,
            &payload_base64_url,
        )?;
        let request = BrokerRequest {
            schema_version: BROKER_SCHEMA_VERSION,
            request_id: request_id.clone(),
            operation: operation.as_str(),
            account_epoch,
            nonce,
            issued_at_ms,
            payload_base64_url,
            capability_proof_base64_url,
        };
        let payload = serde_json::to_vec(&request).map_err(|_| BrokerError::Serialize)?;
        if payload.len() > MAX_BROKER_FRAME_BYTES {
            return Err(BrokerError::InvalidMessage);
        }
        let (response_bytes, fds) = broker_round_trip(
            &self.socket_path,
            &payload,
            operation.max_response_frame_bytes(),
        )?;
        let response: BrokerResponse =
            serde_json::from_slice(&response_bytes).map_err(|_| BrokerError::InvalidMessage)?;
        if response.schema_version != BROKER_SCHEMA_VERSION || response.request_id != request_id {
            return Err(BrokerError::ResponseMismatch);
        }
        let response_payload = URL_SAFE_NO_PAD
            .decode(&response.payload_base64_url)
            .map_err(|_| BrokerError::InvalidMessage)?;
        validate_response_envelope(&response, operation, &response_payload, &fds)?;
        let metadata = BrokerResponseMetadata {
            account_epoch: response.account_epoch,
            descriptor_generation: response.descriptor_generation,
            expires_at_ms: response.expires_at_ms,
        };
        match response.status.as_str() {
            "ok" => Ok(BrokerCallResponse {
                metadata,
                payload: response_payload,
                fds,
            }),
            "error" => {
                if !fds.is_empty() {
                    return Err(BrokerError::InvalidMessage);
                }
                Err(BrokerError::Rejected(
                    response.code.expect("validated error response code"),
                ))
            }
            _ => Err(BrokerError::InvalidMessage),
        }
    }

    fn proof(
        &self,
        request_id: &str,
        operation: BrokerOperation,
        account_epoch: Option<&str>,
        nonce: &str,
        issued_at_ms: i64,
        payload_base64_url: &str,
    ) -> Result<String, BrokerError> {
        let input = broker_capability_proof_input(
            request_id,
            operation,
            account_epoch,
            nonce,
            issued_at_ms,
            payload_base64_url,
        );
        let mut mac = HmacSha256::new_from_slice(&self.capability[..])
            .map_err(|_| BrokerError::InvalidCapabilityDescriptor)?;
        mac.update(input.as_bytes());
        Ok(URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes()))
    }
}

/// Canonical native-broker capability transcript. The Swift broker joins these
/// seven fields with actual newline bytes; changing separators invalidates the
/// proof before the broker can process a request.
fn broker_capability_proof_input(
    request_id: &str,
    operation: BrokerOperation,
    account_epoch: Option<&str>,
    nonce: &str,
    issued_at_ms: i64,
    payload_base64_url: &str,
) -> String {
    format!(
        "{BROKER_SCHEMA_VERSION}\n{request_id}\n{}\n{}\n{nonce}\n{issued_at_ms}\n{payload_base64_url}",
        operation.as_str(),
        account_epoch.unwrap_or_default(),
    )
}

/// Refresh-capable gateway descriptor source used by the long-lived app server
/// and CLI. Its launch pipes are parsed exactly once per process; each refresh
/// atomically replaces the shared in-memory descriptor generation after the
/// broker returns a new descriptor pair.
pub struct ManagedGatewayClient {
    broker: Arc<NativeBrokerClient>,
    state: Mutex<GatewayDescriptorSnapshot>,
    refresh_gate: Mutex<()>,
    #[cfg(feature = "test-support")]
    test_catalog_key_set: Option<CatalogVerificationKeySet>,
}

/// Typed test-only launch authority for an in-process managed gateway client.
///
/// This preserves the production descriptor protocol while keeping synthetic
/// launch material out of process environment and process-global caches.
#[cfg(feature = "test-support")]
pub struct ManagedGatewayTestLaunch {
    auth_descriptor: OwnedFd,
    endpoint_descriptor: OwnedFd,
    capability_descriptor: OwnedFd,
    socket_path: PathBuf,
    release_manifest_sha256: String,
}

#[cfg(feature = "test-support")]
impl ManagedGatewayTestLaunch {
    pub fn new(
        auth_descriptor: OwnedFd,
        endpoint_descriptor: OwnedFd,
        capability_descriptor: OwnedFd,
        socket_path: PathBuf,
        release_manifest_sha256: String,
    ) -> Self {
        Self {
            auth_descriptor,
            endpoint_descriptor,
            capability_descriptor,
            socket_path,
            release_manifest_sha256,
        }
    }
}

static PROCESS_MANAGED_GATEWAY_CLIENT: OnceLock<
    Result<Option<Arc<ManagedGatewayClient>>, ManagedGatewayError>,
> = OnceLock::new();

impl fmt::Debug for ManagedGatewayClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ManagedGatewayClient")
            .field("broker", &self.broker)
            .field("state", &"[REDACTED]")
            .finish()
    }
}

impl ManagedGatewayClient {
    /// Builds an isolated client from test-owned launch descriptors.
    ///
    /// This deliberately bypasses neither descriptor validation nor the
    /// native broker hello. It is feature-gated so production callers retain
    /// the process-scoped launcher authority path.
    #[cfg(all(feature = "test-support", unix))]
    pub fn from_test_launch(
        launch: ManagedGatewayTestLaunch,
    ) -> Result<Arc<Self>, ManagedGatewayError> {
        let auth = read_owned_descriptor(launch.auth_descriptor, MAX_GATEWAY_TOKEN_BYTES)?;
        let endpoint =
            read_owned_descriptor(launch.endpoint_descriptor, MAX_GATEWAY_ENDPOINT_BYTES)?;
        let test_catalog_key_set =
            crate::test_gateway::parse_inherited_test_gateway_descriptor(&endpoint)?
                .map(|descriptor| descriptor.catalog_key_set);
        let snapshot = GatewayDescriptorSnapshot::from_parts(
            &auth,
            &endpoint,
            BrokerResponseMetadata {
                account_epoch: None,
                descriptor_generation: None,
                expires_at_ms: None,
            },
        )?;
        let capability = read_exact_owned_capability(launch.capability_descriptor)?;
        validate_broker_socket_path(&launch.socket_path)?;
        let broker = Arc::new(NativeBrokerClient {
            socket_path: launch.socket_path,
            capability: Zeroizing::new(capability),
            release_manifest_sha256: launch.release_manifest_sha256,
        });
        broker.hello()?;
        Ok(Arc::new(Self {
            broker,
            state: Mutex::new(snapshot),
            refresh_gate: Mutex::new(()),
            test_catalog_key_set,
        }))
    }

    /// Builds a refresh-capable client from the initial launcher descriptor
    /// pair and the separate native-broker capability descriptor. The
    /// descriptors are pipes, so callers receive the process-shared source
    /// rather than attempting a second read.
    pub fn from_environment() -> Result<Option<Arc<Self>>, ManagedGatewayError> {
        PROCESS_MANAGED_GATEWAY_CLIENT
            .get_or_init(Self::initialize_from_environment)
            .clone()
    }

    fn initialize_from_environment() -> Result<Option<Arc<Self>>, ManagedGatewayError> {
        let auth_fd = inherited_fd(GATEWAY_AUTH_FD_ENV)
            .map_err(|_| ManagedGatewayError::InvalidDescriptor)?;
        let endpoint_fd = inherited_fd(GATEWAY_ENDPOINT_FD_ENV)
            .map_err(|_| ManagedGatewayError::InvalidDescriptor)?;
        let (Some(auth_fd), Some(endpoint_fd)) = (auth_fd, endpoint_fd) else {
            if auth_fd.is_none() && endpoint_fd.is_none() {
                return Ok(None);
            }
            return Err(ManagedGatewayError::IncompleteDescriptors);
        };
        let auth = read_inherited_descriptor(auth_fd, MAX_GATEWAY_TOKEN_BYTES)?;
        let endpoint = read_inherited_descriptor(endpoint_fd, MAX_GATEWAY_ENDPOINT_BYTES)?;
        #[cfg(feature = "test-support")]
        let test_catalog_key_set =
            crate::test_gateway::parse_inherited_test_gateway_descriptor(&endpoint)?
                .map(|descriptor| descriptor.catalog_key_set);
        let snapshot = GatewayDescriptorSnapshot::from_parts(
            &auth,
            &endpoint,
            BrokerResponseMetadata {
                account_epoch: None,
                descriptor_generation: None,
                expires_at_ms: None,
            },
        )?;
        let broker = NativeBrokerClient::from_environment()?
            .ok_or(ManagedGatewayError::IncompleteDescriptors)?;
        broker.hello()?;
        Ok(Some(Arc::new(Self {
            broker,
            state: Mutex::new(snapshot),
            refresh_gate: Mutex::new(()),
            #[cfg(feature = "test-support")]
            test_catalog_key_set,
        })))
    }

    pub fn availability_from_environment() -> ManagedGatewaySessionAvailability {
        match Self::from_environment() {
            Ok(Some(_)) => ManagedGatewaySessionAvailability::Available,
            Ok(None) => ManagedGatewaySessionAvailability::Missing,
            Err(_) => ManagedGatewaySessionAvailability::Invalid,
        }
    }

    /// Returns the descriptor-bound key set only for the exact inherited test
    /// transport. Shipping builds do not compile this accessor.
    #[cfg(feature = "test-support")]
    pub fn test_catalog_key_set(&self) -> Option<CatalogVerificationKeySet> {
        self.test_catalog_key_set.clone()
    }

    /// Returns the current descriptor generation without contacting the broker.
    pub fn snapshot(&self) -> Result<GatewayDescriptorSnapshot, ManagedGatewayError> {
        self.state
            .lock()
            .map_err(|_| ManagedGatewayError::InvalidDescriptor)
            .map(|state| state.clone())
    }

    /// Gets a descriptor which has at least one minute remaining. Initial
    /// launcher descriptors intentionally have no trusted expiry metadata, so
    /// they are immediately replaced through `runtime.descriptors`.
    pub fn ensure_fresh(&self) -> Result<GatewayDescriptorSnapshot, ManagedGatewayError> {
        let now = now_unix_ms()?;
        let snapshot = self.snapshot()?;
        if !descriptor_requires_refresh(&snapshot, now) {
            return Ok(snapshot);
        }
        self.replace_descriptors(BrokerOperation::RuntimeDescriptors)
    }

    /// Force-refreshes after a gateway expiry/unauthenticated response.
    pub fn refresh_now(&self) -> Result<GatewayDescriptorSnapshot, ManagedGatewayError> {
        self.replace_descriptors(BrokerOperation::AuthRefresh)
    }

    fn replace_descriptors(
        &self,
        operation: BrokerOperation,
    ) -> Result<GatewayDescriptorSnapshot, ManagedGatewayError> {
        let _refresh_guard = self
            .refresh_gate
            .lock()
            .map_err(|_| ManagedGatewayError::InvalidDescriptor)?;
        let current = self.snapshot()?;
        if operation == BrokerOperation::RuntimeDescriptors {
            let now = now_unix_ms()?;
            if !descriptor_requires_refresh(&current, now) {
                return Ok(current);
            }
        }
        let (metadata, [auth_fd, endpoint_fd]) = self
            .broker
            .request_descriptors(operation, current.account_epoch())?;
        let auth = read_owned_descriptor(auth_fd, MAX_GATEWAY_TOKEN_BYTES)?;
        let endpoint = read_owned_descriptor(endpoint_fd, MAX_GATEWAY_ENDPOINT_BYTES)?;
        let replacement = GatewayDescriptorSnapshot::from_parts(&auth, &endpoint, metadata)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| ManagedGatewayError::InvalidDescriptor)?;
        if !replacement_generation_changed(&replacement, &state) {
            return Err(ManagedGatewayError::InvalidDescriptor);
        }
        *state = replacement.clone();
        Ok(replacement)
    }
}

/// Initial descriptors have no trusted expiry metadata and therefore force a
/// broker handoff. Once metadata exists, the current generation remains usable
/// only while more than the refresh skew remains.
fn descriptor_requires_refresh(snapshot: &GatewayDescriptorSnapshot, now_ms: i64) -> bool {
    snapshot
        .expires_at_ms()
        .is_none_or(|expires_at_ms| expires_at_ms.saturating_sub(now_ms) <= REFRESH_SKEW_MS)
}

/// Native descriptor generations are opaque UUIDs rather than sortable
/// counters. A fresh descriptor must therefore carry a different, validated
/// generation; ordering is neither available nor inferred from UUID bytes.
fn replacement_generation_changed(
    replacement: &GatewayDescriptorSnapshot,
    current: &GatewayDescriptorSnapshot,
) -> bool {
    match (
        replacement.descriptor_generation(),
        current.descriptor_generation(),
    ) {
        (Some(replacement), Some(current)) => replacement != current,
        (Some(_), None) => true,
        _ => false,
    }
}

/// Convenience entry point for components that should not own launch parsing.
pub fn managed_gateway_client_from_environment()
-> Result<Option<Arc<ManagedGatewayClient>>, ManagedGatewayError> {
    ManagedGatewayClient::from_environment()
}

#[derive(Serialize)]
struct HelloPayload<'a> {
    #[serde(rename = "clientKind")]
    client_kind: &'static str,
    #[serde(rename = "releaseManifestSha256")]
    release_manifest_sha256: &'a str,
    #[serde(rename = "runtimeCapabilityBase64Url")]
    runtime_capability_base64_url: Option<&'static str>,
}

#[derive(Serialize)]
struct Empty {}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LoginBeginPayload {
    completion: &'static str,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LoginBeginResponsePayload {
    login_handle: String,
    method: String,
    authorization_url: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LoginCompletePayload<'a> {
    login_handle: &'a str,
    callback_url: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SwitchPayload<'a> {
    opaque_account_key: &'a str,
}

#[derive(Serialize)]
struct HostControlsSnapshotPayload<'a> {
    #[serde(rename = "sessionID", skip_serializing_if = "Option::is_none")]
    session_id: Option<&'a str>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HostControlsApplyPayload {
    expected_revision: u64,
    patch: HostControlsPatchPayload,
}

#[derive(Serialize)]
struct HostControlsPatchPayload {
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    route: Option<&'static str>,
    #[serde(rename = "builtinCapability", skip_serializing_if = "Option::is_none")]
    builtin_capability: Option<&'static str>,
    #[serde(rename = "sessionID", skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    directory: Option<HostControlsThreadDirectoryPayload>,
    #[serde(rename = "permissionMode", skip_serializing_if = "Option::is_none")]
    permission_mode: Option<&'static str>,
    #[serde(rename = "preferScreen", skip_serializing_if = "Option::is_none")]
    prefer_screen: Option<bool>,
    #[serde(rename = "audioAssistMode", skip_serializing_if = "Option::is_none")]
    audio_assist_mode: Option<&'static str>,
    #[serde(
        rename = "transcriptTaskSuggestionsEnabled",
        skip_serializing_if = "Option::is_none"
    )]
    transcript_task_suggestions_enabled: Option<bool>,
}

#[derive(Serialize)]
struct HostControlsThreadDirectoryPayload {
    kind: &'static str,
    #[serde(rename = "canonicalPath", skip_serializing_if = "Option::is_none")]
    canonical_path: Option<String>,
}

impl TryFrom<HostControlsPatch> for HostControlsPatchPayload {
    type Error = BrokerError;

    fn try_from(value: HostControlsPatch) -> Result<Self, Self::Error> {
        let empty = || Self {
            kind: "",
            enabled: None,
            route: None,
            builtin_capability: None,
            session_id: None,
            directory: None,
            permission_mode: None,
            prefer_screen: None,
            audio_assist_mode: None,
            transcript_task_suggestions_enabled: None,
        };
        match value {
            HostControlsPatch::ComputerUseMaster { enabled } => Ok(Self {
                kind: "computer_use_master",
                enabled: Some(enabled),
                ..empty()
            }),
            HostControlsPatch::ComputerUseRoute { route, enabled } => Ok(Self {
                kind: "computer_use_route",
                enabled: Some(enabled),
                route: Some(route.as_str()),
                ..empty()
            }),
            HostControlsPatch::BuiltinCapability {
                capability,
                enabled,
            } => Ok(Self {
                kind: "builtin_capability",
                enabled: Some(enabled),
                builtin_capability: Some(capability.as_str()),
                ..empty()
            }),
            HostControlsPatch::Thread {
                session_id,
                directory,
                permission_mode,
                prefer_screen,
            } => {
                validate_host_control_session_id(&session_id)?;
                if directory.is_none() && permission_mode.is_none() && prefer_screen.is_none() {
                    return Err(BrokerError::InvalidMessage);
                }
                let directory = directory.map(|selection| match selection {
                    HostControlsThreadDirectory::NoDirectory => {
                        HostControlsThreadDirectoryPayload {
                            kind: "noDirectory",
                            canonical_path: None,
                        }
                    }
                    HostControlsThreadDirectory::Directory { canonical_path } => {
                        HostControlsThreadDirectoryPayload {
                            kind: "directory",
                            canonical_path: Some(canonical_path),
                        }
                    }
                });
                Ok(Self {
                    kind: "thread",
                    session_id: Some(session_id),
                    directory,
                    permission_mode: permission_mode.map(HostControlsPermissionMode::as_str),
                    prefer_screen,
                    ..empty()
                })
            }
            HostControlsPatch::AudioAssist {
                mode,
                transcript_task_suggestions_enabled,
            } => {
                if mode.is_none() && transcript_task_suggestions_enabled.is_none() {
                    return Err(BrokerError::InvalidMessage);
                }
                Ok(Self {
                    kind: "audio_assist",
                    audio_assist_mode: mode.map(HostControlsAudioAssistMode::as_str),
                    transcript_task_suggestions_enabled,
                    ..empty()
                })
            }
        }
    }
}

#[derive(Serialize)]
struct HostControlsExecutePayload {
    command: HostControlsCommandPayload,
}

#[derive(Serialize)]
struct HostControlsCommandPayload {
    kind: &'static str,
    #[serde(rename = "sessionID", skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    #[serde(rename = "interactiveAction", skip_serializing_if = "Option::is_none")]
    interactive_action: Option<&'static str>,
}

impl TryFrom<HostControlsCommand> for HostControlsCommandPayload {
    type Error = BrokerError;

    fn try_from(value: HostControlsCommand) -> Result<Self, Self::Error> {
        match value {
            HostControlsCommand::StopComputerUse { session_id } => {
                validate_host_control_session_id(&session_id)?;
                Ok(Self {
                    kind: "stop_computer_use",
                    session_id: Some(session_id),
                    interactive_action: None,
                })
            }
            HostControlsCommand::RequestInteractive { action } => Ok(Self {
                kind: "request_interactive",
                session_id: None,
                interactive_action: Some(action.as_str()),
            }),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StatusPayload {
    authenticated: bool,
    opaque_account_key: Option<String>,
    account_epoch: Option<String>,
}

#[derive(Serialize)]
struct AccountReadPayload<'a> {
    #[serde(rename = "opaqueAccountKey")]
    opaque_account_key: &'a str,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BrokerDescriptorPayload {
    runtime_home: String,
    broker_socket_path: String,
}

#[derive(Serialize)]
struct BrokerRequest<'a> {
    #[serde(rename = "schemaVersion")]
    schema_version: u16,
    #[serde(rename = "requestID")]
    request_id: String,
    #[serde(rename = "operation")]
    operation: &'a str,
    #[serde(rename = "accountEpoch")]
    account_epoch: Option<&'a str>,
    #[serde(rename = "nonce")]
    nonce: String,
    #[serde(rename = "issuedAtMS")]
    issued_at_ms: i64,
    #[serde(rename = "payloadBase64URL")]
    payload_base64_url: String,
    #[serde(rename = "capabilityProofBase64URL")]
    capability_proof_base64_url: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BrokerResponse {
    #[serde(rename = "schemaVersion")]
    schema_version: u16,
    #[serde(rename = "requestID")]
    request_id: String,
    #[serde(rename = "status")]
    status: String,
    #[serde(rename = "accountEpoch")]
    account_epoch: Option<String>,
    #[serde(rename = "descriptorGeneration")]
    descriptor_generation: Option<String>,
    #[serde(rename = "expiresAtMS")]
    expires_at_ms: Option<i64>,
    #[serde(rename = "payloadBase64URL")]
    payload_base64_url: String,
    #[serde(rename = "code")]
    code: Option<BrokerErrorCode>,
}

struct BrokerCallResponse {
    metadata: BrokerResponseMetadata,
    payload: Vec<u8>,
    fds: Vec<std::os::fd::OwnedFd>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GatewayEndpointDocument {
    schema_version: u16,
    gateway_base_url: String,
}

fn deserialize_payload<T: DeserializeOwned>(payload: &[u8]) -> Result<T, BrokerError> {
    serde_json::from_slice(payload).map_err(|_| BrokerError::InvalidMessage)
}

fn validate_host_control_session_id(value: &str) -> Result<(), BrokerError> {
    if value.trim().is_empty()
        || value.len() > 512
        || value.chars().any(|character| character.is_control())
    {
        return Err(BrokerError::InvalidMessage);
    }
    Ok(())
}

fn decode_host_controls_result(payload: &[u8]) -> Result<serde_json::Value, BrokerError> {
    if payload.len() > 32 * 1024 {
        return Err(BrokerError::InvalidMessage);
    }
    let value: serde_json::Value = deserialize_payload(payload)?;
    if !value.is_object() {
        return Err(BrokerError::InvalidMessage);
    }
    Ok(value)
}

fn account_read_binding(
    binding: &BrokerLoginStatus,
) -> Result<(&str, &str), AccountProjectionError> {
    match (
        binding.authenticated,
        binding.opaque_account_key.as_deref(),
        binding.account_epoch.as_deref(),
    ) {
        (true, Some(opaque_account_key), Some(account_epoch))
            if valid_opaque_account_key(opaque_account_key)
                && validate_account_epoch(account_epoch).is_ok() =>
        {
            Ok((opaque_account_key, account_epoch))
        }
        _ => Err(AccountProjectionError::Unavailable),
    }
}

fn validate_account_read_response_epoch(
    response: &BrokerCallResponse,
    expected_account_epoch: &str,
) -> Result<(), AccountProjectionError> {
    if response.metadata.account_epoch.as_deref() == Some(expected_account_epoch) {
        Ok(())
    } else {
        Err(AccountProjectionError::Unavailable)
    }
}

fn validate_account_read_payload_size(
    payload: &[u8],
    oversized_error: AccountProjectionError,
) -> Result<(), AccountProjectionError> {
    if payload.len() <= MAX_ACCOUNT_READ_PAYLOAD_BYTES {
        Ok(())
    } else {
        Err(oversized_error)
    }
}

fn validate_response_envelope(
    response: &BrokerResponse,
    operation: BrokerOperation,
    payload: &[u8],
    fds: &[std::os::fd::OwnedFd],
) -> Result<(), BrokerError> {
    let descriptor_operation = matches!(
        operation,
        BrokerOperation::AuthRefresh | BrokerOperation::RuntimeDescriptors
    );
    let account_read_operation = matches!(
        operation,
        BrokerOperation::AccountUsageRead | BrokerOperation::AccountConnectionsRead
    );
    match response.status.as_str() {
        "error" => {
            if response.code.is_none()
                || !payload.is_empty()
                || !fds.is_empty()
                || response.account_epoch.is_some()
                || response.descriptor_generation.is_some()
                || response.expires_at_ms.is_some()
            {
                return Err(BrokerError::InvalidMessage);
            }
        }
        "ok" => {
            if response.code.is_some() {
                return Err(BrokerError::InvalidMessage);
            }
            if descriptor_operation {
                if fds.len() != 2
                    || !matches!(
                        response.account_epoch.as_deref(),
                        Some(value) if validate_account_epoch(value).is_ok()
                    )
                    || response.descriptor_generation.is_none()
                    || response.expires_at_ms.is_none()
                {
                    return Err(BrokerError::InvalidMessage);
                }
            } else if account_read_operation {
                if !fds.is_empty()
                    || !matches!(
                        response.account_epoch.as_deref(),
                        Some(value) if validate_account_epoch(value).is_ok()
                    )
                    || response.descriptor_generation.is_some()
                    || response.expires_at_ms.is_some()
                {
                    return Err(BrokerError::InvalidMessage);
                }
            } else if !fds.is_empty()
                || response.account_epoch.is_some()
                || response.descriptor_generation.is_some()
                || response.expires_at_ms.is_some()
            {
                return Err(BrokerError::InvalidMessage);
            }
        }
        _ => return Err(BrokerError::InvalidMessage),
    }
    Ok(())
}

fn validate_broker_descriptor_payload(
    payload: &[u8],
    expected_socket_path: &Path,
) -> Result<(), BrokerError> {
    let descriptor: BrokerDescriptorPayload = deserialize_payload(payload)?;
    let expected_socket_path = expected_socket_path
        .to_str()
        .ok_or(BrokerError::InvalidMessage)?;
    let runtime_home = Path::new(&descriptor.runtime_home);
    if descriptor.runtime_home.len() > 4 * 1024
        || descriptor
            .runtime_home
            .contains(|character: char| character.is_control())
        || !runtime_home.is_absolute()
        || descriptor.broker_socket_path != expected_socket_path
    {
        return Err(BrokerError::InvalidMessage);
    }
    Ok(())
}

fn parse_bearer_descriptor(bytes: &[u8]) -> Result<String, ManagedGatewayError> {
    let token = std::str::from_utf8(bytes).map_err(|_| ManagedGatewayError::InvalidDescriptor)?;
    let valid = (8..=usize::try_from(MAX_GATEWAY_TOKEN_BYTES).unwrap_or(usize::MAX))
        .contains(&token.len())
        && token.bytes().all(|byte| byte.is_ascii_graphic());
    if !valid {
        return Err(ManagedGatewayError::InvalidDescriptor);
    }
    Ok(token.to_string())
}

fn parse_gateway_endpoint_descriptor(bytes: &[u8]) -> Result<String, ManagedGatewayError> {
    #[cfg(feature = "test-support")]
    if let Some(test_descriptor) =
        crate::test_gateway::parse_inherited_test_gateway_descriptor(bytes)?
    {
        return Ok(test_descriptor.api_base_url);
    }

    parse_release_gateway_endpoint_descriptor(bytes)
}

fn parse_release_gateway_endpoint_descriptor(bytes: &[u8]) -> Result<String, ManagedGatewayError> {
    let document: GatewayEndpointDocument =
        serde_json::from_slice(bytes).map_err(|_| ManagedGatewayError::InvalidDescriptor)?;
    if document.schema_version != GATEWAY_ENDPOINT_SCHEMA_VERSION {
        return Err(ManagedGatewayError::InvalidDescriptor);
    }
    let endpoint = Url::parse(&document.gateway_base_url)
        .map_err(|_| ManagedGatewayError::InvalidDescriptor)?;
    if endpoint.scheme() != "https"
        || endpoint.host_str() != Some(WHISPLY_GATEWAY_ORIGIN_HOST)
        || !endpoint.username().is_empty()
        || endpoint.password().is_some()
        || endpoint.query().is_some()
        || endpoint.fragment().is_some()
        || endpoint.path() != "/"
        || endpoint.port().is_some()
    {
        return Err(ManagedGatewayError::InvalidDescriptor);
    }
    Ok(format!(
        "{}/v1",
        document.gateway_base_url.trim_end_matches('/')
    ))
}

fn validate_status_payload(payload: &StatusPayload) -> Result<(), BrokerError> {
    match (
        payload.authenticated,
        payload.opaque_account_key.as_deref(),
        payload.account_epoch.as_deref(),
    ) {
        (true, Some(account_key), Some(account_epoch))
            if valid_opaque_account_key(account_key)
                && validate_account_epoch(account_epoch).is_ok() =>
        {
            Ok(())
        }
        (false, None, None) => Ok(()),
        _ => Err(BrokerError::InvalidMessage),
    }
}

fn valid_opaque_account_key(value: &str) -> bool {
    value.len() == 53
        && value.starts_with("acct_")
        && value[5..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (byte.is_ascii_lowercase() && byte <= b'f'))
}

fn validate_account_epoch(value: &str) -> Result<(), ManagedGatewayError> {
    let parsed = Uuid::parse_str(value).map_err(|_| ManagedGatewayError::InvalidDescriptor)?;
    if parsed.hyphenated().to_string() == value {
        Ok(())
    } else {
        Err(ManagedGatewayError::InvalidDescriptor)
    }
}

fn validate_descriptor_generation(value: &str) -> Result<(), ManagedGatewayError> {
    let parsed = Uuid::parse_str(value).map_err(|_| ManagedGatewayError::InvalidDescriptor)?;
    if parsed.to_string() != value {
        return Err(ManagedGatewayError::InvalidDescriptor);
    }
    Ok(())
}

fn is_opaque_uuid(value: &str) -> bool {
    Uuid::parse_str(value).is_ok()
}

fn valid_browser_url(value: &str) -> bool {
    Url::parse(value).is_ok_and(|url| {
        url.scheme() == "https"
            && !url.host_str().unwrap_or_default().is_empty()
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none()
    })
}

fn valid_callback_url(value: &str) -> bool {
    value.len() <= 4_096
        && Url::parse(value).is_ok_and(|url| {
            url.scheme() == "whisply"
                && url.host_str() == Some("auth-callback")
                && url.fragment().is_none()
        })
}

fn now_unix_ms() -> Result<i64, ManagedGatewayError> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| ManagedGatewayError::InvalidDescriptor)?
        .as_millis();
    i64::try_from(millis).map_err(|_| ManagedGatewayError::InvalidDescriptor)
}

fn inherited_fd(name: &str) -> Result<Option<i32>, ()> {
    let Some(raw_fd) = std::env::var_os(name) else {
        return Ok(None);
    };
    let raw_fd = raw_fd.to_string_lossy();
    let fd = raw_fd.parse::<i32>().map_err(|_| ())?;
    if fd < 3 { Err(()) } else { Ok(Some(fd)) }
}

#[cfg(unix)]
fn read_inherited_descriptor(fd: i32, max_bytes: u64) -> Result<Vec<u8>, ManagedGatewayError> {
    use std::fs::File;
    use std::os::fd::FromRawFd;

    let file = unsafe { File::from_raw_fd(fd) };
    read_descriptor_file(file, max_bytes)
}

#[cfg(not(unix))]
fn read_inherited_descriptor(_: i32, _: u64) -> Result<Vec<u8>, ManagedGatewayError> {
    Err(ManagedGatewayError::InvalidDescriptor)
}

#[cfg(unix)]
fn read_owned_descriptor(
    fd: std::os::fd::OwnedFd,
    max_bytes: u64,
) -> Result<Vec<u8>, ManagedGatewayError> {
    use std::fs::File;

    let file = File::from(fd);
    read_descriptor_file(file, max_bytes)
}

#[cfg(not(unix))]
fn read_owned_descriptor(_: (), _: u64) -> Result<Vec<u8>, ManagedGatewayError> {
    Err(ManagedGatewayError::InvalidDescriptor)
}

fn read_descriptor_file(
    file: std::fs::File,
    max_bytes: u64,
) -> Result<Vec<u8>, ManagedGatewayError> {
    let mut bytes = Vec::new();
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| ManagedGatewayError::DescriptorIo)?;
    if bytes.len() > usize::try_from(max_bytes).unwrap_or(usize::MAX) {
        return Err(ManagedGatewayError::InvalidDescriptor);
    }
    Ok(bytes)
}

#[cfg(unix)]
fn read_exact_inherited_capability(fd: i32) -> Result<[u8; 32], BrokerError> {
    use std::fs::File;
    use std::os::fd::FromRawFd;

    let mut file = unsafe { File::from_raw_fd(fd) };
    let mut capability = [0_u8; 32];
    file.read_exact(&mut capability)
        .map_err(|_| BrokerError::InvalidCapabilityDescriptor)?;
    let mut trailing = [0_u8; 1];
    if file
        .read(&mut trailing)
        .map_err(|_| BrokerError::InvalidCapabilityDescriptor)?
        != 0
    {
        return Err(BrokerError::InvalidCapabilityDescriptor);
    }
    Ok(capability)
}

#[cfg(all(feature = "test-support", unix))]
fn read_exact_owned_capability(fd: OwnedFd) -> Result<[u8; 32], BrokerError> {
    use std::fs::File;

    let mut file = File::from(fd);
    let mut capability = [0_u8; 32];
    file.read_exact(&mut capability)
        .map_err(|_| BrokerError::InvalidCapabilityDescriptor)?;
    let mut trailing = [0_u8; 1];
    if file
        .read(&mut trailing)
        .map_err(|_| BrokerError::InvalidCapabilityDescriptor)?
        != 0
    {
        return Err(BrokerError::InvalidCapabilityDescriptor);
    }
    Ok(capability)
}

#[cfg(not(unix))]
fn read_exact_inherited_capability(_: i32) -> Result<[u8; 32], BrokerError> {
    Err(BrokerError::UnsupportedPlatform)
}

#[cfg(unix)]
fn validate_broker_socket_path(path: &Path) -> Result<(), BrokerError> {
    use std::fs;
    use std::os::unix::fs::FileTypeExt;
    use std::os::unix::fs::MetadataExt;

    if !path.is_absolute()
        || path.file_name().and_then(|name| name.to_str()) != Some(NATIVE_BROKER_SOCKET_FILENAME)
        || path
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            != Some(NATIVE_BROKER_DIRECTORY)
    {
        return Err(BrokerError::UnsafeSocket);
    }
    let parent = path.parent().ok_or(BrokerError::UnsafeSocket)?;
    let parent_metadata = fs::symlink_metadata(parent).map_err(|_| BrokerError::UnsafeSocket)?;
    let socket_metadata = fs::symlink_metadata(path).map_err(|_| BrokerError::UnsafeSocket)?;
    let uid = unsafe { libc::geteuid() };
    let private = |metadata: &fs::Metadata| {
        !metadata.file_type().is_symlink() && metadata.uid() == uid && metadata.mode() & 0o077 == 0
    };
    if !private(&parent_metadata)
        || !private(&socket_metadata)
        || !socket_metadata.file_type().is_socket()
    {
        return Err(BrokerError::UnsafeSocket);
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_broker_socket_path(_: &Path) -> Result<(), BrokerError> {
    Err(BrokerError::UnsupportedPlatform)
}

#[cfg(unix)]
fn broker_round_trip(
    path: &Path,
    payload: &[u8],
    max_response_frame_bytes: usize,
) -> Result<(Vec<u8>, Vec<std::os::fd::OwnedFd>), BrokerError> {
    use std::io::Read;
    use std::io::Write;
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;

    let mut stream = UnixStream::connect(path).map_err(|_| BrokerError::Io)?;
    validate_broker_peer(&stream)?;
    let frame_len = u32::try_from(payload.len()).map_err(|_| BrokerError::InvalidMessage)?;
    stream
        .write_all(&frame_len.to_be_bytes())
        .and_then(|_| stream.write_all(payload))
        .map_err(|_| BrokerError::Io)?;

    let mut header = [0_u8; 4];
    let mut control = vec![0_u8; control_space_for_fds(2)];
    let mut iov = libc::iovec {
        iov_base: header.as_mut_ptr().cast(),
        iov_len: header.len(),
    };
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut iov;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen =
        u32::try_from(control.len()).map_err(|_| BrokerError::InvalidMessage)?;
    let first_read = unsafe { libc::recvmsg(stream.as_raw_fd(), &mut message, 0) };
    // `recvmsg` installs SCM_RIGHTS descriptors before reporting whether the
    // frame is otherwise acceptable. Take ownership and set close-on-exec
    // before any validation branch so malformed/truncated responses cannot
    // leak a live gateway descriptor into a later child process.
    let fds = if first_read >= 0 {
        extract_rights_fds(&message)?
    } else {
        Vec::new()
    };
    if first_read <= 0 || first_read > i64::try_from(header.len()).unwrap_or(i64::MAX) as isize {
        return Err(BrokerError::Io);
    }
    if message.msg_flags & libc::MSG_CTRUNC != 0 {
        return Err(BrokerError::InvalidMessage);
    }
    let first_read = usize::try_from(first_read).map_err(|_| BrokerError::Io)?;
    if first_read < header.len() {
        stream
            .read_exact(&mut header[first_read..])
            .map_err(|_| BrokerError::Io)?;
    }
    let response_len =
        usize::try_from(u32::from_be_bytes(header)).map_err(|_| BrokerError::InvalidMessage)?;
    if response_len > max_response_frame_bytes {
        return Err(BrokerError::InvalidMessage);
    }
    let mut response = vec![0_u8; response_len];
    stream
        .read_exact(&mut response)
        .map_err(|_| BrokerError::Io)?;
    Ok((response, fds))
}

#[cfg(not(unix))]
fn broker_round_trip(_: &Path, _: &[u8], _: usize) -> Result<(Vec<u8>, Vec<()>), BrokerError> {
    Err(BrokerError::UnsupportedPlatform)
}

#[cfg(unix)]
fn control_space_for_fds(count: usize) -> usize {
    unsafe { libc::CMSG_SPACE((count * size_of::<i32>()) as _) as usize }
}

#[cfg(unix)]
fn extract_rights_fds(message: &libc::msghdr) -> Result<Vec<std::os::fd::OwnedFd>, BrokerError> {
    use std::os::fd::AsRawFd;
    use std::os::fd::FromRawFd;
    use std::os::fd::OwnedFd;
    use std::os::fd::RawFd;

    let mut descriptors = Vec::new();
    let mut invalid_message = false;
    let mut close_on_exec_failed = false;
    let control_start = message.msg_control as usize;
    let control_len =
        usize::try_from(message.msg_controllen).map_err(|_| BrokerError::InvalidMessage)?;
    let control_end = control_start
        .checked_add(control_len)
        .ok_or(BrokerError::InvalidMessage)?;
    if control_len == 0 {
        return Ok(descriptors);
    }
    if control_start == 0 {
        return Err(BrokerError::InvalidMessage);
    }

    let mut current = unsafe { libc::CMSG_FIRSTHDR(message) };
    if current.is_null() {
        return Err(BrokerError::InvalidMessage);
    }
    while !current.is_null() {
        let current_address = current as usize;
        let header_len = unsafe { libc::CMSG_LEN(0) as usize };
        if current_address < control_start
            || current_address
                .checked_add(header_len)
                .is_none_or(|header_end| header_end > control_end)
        {
            return Err(BrokerError::InvalidMessage);
        }
        let is_rights = unsafe {
            (*current).cmsg_level == libc::SOL_SOCKET && (*current).cmsg_type == libc::SCM_RIGHTS
        };
        let message_len = usize::try_from(unsafe { (*current).cmsg_len })
            .map_err(|_| BrokerError::InvalidMessage)?;
        let available_data_len = control_end - (current_address + header_len);
        let declared_data_len = message_len.saturating_sub(header_len);
        let readable_data_len = declared_data_len.min(available_data_len);

        if is_rights {
            let count = readable_data_len / size_of::<RawFd>();
            let data = unsafe { libc::CMSG_DATA(current).cast::<RawFd>() };
            for index in 0..count {
                // SCM_RIGHTS creates new descriptor table entries with
                // FD_CLOEXEC clear. Own each one before any semantic count
                // validation, then mark it close-on-exec immediately so every
                // error path drops a safely-contained descriptor.
                let descriptor = unsafe { OwnedFd::from_raw_fd(data.add(index).read()) };
                close_on_exec_failed |=
                    mark_descriptor_close_on_exec(descriptor.as_raw_fd()).is_err();
                descriptors.push(descriptor);
            }
        }

        let structurally_valid = message_len >= header_len
            && current_address
                .checked_add(message_len)
                .is_some_and(|message_end| message_end <= control_end);
        if !structurally_valid {
            invalid_message = true;
            break;
        }
        if !is_rights || declared_data_len % size_of::<RawFd>() != 0 {
            invalid_message = true;
        }

        let next = unsafe { libc::CMSG_NXTHDR(message, current) };
        if !next.is_null() && next as usize <= current_address {
            invalid_message = true;
            break;
        }
        current = next;
    }
    if close_on_exec_failed {
        return Err(BrokerError::Io);
    }
    if invalid_message || descriptors.len() > 2 {
        return Err(BrokerError::InvalidMessage);
    }
    Ok(descriptors)
}

#[cfg(unix)]
fn mark_descriptor_close_on_exec(fd: std::os::fd::RawFd) -> Result<(), BrokerError> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(BrokerError::Io);
    }
    if flags & libc::FD_CLOEXEC == 0 {
        let result = unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) };
        if result < 0 {
            return Err(BrokerError::Io);
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn validate_broker_peer(stream: &std::os::unix::net::UnixStream) -> Result<(), BrokerError> {
    use std::os::fd::AsRawFd;

    let mut peer_uid = 0;
    let mut peer_gid = 0;
    let result = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut peer_uid, &mut peer_gid) };
    if result != 0 || peer_uid != unsafe { libc::geteuid() } {
        return Err(BrokerError::InvalidPeer);
    }
    Ok(())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn validate_broker_peer(_: &std::os::unix::net::UnixStream) -> Result<(), BrokerError> {
    // Production Whisply distributions are macOS-only. Other Unix development
    // targets retain path/ownership checks but cannot silently claim a Darwin
    // LOCAL_PEERPID validation they do not implement.
    Err(BrokerError::InvalidPeer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[cfg(unix)]
    unsafe fn rights_message_for_test(
        control: &mut [u8],
        descriptors: &[std::os::fd::RawFd],
    ) -> libc::msghdr {
        let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen =
            u32::try_from(control.len()).expect("test control buffer fits in msg_controllen");
        let current = unsafe { libc::CMSG_FIRSTHDR(&message) };
        assert!(!current.is_null(), "test control buffer has a cmsg header");
        unsafe {
            (*current).cmsg_level = libc::SOL_SOCKET;
            (*current).cmsg_type = libc::SCM_RIGHTS;
            (*current).cmsg_len =
                libc::CMSG_LEN((descriptors.len() * size_of::<std::os::fd::RawFd>()) as _) as _;
            std::ptr::copy_nonoverlapping(
                descriptors.as_ptr().cast::<u8>(),
                libc::CMSG_DATA(current).cast(),
                std::mem::size_of_val(descriptors),
            );
        }
        message
    }

    #[cfg(unix)]
    unsafe fn mixed_non_rights_then_rights_message_for_test(
        control: &mut [u8],
        descriptor: std::os::fd::RawFd,
    ) -> libc::msghdr {
        let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen =
            u32::try_from(control.len()).expect("test control buffer fits in msg_controllen");
        let first = unsafe { libc::CMSG_FIRSTHDR(&message) };
        assert!(
            !first.is_null(),
            "test control buffer has a first cmsg header"
        );
        unsafe {
            (*first).cmsg_level = libc::SOL_SOCKET;
            (*first).cmsg_type = libc::SCM_RIGHTS + 1;
            (*first).cmsg_len = libc::CMSG_LEN(0) as _;
        }
        let second = unsafe { libc::CMSG_NXTHDR(&message, first) };
        assert!(
            !second.is_null(),
            "test control buffer has a second cmsg header"
        );
        unsafe {
            (*second).cmsg_level = libc::SOL_SOCKET;
            (*second).cmsg_type = libc::SCM_RIGHTS;
            (*second).cmsg_len = libc::CMSG_LEN(size_of::<std::os::fd::RawFd>() as _) as _;
            libc::CMSG_DATA(second)
                .cast::<std::os::fd::RawFd>()
                .write(descriptor);
        }
        message
    }

    #[cfg(unix)]
    #[test]
    fn received_scm_rights_descriptors_are_marked_close_on_exec() {
        use std::os::fd::AsRawFd;
        use std::os::fd::FromRawFd;
        use std::os::fd::OwnedFd;

        let mut pipe_fds = [-1; 2];
        assert_eq!(unsafe { libc::pipe(pipe_fds.as_mut_ptr()) }, 0);
        let _write_end = unsafe { OwnedFd::from_raw_fd(pipe_fds[1]) };
        let mut control = vec![0_u8; control_space_for_fds(1)];
        let message = unsafe { rights_message_for_test(&mut control, &[pipe_fds[0]]) };

        let descriptors = extract_rights_fds(&message).expect("accept one SCM_RIGHTS descriptor");
        assert_eq!(descriptors.len(), 1);
        let flags = unsafe { libc::fcntl(descriptors[0].as_raw_fd(), libc::F_GETFD) };
        assert_ne!(flags, -1);
        assert_ne!(flags & libc::FD_CLOEXEC, 0);
    }

    #[cfg(unix)]
    #[test]
    fn rejected_extra_scm_rights_descriptors_are_closed() {
        use std::os::fd::FromRawFd;
        use std::os::fd::OwnedFd;

        let mut raw_read_ends = Vec::new();
        let mut write_ends = Vec::new();
        for _ in 0..3 {
            let mut pipe_fds = [-1; 2];
            assert_eq!(unsafe { libc::pipe(pipe_fds.as_mut_ptr()) }, 0);
            raw_read_ends.push(pipe_fds[0]);
            write_ends.push(unsafe { OwnedFd::from_raw_fd(pipe_fds[1]) });
        }
        let mut control = vec![0_u8; control_space_for_fds(raw_read_ends.len())];
        let message = unsafe { rights_message_for_test(&mut control, &raw_read_ends) };

        assert!(matches!(
            extract_rights_fds(&message),
            Err(BrokerError::InvalidMessage)
        ));
        for descriptor in raw_read_ends {
            assert_eq!(unsafe { libc::fcntl(descriptor, libc::F_GETFD) }, -1);
        }
        drop(write_ends);
    }

    #[cfg(unix)]
    #[test]
    fn malformed_non_rights_cmsg_still_closes_later_scm_rights_descriptor() {
        use std::os::fd::FromRawFd;
        use std::os::fd::OwnedFd;

        let mut pipe_fds = [-1; 2];
        assert_eq!(unsafe { libc::pipe(pipe_fds.as_mut_ptr()) }, 0);
        let _write_end = unsafe { OwnedFd::from_raw_fd(pipe_fds[1]) };
        let mut control = vec![0_u8; control_space_for_fds(0) + control_space_for_fds(1)];
        let message =
            unsafe { mixed_non_rights_then_rights_message_for_test(&mut control, pipe_fds[0]) };

        assert!(matches!(
            extract_rights_fds(&message),
            Err(BrokerError::InvalidMessage)
        ));
        assert_eq!(unsafe { libc::fcntl(pipe_fds[0], libc::F_GETFD) }, -1);
    }

    #[cfg(unix)]
    #[test]
    fn short_nonempty_control_buffer_is_rejected() {
        let mut control = [0_u8; 1];
        let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen = 1;

        assert!(matches!(
            extract_rights_fds(&message),
            Err(BrokerError::InvalidMessage)
        ));
    }

    #[test]
    fn endpoint_descriptor_accepts_only_the_fixed_release_origin() {
        let endpoint = parse_gateway_endpoint_descriptor(
            br#"{"schemaVersion":1,"gatewayBaseUrl":"https://proxy.whisply.net"}"#,
        )
        .expect("release endpoint");
        assert_eq!(endpoint, "https://proxy.whisply.net/v1");

        for descriptor in [
            br#"{"schemaVersion":1,"gatewayBaseUrl":"http://proxy.whisply.net"}"#.as_slice(),
            br#"{"schemaVersion":1,"gatewayBaseUrl":"https://proxy.whisply.net/v1"}"#.as_slice(),
            br#"{"schemaVersion":1,"gatewayBaseUrl":"https://proxy.whisply.net?redirect=1"}"#
                .as_slice(),
            br#"{"schemaVersion":1,"gatewayBaseUrl":"https://gateway.example.test"}"#.as_slice(),
        ] {
            assert!(parse_gateway_endpoint_descriptor(descriptor).is_err());
        }
    }

    #[test]
    fn bearer_descriptor_never_normalizes_newline_or_whitespace() {
        assert!(parse_bearer_descriptor(b"brokered-token").is_ok());
        assert!(parse_bearer_descriptor(b"brokered-token\n").is_err());
        assert!(parse_bearer_descriptor(b" brokered-token").is_err());
    }

    #[test]
    fn capability_proof_matches_the_native_newline_transcript_vector() {
        // Mirrors WhisplyGatewayBrokerWireRequest.proofInput in the native
        // service: its seven fields are joined with `"\n"`, not the two bytes
        // `\\` and `n`.
        let capability = [
            0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23,
            24, 25, 26, 27, 28, 29, 30, 31,
        ];
        let transcript = broker_capability_proof_input(
            "request-0001",
            BrokerOperation::Hello,
            None,
            "nonce-0001",
            1_700_000_000_123,
            "e30",
        );
        assert_eq!(
            transcript.as_bytes(),
            b"1\nrequest-0001\nbroker.hello\n\nnonce-0001\n1700000000123\ne30"
        );

        let client = NativeBrokerClient {
            socket_path: PathBuf::from("/private/tmp/test/.native-broker/v1.sock"),
            capability: Zeroizing::new(capability),
            release_manifest_sha256: "0".repeat(64),
        };
        assert_eq!(
            client
                .proof(
                    "request-0001",
                    BrokerOperation::Hello,
                    None,
                    "nonce-0001",
                    1_700_000_000_123,
                    "e30",
                )
                .expect("fixed proof vector should be valid"),
            "jjdtjXedTZDIoRA3mnPD9BBWe7K9xDLr1zF_qXvBuSI"
        );
    }

    #[test]
    fn native_broker_envelope_uses_canonical_acronym_casing_and_uuid_generation() {
        let release_manifest_sha256 = "0".repeat(64);
        let hello = HelloPayload {
            client_kind: "runtime",
            release_manifest_sha256: &release_manifest_sha256,
            runtime_capability_base64_url: None,
        };
        assert_eq!(
            serde_json::to_value(&hello).expect("serialize canonical runtime hello"),
            serde_json::json!({
                "clientKind": "runtime",
                "releaseManifestSha256": "0".repeat(64),
                "runtimeCapabilityBase64Url": null,
            })
        );

        let request = BrokerRequest {
            schema_version: 1,
            request_id: "request-0001".to_string(),
            operation: "broker.hello",
            account_epoch: None,
            nonce: "nonce-0001".to_string(),
            issued_at_ms: 1_700_000_000_123,
            payload_base64_url: "e30".to_string(),
            capability_proof_base64_url: "proof".to_string(),
        };
        assert_eq!(
            serde_json::to_value(&request).expect("serialize native broker request"),
            serde_json::json!({
                "schemaVersion": 1,
                "requestID": "request-0001",
                "operation": "broker.hello",
                "accountEpoch": null,
                "nonce": "nonce-0001",
                "issuedAtMS": 1_700_000_000_123_i64,
                "payloadBase64URL": "e30",
                "capabilityProofBase64URL": "proof",
            })
        );

        let response: BrokerResponse = serde_json::from_value(serde_json::json!({
            "schemaVersion": 1,
            "requestID": "request-0001",
            "status": "ok",
            "accountEpoch": "5ce47c6b-387a-4f92-9dd9-0bdb1082bc07",
            "descriptorGeneration": "63ff45f8-1a7d-49c0-a892-9df2ca3e0b59",
            "expiresAtMS": 4_102_444_800_000_i64,
            "payloadBase64URL": "e30",
        }))
        .expect("decode native broker response shape");
        assert_eq!(response.request_id, "request-0001");
        assert_eq!(
            response.descriptor_generation.as_deref(),
            Some("63ff45f8-1a7d-49c0-a892-9df2ca3e0b59")
        );
        assert_eq!(response.expires_at_ms, Some(4_102_444_800_000));

        let snapshot = GatewayDescriptorSnapshot::from_parts(
            b"brokered-token",
            br#"{"schemaVersion":1,"gatewayBaseUrl":"https://proxy.whisply.net"}"#,
            BrokerResponseMetadata {
                account_epoch: response.account_epoch,
                descriptor_generation: response.descriptor_generation,
                expires_at_ms: response.expires_at_ms,
            },
        )
        .expect("native UUID descriptor metadata should be accepted");
        assert_eq!(
            snapshot.descriptor_generation(),
            Some("63ff45f8-1a7d-49c0-a892-9df2ca3e0b59")
        );

        assert!(serde_json::from_slice::<BrokerResponse>(
            br#"{"schemaVersion":1,"requestID":"request-0001","status":"ok","descriptorGeneration":1,"payloadBase64URL":"e30"}"#
        )
        .is_err());
    }

    #[test]
    fn account_status_never_accepts_raw_account_identity() {
        let valid = StatusPayload {
            authenticated: true,
            opaque_account_key: Some(format!("acct_{}", "a".repeat(48))),
            account_epoch: Some("5ce47c6b-387a-4f92-9dd9-0bdb1082bc07".to_string()),
        };
        assert!(validate_status_payload(&valid).is_ok());

        let invalid = StatusPayload {
            authenticated: true,
            opaque_account_key: Some("person@example.com".to_string()),
            account_epoch: valid.account_epoch,
        };
        assert!(validate_status_payload(&invalid).is_err());
    }

    #[test]
    fn runtime_account_read_uses_exact_opaque_status_binding() {
        let binding = BrokerLoginStatus {
            authenticated: true,
            opaque_account_key: Some(format!("acct_{}", "a".repeat(48))),
            account_epoch: Some("5ce47c6b-387a-4f92-9dd9-0bdb1082bc07".to_string()),
        };
        let (opaque_account_key, account_epoch) =
            account_read_binding(&binding).expect("validated status can bind an account read");
        assert_eq!(
            serde_json::to_value(AccountReadPayload { opaque_account_key })
                .expect("serialize fixed account-read payload"),
            serde_json::json!({"opaqueAccountKey": opaque_account_key})
        );
        assert_eq!(account_epoch, "5ce47c6b-387a-4f92-9dd9-0bdb1082bc07");

        let mut stale = binding.clone();
        stale.account_epoch = Some("5CE47C6B-387A-4F92-9DD9-0BDB1082BC07".to_string());
        assert_eq!(
            account_read_binding(&stale),
            Err(AccountProjectionError::Unavailable)
        );
    }

    #[test]
    fn account_read_response_requires_echoed_epoch_and_shared_payload_limit() {
        let expected_epoch = "5ce47c6b-387a-4f92-9dd9-0bdb1082bc07";
        let response = BrokerCallResponse {
            metadata: BrokerResponseMetadata {
                account_epoch: Some(expected_epoch.to_string()),
                descriptor_generation: None,
                expires_at_ms: None,
            },
            payload: vec![0; MAX_ACCOUNT_READ_PAYLOAD_BYTES],
            fds: vec![],
        };
        assert_eq!(
            validate_account_read_response_epoch(&response, expected_epoch),
            Ok(())
        );
        assert_eq!(
            validate_account_read_payload_size(
                &response.payload,
                AccountProjectionError::InvalidUsage
            ),
            Ok(())
        );

        let stale_response = BrokerCallResponse {
            metadata: BrokerResponseMetadata {
                account_epoch: Some("4ce47c6b-387a-4f92-9dd9-0bdb1082bc07".to_string()),
                descriptor_generation: None,
                expires_at_ms: None,
            },
            payload: vec![0; MAX_ACCOUNT_READ_PAYLOAD_BYTES + 1],
            fds: vec![],
        };
        assert_eq!(
            validate_account_read_response_epoch(&stale_response, expected_epoch),
            Err(AccountProjectionError::Unavailable)
        );
        assert_eq!(
            validate_account_read_payload_size(
                &stale_response.payload,
                AccountProjectionError::InvalidUsage
            ),
            Err(AccountProjectionError::InvalidUsage)
        );
    }

    #[test]
    fn broker_response_rejects_malformed_status_code_and_account_metadata_pairs() {
        let malformed_success: BrokerResponse = serde_json::from_value(serde_json::json!({
            "schemaVersion": 1,
            "requestID": "request-0001",
            "status": "ok",
            "code": "staleEpoch",
            "payloadBase64URL": "e30",
        }))
        .expect("wire shape is decodable before semantic validation");
        assert!(
            validate_response_envelope(
                &malformed_success,
                BrokerOperation::AccountUsageRead,
                b"{}",
                &[]
            )
            .is_err()
        );

        let malformed_error: BrokerResponse = serde_json::from_value(serde_json::json!({
            "schemaVersion": 1,
            "requestID": "request-0001",
            "status": "error",
            "code": "staleEpoch",
            "accountEpoch": "5ce47c6b-387a-4f92-9dd9-0bdb1082bc07",
            "payloadBase64URL": "e30",
        }))
        .expect("wire shape is decodable before semantic validation");
        assert!(
            validate_response_envelope(
                &malformed_error,
                BrokerOperation::AccountUsageRead,
                b"{}",
                &[]
            )
            .is_err()
        );
    }

    #[test]
    fn broker_descriptor_payload_requires_exact_bounded_runtime_and_socket_fields() {
        let socket_path = Path::new("/private/tmp/whisply/.native-broker/v1.sock");
        let valid = br#"{"runtimeHome":"/Users/test/.whisply/accounts/acct_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/runtime","brokerSocketPath":"/private/tmp/whisply/.native-broker/v1.sock"}"#;
        assert!(validate_broker_descriptor_payload(valid, socket_path).is_ok());

        let unknown_key = br#"{"runtimeHome":"/safe","brokerSocketPath":"/private/tmp/whisply/.native-broker/v1.sock","extra":"rejected"}"#;
        assert!(validate_broker_descriptor_payload(unknown_key, socket_path).is_err());

        let wrong_socket =
            br#"{"runtimeHome":"/safe","brokerSocketPath":"/private/tmp/other.sock"}"#;
        assert!(validate_broker_descriptor_payload(wrong_socket, socket_path).is_err());
    }

    #[test]
    fn callback_urls_are_never_valid_beyond_the_private_whisply_route() {
        assert!(valid_callback_url("whisply://auth-callback?code=opaque"));
        assert!(!valid_callback_url("https://auth-callback?code=opaque"));
        assert!(!valid_callback_url("whisply://other?code=opaque"));
    }

    #[test]
    fn managed_snapshot_redacts_bearer_debug_output() {
        let snapshot = GatewayDescriptorSnapshot::for_test(
            "brokered-session-token",
            "https://proxy.whisply.net/v1",
        );
        let debug = format!("{snapshot:?}");
        assert!(!debug.contains("brokered-session-token"));
        assert!(debug.contains("[REDACTED]"));
    }

    #[test]
    fn descriptor_refresh_policy_requires_a_changed_opaque_generation() {
        let now_ms = 1_786_126_401_000;
        let unknown_expiry = GatewayDescriptorSnapshot::for_test_with(
            "brokered-session-token",
            "https://proxy.whisply.net/v1",
            None,
            None,
        );
        let expiring = GatewayDescriptorSnapshot::for_test_with(
            "brokered-session-token",
            "https://proxy.whisply.net/v1",
            Some("63ff45f8-1a7d-49c0-a892-9df2ca3e0b59"),
            Some(now_ms + REFRESH_SKEW_MS),
        );
        let fresh = GatewayDescriptorSnapshot::for_test_with(
            "brokered-session-token",
            "https://proxy.whisply.net/v1",
            Some("5b13390a-6ddd-4d43-a30e-159cc54e0e2c"),
            Some(now_ms + REFRESH_SKEW_MS + 1),
        );
        let same_generation = GatewayDescriptorSnapshot::for_test_with(
            "brokered-session-token",
            "https://proxy.whisply.net/v1",
            Some("5b13390a-6ddd-4d43-a30e-159cc54e0e2c"),
            Some(now_ms + REFRESH_SKEW_MS + 1),
        );

        assert!(descriptor_requires_refresh(&unknown_expiry, now_ms));
        assert!(descriptor_requires_refresh(&expiring, now_ms));
        assert!(!descriptor_requires_refresh(&fresh, now_ms));
        assert!(replacement_generation_changed(&fresh, &expiring));
        assert!(replacement_generation_changed(&expiring, &fresh));
        assert!(!replacement_generation_changed(&same_generation, &fresh));
    }
}

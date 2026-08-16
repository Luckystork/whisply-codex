//! Zero-authority diagnostics, fixture, and local-IPC contracts.
//!
//! Diagnostics are deliberately not an alternate authority plane. Presentation
//! fixtures and contract replay use isolated temporary roots and an
//! authenticated one-time local channel; real integration uses normal product
//! authority and is never allowed to accept fixture shortcuts.

use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::Hmac;
use hmac::Mac;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use sha2::Sha256;
use tempfile::Builder as TempDirBuilder;
use tempfile::TempDir;
use thiserror::Error;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::catalog::canonical_json_bytes;

/// Current local diagnostic request/event/IPC schema.
pub const DIAGNOSTIC_PROTOCOL_SCHEMA_VERSION: u16 = 1;
/// A fixture IPC request is deliberately smaller than the broker frame cap.
pub const MAX_DIAGNOSTIC_FRAME_BYTES: usize = 16 * 1024;
/// Fixture sessions are short lived by default and can never exceed one day.
pub const DEFAULT_FIXTURE_SESSION_TTL_MS: i64 = 30 * 60 * 1_000;
pub const MAX_FIXTURE_SESSION_TTL_MS: i64 = 24 * 60 * 60 * 1_000;
/// Persistent visible/a11y marker required on every fixture host surface.
pub const PRESENTATION_FIXTURE_MARKER: &str = "PRESENTATION FIXTURE — NO ACTION EXECUTED";
/// Current immutable registry schema for deterministic diagnostic scenarios.
pub const DIAGNOSTIC_SCENARIO_REGISTRY_SCHEMA_VERSION: u16 = 1;
/// Stable protocol identifier for deterministic diagnostic scenario metadata.
pub const DIAGNOSTIC_SCENARIO_REGISTRY_PROTOCOL: &str = "whisply.diagnostics.v1";
/// Runner contract version accepted by this runtime line.
pub const DIAGNOSTIC_SCENARIO_RUNNER_VERSION: &str = "1.0.0";

type HmacSha256 = Hmac<Sha256>;

/// Hard-separated runtime diagnostic lanes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticLane {
    PresentationFixture,
    ContractReplay,
    RealIntegration,
    ReleaseOrchestration,
}

impl DiagnosticLane {
    /// Whether this lane may initialize any real authority owner.
    pub const fn can_initialize_real_authority(self) -> bool {
        matches!(self, Self::RealIntegration)
    }

    /// Fixture/replay lanes are network denied. A release orchestrator can
    /// compose named suites, but cannot itself obtain a provider, broker, or
    /// mutation authority.
    pub const fn network_is_denied(self) -> bool {
        !matches!(self, Self::RealIntegration)
    }
}

/// Immutable metadata for deterministic diagnostic scenarios. It describes a
/// fixture contract only; it has no executable, account, endpoint, or
/// authority-bearing field.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiagnosticScenarioRegistry {
    pub schema_version: u16,
    pub protocol: String,
    pub runner_version: String,
    pub commercial_boundary: DiagnosticCommercialBoundary,
    pub areas: Vec<String>,
    pub scenarios: Vec<DiagnosticScenario>,
}

impl DiagnosticScenarioRegistry {
    /// Validates fixed schema, fixture authority, and evidence invariants
    /// before a controller can enumerate the metadata. This deliberately
    /// does not run, launch, or attach a scenario to a normal app process.
    pub fn validate(&self) -> Result<(), DiagnosticScenarioRegistryError> {
        if self.schema_version != DIAGNOSTIC_SCENARIO_REGISTRY_SCHEMA_VERSION
            || self.protocol != DIAGNOSTIC_SCENARIO_REGISTRY_PROTOCOL
            || self.runner_version != DIAGNOSTIC_SCENARIO_RUNNER_VERSION
            || !self.commercial_boundary.is_fixed_zero_authority_boundary()
        {
            return Err(DiagnosticScenarioRegistryError::InvalidContract);
        }

        let areas = unique_nonempty_registry_slugs(&self.areas)?;
        if self.scenarios.is_empty() {
            return Err(DiagnosticScenarioRegistryError::InvalidContract);
        }

        let mut scenario_ids = BTreeSet::new();
        for scenario in &self.scenarios {
            if !scenario_ids.insert(scenario.id.as_str())
                || !areas.contains(scenario.area.as_str())
                || !scenario.is_fixed_zero_authority_fixture()
            {
                return Err(DiagnosticScenarioRegistryError::InvalidContract);
            }
        }
        Ok(())
    }
}

/// Parses and validates a diagnostic registry without accepting a path,
/// process, or authority owner. Controllers should use this only for an
/// immutable in-binary resource.
pub fn parse_diagnostic_scenario_registry(
    source: &str,
) -> Result<DiagnosticScenarioRegistry, DiagnosticScenarioRegistryError> {
    let registry: DiagnosticScenarioRegistry = serde_json::from_str(source)
        .map_err(|_| DiagnosticScenarioRegistryError::InvalidEncoding)?;
    registry.validate()?;
    Ok(registry)
}

/// The fixed boundary all deterministic scenario metadata must preserve.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiagnosticCommercialBoundary {
    pub isolated_fixtures: String,
    pub production_targets: String,
    pub forbidden_authorities: Vec<String>,
}

impl DiagnosticCommercialBoundary {
    fn is_fixed_zero_authority_boundary(&self) -> bool {
        const REQUIRED: [&str; 6] = [
            "supabase-session",
            "subscription",
            "entitlement",
            "commercial-funding",
            "usage-reservation",
            "provider-request",
        ];
        let forbidden = self
            .forbidden_authorities
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        self.isolated_fixtures == "presentation-only-no-live-authority"
            && self.production_targets == "real-server-side-gates-only"
            && forbidden == REQUIRED.into_iter().collect()
            && forbidden.len() == self.forbidden_authorities.len()
    }
}

/// One deterministic, non-authoritative fixture scenario.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiagnosticScenario {
    pub id: String,
    pub version: u32,
    pub title: String,
    pub description: String,
    pub area: String,
    pub tags: Vec<String>,
    pub adapter: String,
    pub test_filter: String,
    pub steps: Vec<String>,
    pub defaults: DiagnosticScenarioDefaults,
    pub authority: DiagnosticScenarioAuthority,
    pub fixture: DiagnosticScenarioFixture,
    pub required_evidence: Vec<String>,
    pub assertions: Vec<String>,
}

impl DiagnosticScenario {
    fn is_fixed_zero_authority_fixture(&self) -> bool {
        self.version >= 1
            && valid_registry_slug(&self.id)
            && valid_registry_slug(&self.area)
            && (3..=120).contains(&self.title.chars().count())
            && (20..=1_000).contains(&self.description.chars().count())
            && unique_registry_slugs(&self.tags).is_ok()
            && self.adapter == "swift-xctest"
            && self.test_filter == "WhisplyDiagnosticScenarioTests/testRunSelectedScenario"
            && unique_nonempty_registry_slugs(&self.steps).is_ok()
            && self.defaults.is_valid()
            && self.authority.is_fixed_zero_authority()
            && self.fixture.is_fixed_zero_authority()
            && required_evidence_is_complete(&self.required_evidence)
            && unique_nonempty_registry_slugs(&self.assertions).is_ok()
    }
}

/// Deterministic presentation defaults for a scenario.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiagnosticScenarioDefaults {
    pub seed: u32,
    pub clock: String,
    pub width: u16,
    pub height: u16,
    pub appearance: String,
    pub reduce_motion: bool,
    pub increased_contrast: bool,
    pub locale: String,
}

impl DiagnosticScenarioDefaults {
    fn is_valid(&self) -> bool {
        (320..=1_920).contains(&self.width)
            && (180..=1_440).contains(&self.height)
            && matches!(self.appearance.as_str(), "light" | "dark")
            && self.clock.ends_with('Z')
            && OffsetDateTime::parse(&self.clock, &Rfc3339).is_ok()
            && valid_locale(&self.locale)
    }
}

/// Fixture-only authority contract. Every field is fixed so a registry cannot
/// switch an XCTest fixture into a real product or commercial execution lane.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiagnosticScenarioAuthority {
    pub host: String,
    pub network: String,
    pub provider_requests: String,
    pub commercial_authority: String,
    pub state_forcing: String,
    pub production_mutation: String,
    pub requires_authenticated_account: bool,
}

impl DiagnosticScenarioAuthority {
    fn is_fixed_zero_authority(&self) -> bool {
        self.host == "xctest-only"
            && self.network == "forbidden"
            && self.provider_requests == "forbidden"
            && self.commercial_authority == "none"
            && self.state_forcing == "isolated-fixtures-only"
            && self.production_mutation == "forbidden"
            && !self.requires_authenticated_account
    }
}

/// Synthetic fixture state. It may describe a presentation but cannot mint
/// authority, reserve Usage, or reach a provider.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiagnosticScenarioFixture {
    pub account_state: String,
    pub subscription_presentation: String,
    pub can_mint_authority: bool,
    pub can_reserve_usage: bool,
    pub can_reach_provider: bool,
}

impl DiagnosticScenarioFixture {
    fn is_fixed_zero_authority(&self) -> bool {
        matches!(
            self.account_state.as_str(),
            "none" | "synthetic-signed-out" | "synthetic-local-account"
        ) && valid_registry_slug(&self.subscription_presentation)
            && !self.can_mint_authority
            && !self.can_reserve_usage
            && !self.can_reach_provider
    }
}

/// A deterministic fixture request. It deliberately cannot name a network
/// endpoint, executable, account ID, or credential.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FixtureScenario {
    pub fixture_id: String,
    pub surface_id: String,
    pub state_id: String,
    pub seed: u64,
    pub lane: DiagnosticLane,
    pub fixture_root: PathBuf,
    pub simulated: bool,
    pub billable: bool,
    pub authority: String,
}

impl FixtureScenario {
    /// Validates the zero-authority fixture contract before launch.
    pub fn validate(&self) -> Result<(), FixtureValidationError> {
        if self.lane != DiagnosticLane::PresentationFixture
            || !self.simulated
            || self.billable
            || self.authority != "none"
        {
            return Err(FixtureValidationError::AuthorityViolation);
        }
        validate_id(&self.fixture_id)?;
        validate_id(&self.surface_id)?;
        validate_id(&self.state_id)?;
        validate_owner_only_temporary_root(&self.fixture_root)
            .map_err(|_| FixtureValidationError::UnsafeRoot(self.fixture_root.clone()))
    }
}

/// Appearance selection that the real fixture host may render. It cannot
/// affect the ordinary application process or increase authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FixtureAppearance {
    Light,
    Dark,
    Auto,
}

/// Product themes that a zero-authority fixture may render. The set mirrors
/// the built-in Whisply theme catalog and intentionally excludes `custom` and
/// saved user themes, so fixture rendering cannot read or mutate user-owned
/// appearance data.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FixtureTheme {
    System,
    Light,
    Dark,
    Midnight,
    Graphite,
    Meadow,
    Sunset,
    Dawn,
    Linen,
    Rainbow,
    Galaxy,
    Aurora,
    Ocean,
    Ember,
    Sol,
}

/// Exact layout direction available to the isolated fixture process. This is
/// a render-tree environment value, not a setting for an ordinary app window
/// or the person's macOS language/accessibility configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FixtureLayoutDirection {
    LeftToRight,
    RightToLeft,
}

/// Bounded SwiftUI Dynamic Type scale available to the isolated fixture
/// process. The values are semantic, exact, and closed so diagnostics cannot
/// ask a host to infer or read a person's preferred text size.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FixtureFontScale {
    Small,
    Standard,
    Large,
}

/// Fixture-local presentation controls that affect only the isolated fixture
/// process. Catalog execution derives these from immutable defaults; direct
/// owner-only fixtures may choose bounded locale, direction, font scale,
/// motion, and contrast variants without widening fixture authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FixturePresentation {
    pub locale: String,
    pub layout_direction: FixtureLayoutDirection,
    pub font_scale: FixtureFontScale,
    pub reduce_motion: bool,
    pub increased_contrast: bool,
}

impl FixturePresentation {
    fn is_valid(&self) -> bool {
        valid_locale(&self.locale)
    }
}

/// A bounded display selection for the isolated fixture window. The numeric
/// backing-scale representation avoids serializing a floating-point control:
/// `2.0` is carried as `2000`. This selects only an already-attached display
/// for the short-lived fixture process; it never changes a person's display
/// arrangement, resolution, accessibility setting, or ordinary app window.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FixtureDisplay {
    pub display_identifier: Option<u32>,
    pub backing_scale_milli: Option<u16>,
}

impl FixtureDisplay {
    fn is_valid(&self) -> bool {
        self.display_identifier
            .is_none_or(|identifier| identifier != 0)
            && self.backing_scale_milli.is_none_or(|scale| {
                self.display_identifier.is_some() && (500..=8_000).contains(&scale)
            })
    }
}

/// A self-capture point owned by the fixture process, never by Screen
/// Recording or Computer Use.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FixtureCapturePoint {
    Initial,
    NamedEvent { event_id: String },
    Terminal,
}

/// The one direct, real-Swift static-fixture event allowed outside the
/// catalog-thread timeline. It selects the existing callback-inert streaming
/// Chat Overlay atom; callers cannot provide an event stream or event name of
/// their own.
const CHAT_OVERLAY_STREAMING_CAPTURE_EVENT_ID: &str = "chat-overlay-streaming";

fn static_fixture_capture_matches(
    scenario: &FixtureScenario,
    capture: &FixtureCapturePoint,
) -> bool {
    match (
        scenario.surface_id.as_str(),
        scenario.state_id.as_str(),
        capture,
    ) {
        ("chat.overlay", "empty", FixtureCapturePoint::Initial) => true,
        ("chat.overlay", "streaming", FixtureCapturePoint::NamedEvent { event_id }) => {
            event_id == CHAT_OVERLAY_STREAMING_CAPTURE_EVENT_ID
        }
        ("chat.overlay", "needs-input", FixtureCapturePoint::Terminal) => true,
        ("history", "empty", FixtureCapturePoint::Initial)
        | ("preferences", "general", FixtureCapturePoint::Initial)
        | ("preferences", "usage", FixtureCapturePoint::Initial)
        | ("preferences", "account", FixtureCapturePoint::Initial)
        | ("subscription.gate", "starter", FixtureCapturePoint::Initial) => true,
        _ => false,
    }
}

/// Typed `force-ui` request. A host may only render registered fixture state;
/// it has no route to a normal app process, desktop capture, Keychain, or a
/// live account.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ForceUiRequest {
    pub scenario: FixtureScenario,
    pub width: u16,
    pub height: u16,
    pub appearance: FixtureAppearance,
    pub theme: FixtureTheme,
    pub presentation: FixturePresentation,
    pub display: FixtureDisplay,
    pub capture: FixtureCapturePoint,
    pub watermark: String,
    /// Optional catalog-attested plan/timeline/event/selector handoff. A
    /// generic static fixture leaves this absent; a deterministic fixture
    /// thread supplies it so the native host can validate and render the
    /// exact zero-authority projection.
    pub fixture_thread: Option<crate::DiagnosticFixtureExecution>,
    /// Optional reduced replay snapshot for a catalog-attested fixture thread.
    /// It is derived from the immutable virtual timeline and can never carry a
    /// caller-authored event, clock, target, path, account, or authority.
    pub fixture_replay: Option<crate::DiagnosticFixtureReplayState>,
}

impl ForceUiRequest {
    /// Validates only presentation and display controls. There is intentionally no
    /// generic `force`, `mock`, `skip-policy`, or authority override field.
    pub fn validate(&self) -> Result<(), FixtureValidationError> {
        self.scenario.validate()?;
        if self.width == 0
            || self.height == 0
            || self.width > 8_192
            || self.height > 8_192
            || self.watermark != PRESENTATION_FIXTURE_MARKER
            || !self.presentation.is_valid()
            || !self.display.is_valid()
        {
            return Err(FixtureValidationError::InvalidPresentationRequest);
        }
        if let FixtureCapturePoint::NamedEvent { event_id } = &self.capture {
            validate_id(event_id)?;
        }
        match (&self.fixture_thread, &self.fixture_replay) {
            (Some(fixture_thread), fixture_replay) => {
                let fixture_capture_matches_state = match (&self.scenario.state_id, &self.capture) {
                    (state, FixtureCapturePoint::Initial) => state == "initial",
                    (state, FixtureCapturePoint::Terminal) => state == "terminal",
                    (state, FixtureCapturePoint::NamedEvent { event_id }) => {
                        state == "event" && fixture_thread.supports_named_capture_event(event_id)
                    }
                };
                let fixture_capture_matches_replay = match fixture_replay {
                    Some(replay) => fixture_capture_for_replay(fixture_thread, replay)
                        .is_ok_and(|expected| expected == self.capture),
                    None => true,
                };
                if self.scenario.surface_id != "chat.activity"
                    || fixture_thread.plan.fixture_id != self.scenario.fixture_id
                    || fixture_thread.plan.seed
                        != u32::try_from(self.scenario.seed)
                            .map_err(|_| FixtureValidationError::InvalidPresentationRequest)?
                    || fixture_thread.plan.lane != self.scenario.lane
                    || fixture_thread.plan.simulated != self.scenario.simulated
                    || fixture_thread.plan.billable != self.scenario.billable
                    || self.scenario.authority != "none"
                    || !fixture_capture_matches_state
                    || !fixture_capture_matches_replay
                {
                    return Err(FixtureValidationError::InvalidPresentationRequest);
                }
            }
            (None, None) => {
                if !static_fixture_capture_matches(&self.scenario, &self.capture) {
                    return Err(FixtureValidationError::InvalidPresentationRequest);
                }
            }
            // A replay state has meaning only beside the exact catalog execution
            // it was reduced from; it cannot turn a direct static fixture into
            // a generic mutable replay surface.
            (None, Some(_)) => return Err(FixtureValidationError::InvalidPresentationRequest),
        }
        Ok(())
    }
}

/// Derives the one reviewed capture point for a validated replay snapshot.
/// Each snapshot index maps to the matching catalog event; callers cannot
/// choose an unrelated named event or normal-application state alongside it.
pub fn fixture_capture_for_replay(
    execution: &crate::DiagnosticFixtureExecution,
    replay: &crate::DiagnosticFixtureReplayState,
) -> Result<FixtureCapturePoint, FixtureValidationError> {
    if !replay.is_valid_for_execution(execution) {
        return Err(FixtureValidationError::InvalidPresentationRequest);
    }
    let last_index = execution
        .timeline
        .projections
        .len()
        .checked_sub(1)
        .ok_or(FixtureValidationError::InvalidPresentationRequest)?;
    let checkpoint_index = usize::from(replay.checkpoint_index);
    if checkpoint_index == 0 {
        return Ok(FixtureCapturePoint::Initial);
    }
    if checkpoint_index == last_index {
        return Ok(FixtureCapturePoint::Terminal);
    }
    let event = execution
        .events
        .events
        .get(checkpoint_index)
        .filter(|event| usize::from(event.sequence) == checkpoint_index)
        .filter(|event| execution.supports_named_capture_event(&event.event_id))
        .ok_or(FixtureValidationError::InvalidPresentationRequest)?;
    Ok(FixtureCapturePoint::NamedEvent {
        event_id: event.event_id.clone(),
    })
}

/// A contract replay never contacts a product authority. The referenced input
/// and all exported output must live under the same owner-only root.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContractReplayRequest {
    pub replay_id: String,
    pub root: PathBuf,
    pub input: PathBuf,
    pub output: PathBuf,
    pub simulated: bool,
    pub billable: bool,
    pub authority: String,
}

impl ContractReplayRequest {
    pub fn validate(&self) -> Result<(), DiagnosticSecurityError> {
        validate_diagnostic_id(&self.replay_id)?;
        if !self.simulated || self.billable || self.authority != "none" {
            return Err(DiagnosticSecurityError::AuthorityViolation);
        }
        validate_owner_only_temporary_root(&self.root)?;
        validate_path_inside_root(&self.root, &self.input)?;
        validate_path_inside_root(&self.root, &self.output)?;
        Ok(())
    }
}

/// Explicit real-integration selector. It contains no bearer, account UUID,
/// raw email, or test bypass. Integration is still subject to normal broker,
/// Usage, permissions, sandbox, and confirmation checks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RealIntegrationAccount {
    Current,
    AuthenticatedAlias(String),
}

/// Real integration deliberately has a different request type from fixtures.
/// There are no fixture clocks, mock outcomes, forced UI state, or policy
/// shortcuts in this shape.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RealIntegrationRequest {
    pub test_id: String,
    pub account: RealIntegrationAccount,
    pub operation: String,
}

impl RealIntegrationRequest {
    pub fn validate(&self) -> Result<(), DiagnosticSecurityError> {
        validate_diagnostic_id(&self.test_id)?;
        validate_diagnostic_id(&self.operation)?;
        if let RealIntegrationAccount::AuthenticatedAlias(alias) = &self.account {
            validate_diagnostic_id(alias)?;
        }
        Ok(())
    }
}

/// The only terminal categories diagnostics may report. None represent an
/// approval, entitlement, Usage settlement, or authorization decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticTerminalStatus {
    Succeeded,
    Failed,
    Unavailable,
    Denied,
    Cancelled,
}

/// Metadata recorded for self-captured fixture artifacts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FixtureArtifactManifest {
    pub schema_version: u16,
    pub fixture_id: String,
    pub runtime_version: String,
    pub registry_hash: String,
    pub presentation_fixture: bool,
    pub authority: String,
    pub network: String,
    pub artifacts: Vec<PathBuf>,
}

impl FixtureArtifactManifest {
    /// Artifacts are valid only when every path is inside the one fixture
    /// root and the immutable zero-authority facts are preserved.
    pub fn validate_for_root(&self, root: &Path) -> Result<(), DiagnosticSecurityError> {
        if self.schema_version != DIAGNOSTIC_PROTOCOL_SCHEMA_VERSION
            || !self.presentation_fixture
            || self.authority != "none"
            || self.network != "denied"
            || self.artifacts.len() > 256
        {
            return Err(DiagnosticSecurityError::InvalidManifest);
        }
        validate_diagnostic_id(&self.fixture_id)?;
        if !valid_lowercase_sha256(&self.registry_hash)
            || self.runtime_version.is_empty()
            || self.runtime_version.len() > 256
        {
            return Err(DiagnosticSecurityError::InvalidManifest);
        }
        for artifact in &self.artifacts {
            validate_path_inside_root(root, artifact)?;
        }
        Ok(())
    }
}

/// Release-bound metadata handed to an isolated fixture host. Capability
/// material is intentionally absent: it is inherited on a file descriptor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiagnosticLaunchManifest {
    pub schema_version: u16,
    pub lane: DiagnosticLane,
    pub release_manifest_sha256: String,
    pub fixture_root: PathBuf,
    pub socket_path: PathBuf,
    pub expires_at_ms: i64,
}

impl DiagnosticLaunchManifest {
    pub fn validate(&self, now_ms: i64) -> Result<(), DiagnosticSecurityError> {
        if self.schema_version != DIAGNOSTIC_PROTOCOL_SCHEMA_VERSION
            || !matches!(
                self.lane,
                DiagnosticLane::PresentationFixture | DiagnosticLane::ContractReplay
            )
            || !valid_lowercase_sha256(&self.release_manifest_sha256)
            || self.expires_at_ms <= now_ms
            || self.expires_at_ms.saturating_sub(now_ms) > MAX_FIXTURE_SESSION_TTL_MS
        {
            return Err(DiagnosticSecurityError::InvalidManifest);
        }
        validate_owner_only_temporary_root(&self.fixture_root)?;
        validate_socket_path_inside_root(&self.fixture_root, &self.socket_path)
    }
}

/// An owner-only temporary root that cleans itself up unless explicitly
/// exported. The caller must never substitute a user-configured directory.
pub struct OwnerOnlyTemporaryRoot {
    temporary: TempDir,
}

impl std::fmt::Debug for OwnerOnlyTemporaryRoot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OwnerOnlyTemporaryRoot")
            .field("path", &"[owner-only temporary root]")
            .finish()
    }
}

impl OwnerOnlyTemporaryRoot {
    /// Creates a new 0700 root through `tempfile`, then revalidates owner,
    /// mode, type, and no-symlink state before any fixture can use it.
    pub fn new(prefix: &str) -> Result<Self, DiagnosticSecurityError> {
        validate_diagnostic_id(prefix)?;
        let temporary = TempDirBuilder::new()
            .prefix(prefix)
            .tempdir()
            .map_err(|_| DiagnosticSecurityError::Io)?;
        set_owner_only_mode(temporary.path())?;
        validate_owner_only_temporary_root(temporary.path())?;
        Ok(Self { temporary })
    }

    pub fn path(&self) -> &Path {
        self.temporary.path()
    }

    /// Reserves a direct child path. The caller still has to create it with a
    /// no-follow API appropriate for its type; arbitrary absolute paths and
    /// traversal are never accepted here.
    pub fn child(&self, name: &str) -> Result<PathBuf, DiagnosticSecurityError> {
        validate_diagnostic_id(name)?;
        let path = self.path().join(name);
        validate_path_inside_root(self.path(), &path)?;
        Ok(path)
    }
}

/// One bounded JSON frame on a short-lived fixture control socket. The outer
/// transport is length-prefixed; callers must run `decode_bounded_frame`
/// before deserializing anything else.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiagnosticIpcFrame {
    pub schema_version: u16,
    pub session_id: String,
    pub sequence: u64,
    pub issued_at_ms: i64,
    pub nonce: String,
    pub payload: Value,
    pub mac_base64_url: String,
}

/// Local fixture channel state. Its one-time 32-byte capability is held only
/// in memory and zeroized on drop; it never appears in a serializable type,
/// argv, env, URL, fixture manifest, or log message.
pub struct DiagnosticSessionAuthenticator {
    session_id: String,
    root: PathBuf,
    release_manifest_sha256: String,
    created_at_ms: i64,
    expires_at_ms: i64,
    expected_sequence: u64,
    used_nonces: BTreeSet<String>,
    capability: Zeroizing<Vec<u8>>,
}

impl std::fmt::Debug for DiagnosticSessionAuthenticator {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DiagnosticSessionAuthenticator")
            .field("session_id", &self.session_id)
            .field("root", &"[owner-only temporary root]")
            .field("release_manifest_sha256", &self.release_manifest_sha256)
            .field("created_at_ms", &self.created_at_ms)
            .field("expires_at_ms", &self.expires_at_ms)
            .field("expected_sequence", &self.expected_sequence)
            .field("capability", &"[REDACTED]")
            .finish()
    }
}

impl DiagnosticSessionAuthenticator {
    pub fn new(
        root: &OwnerOnlyTemporaryRoot,
        release_manifest_sha256: &str,
        now_ms: i64,
        expires_at_ms: i64,
        capability: [u8; 32],
    ) -> Result<Self, DiagnosticSecurityError> {
        if !valid_lowercase_sha256(release_manifest_sha256)
            || expires_at_ms <= now_ms
            || expires_at_ms.saturating_sub(now_ms) > MAX_FIXTURE_SESSION_TTL_MS
        {
            return Err(DiagnosticSecurityError::InvalidManifest);
        }
        validate_owner_only_temporary_root(root.path())?;
        Ok(Self {
            session_id: Uuid::new_v4().to_string(),
            root: root.path().to_path_buf(),
            release_manifest_sha256: release_manifest_sha256.to_string(),
            created_at_ms: now_ms,
            expires_at_ms,
            expected_sequence: 1,
            used_nonces: BTreeSet::new(),
            capability: Zeroizing::new(capability.to_vec()),
        })
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn release_manifest_sha256(&self) -> &str {
        &self.release_manifest_sha256
    }

    /// Signs one frame for a fixture peer. This does not consume the sequence;
    /// receiving and verifying the frame does, which makes duplicate delivery
    /// and out-of-order delivery fail closed.
    pub fn sign_frame(
        &self,
        sequence: u64,
        issued_at_ms: i64,
        nonce: String,
        payload: Value,
    ) -> Result<DiagnosticIpcFrame, DiagnosticSecurityError> {
        validate_nonce(&nonce)?;
        if !valid_safe_diagnostic_json(&payload, 0) {
            return Err(DiagnosticSecurityError::UnsafePayload);
        }
        let mut frame = DiagnosticIpcFrame {
            schema_version: DIAGNOSTIC_PROTOCOL_SCHEMA_VERSION,
            session_id: self.session_id.clone(),
            sequence,
            issued_at_ms,
            nonce,
            payload,
            mac_base64_url: String::new(),
        };
        frame.mac_base64_url = self.frame_mac(&frame)?;
        Ok(frame)
    }

    /// Verifies the bounded MACed frame plus expiry, root, UID/mode, unique
    /// nonce, and strict local monotonic sequence. It contains no global
    /// ordering claim and never turns a fixture message into real authority.
    pub fn verify_frame(
        &mut self,
        frame: &DiagnosticIpcFrame,
        now_ms: i64,
    ) -> Result<(), DiagnosticSecurityError> {
        validate_owner_only_temporary_root(&self.root)?;
        if now_ms > self.expires_at_ms
            || frame.schema_version != DIAGNOSTIC_PROTOCOL_SCHEMA_VERSION
            || frame.session_id != self.session_id
            || frame.issued_at_ms < self.created_at_ms
            || frame.issued_at_ms > self.expires_at_ms
            || frame.sequence != self.expected_sequence
            || self.used_nonces.len() >= 1_024
            || self.used_nonces.contains(&frame.nonce)
        {
            return Err(DiagnosticSecurityError::InvalidFrame);
        }
        validate_nonce(&frame.nonce)?;
        if !valid_safe_diagnostic_json(&frame.payload, 0) {
            return Err(DiagnosticSecurityError::UnsafePayload);
        }
        let serialized =
            serde_json::to_vec(frame).map_err(|_| DiagnosticSecurityError::InvalidFrame)?;
        if serialized.len() > MAX_DIAGNOSTIC_FRAME_BYTES {
            return Err(DiagnosticSecurityError::FrameTooLarge);
        }
        let encoded_mac = URL_SAFE_NO_PAD
            .decode(frame.mac_base64_url.as_bytes())
            .map_err(|_| DiagnosticSecurityError::InvalidMac)?;
        if encoded_mac.len() != 32 {
            return Err(DiagnosticSecurityError::InvalidMac);
        }
        let expected = self.frame_mac(frame)?;
        let expected = URL_SAFE_NO_PAD
            .decode(expected.as_bytes())
            .map_err(|_| DiagnosticSecurityError::InvalidMac)?;
        let mut verifier = HmacSha256::new_from_slice(self.capability.as_slice())
            .map_err(|_| DiagnosticSecurityError::InvalidMac)?;
        verifier.update(&frame_mac_input(frame)?);
        verifier
            .verify_slice(&encoded_mac)
            .map_err(|_| DiagnosticSecurityError::InvalidMac)?;
        // Keep a constant-time verification path above. This equality only
        // asserts the local encoder's exact bounded base64 framing.
        if expected != encoded_mac {
            return Err(DiagnosticSecurityError::InvalidMac);
        }
        self.used_nonces.insert(frame.nonce.clone());
        self.expected_sequence = self
            .expected_sequence
            .checked_add(1)
            .ok_or(DiagnosticSecurityError::InvalidFrame)?;
        Ok(())
    }

    fn frame_mac(&self, frame: &DiagnosticIpcFrame) -> Result<String, DiagnosticSecurityError> {
        let mut mac = HmacSha256::new_from_slice(self.capability.as_slice())
            .map_err(|_| DiagnosticSecurityError::InvalidMac)?;
        mac.update(&frame_mac_input(frame)?);
        Ok(URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes()))
    }
}

/// Reads the fixture capability only from an inherited descriptor. The caller
/// owns the descriptor source; this helper refuses trailing bytes so an FD
/// cannot smuggle a concatenated secret or second protocol value.
#[cfg(unix)]
pub fn read_one_time_diagnostic_capability(
    capability_fd: std::os::fd::OwnedFd,
) -> Result<[u8; 32], DiagnosticSecurityError> {
    let mut file = std::fs::File::from(capability_fd);
    let mut capability = [0_u8; 32];
    file.read_exact(&mut capability)
        .map_err(|_| DiagnosticSecurityError::InvalidCapability)?;
    let mut trailing = [0_u8; 1];
    if file
        .read(&mut trailing)
        .map_err(|_| DiagnosticSecurityError::InvalidCapability)?
        != 0
    {
        return Err(DiagnosticSecurityError::InvalidCapability);
    }
    Ok(capability)
}

#[cfg(not(unix))]
pub fn read_one_time_diagnostic_capability(_: ()) -> Result<[u8; 32], DiagnosticSecurityError> {
    Err(DiagnosticSecurityError::UnsupportedPlatform)
}

/// Pre-decode frame cap and strict schema boundary for local fixture IPC.
pub fn decode_bounded_diagnostic_frame(
    bytes: &[u8],
) -> Result<DiagnosticIpcFrame, DiagnosticSecurityError> {
    if bytes.is_empty() || bytes.len() > MAX_DIAGNOSTIC_FRAME_BYTES {
        return Err(DiagnosticSecurityError::FrameTooLarge);
    }
    serde_json::from_slice(bytes).map_err(|_| DiagnosticSecurityError::InvalidFrame)
}

/// Rejects symlinked, foreign-owned, group/world-accessible, non-directory
/// roots. A fixture must be constructed through `OwnerOnlyTemporaryRoot`; this
/// recheck protects every later IPC frame from a post-creation replacement.
#[cfg(unix)]
pub fn validate_owner_only_temporary_root(path: &Path) -> Result<(), DiagnosticSecurityError> {
    use std::os::unix::fs::MetadataExt;

    let metadata = fs::symlink_metadata(path).map_err(|_| DiagnosticSecurityError::UnsafeRoot)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(DiagnosticSecurityError::UnsafeRoot);
    }
    if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0 {
        return Err(DiagnosticSecurityError::UnsafeRoot);
    }
    Ok(())
}

#[cfg(not(unix))]
pub fn validate_owner_only_temporary_root(_: &Path) -> Result<(), DiagnosticSecurityError> {
    Err(DiagnosticSecurityError::UnsupportedPlatform)
}

/// Validates the fixture socket path before bind; it must be one direct child
/// of the owner-only temporary root and must not exist as a symlink or any
/// other user-provided file.
pub fn validate_socket_path_inside_root(
    root: &Path,
    socket_path: &Path,
) -> Result<(), DiagnosticSecurityError> {
    validate_path_inside_root(root, socket_path)?;
    if socket_path.parent() != Some(root)
        || socket_path.extension().and_then(|v| v.to_str()) != Some("sock")
    {
        return Err(DiagnosticSecurityError::UnsafeSocket);
    }
    match fs::symlink_metadata(socket_path) {
        Ok(_) => Err(DiagnosticSecurityError::UnsafeSocket),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(DiagnosticSecurityError::UnsafeSocket),
    }
}

/// Validates a bound socket's type, UID, and mode before connecting.
#[cfg(unix)]
pub fn validate_owner_only_diagnostic_socket(path: &Path) -> Result<(), DiagnosticSecurityError> {
    use std::os::unix::fs::FileTypeExt;
    use std::os::unix::fs::MetadataExt;

    let metadata = fs::symlink_metadata(path).map_err(|_| DiagnosticSecurityError::UnsafeSocket)?;
    if metadata.file_type().is_symlink()
        || !metadata.file_type().is_socket()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        return Err(DiagnosticSecurityError::UnsafeSocket);
    }
    Ok(())
}

#[cfg(not(unix))]
pub fn validate_owner_only_diagnostic_socket(_: &Path) -> Result<(), DiagnosticSecurityError> {
    Err(DiagnosticSecurityError::UnsupportedPlatform)
}

/// Verifies the peer UID on the connected fixture channel. macOS's
/// `getpeereid` is mandatory for production; unsupported Unix targets fail
/// closed instead of pretending `SO_PEERCRED` is equivalent.
#[cfg(target_os = "macos")]
pub fn validate_diagnostic_socket_peer(
    stream: &std::os::unix::net::UnixStream,
) -> Result<(), DiagnosticSecurityError> {
    use std::os::fd::AsRawFd;

    let mut peer_uid = 0;
    let mut peer_gid = 0;
    let result = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut peer_uid, &mut peer_gid) };
    if result != 0 || peer_uid != unsafe { libc::geteuid() } {
        return Err(DiagnosticSecurityError::InvalidPeer);
    }
    Ok(())
}

#[cfg(all(unix, not(target_os = "macos")))]
pub fn validate_diagnostic_socket_peer(
    _: &std::os::unix::net::UnixStream,
) -> Result<(), DiagnosticSecurityError> {
    Err(DiagnosticSecurityError::UnsupportedPlatform)
}

/// Fixture request validation failures exposed to callers without an account,
/// capability, local path, or raw frame value.
#[derive(Debug, Error)]
pub enum FixtureValidationError {
    #[error("fixture lanes may not initialize real authority")]
    AuthorityViolation,
    #[error("fixture IDs must be non-empty, bounded, and use lowercase URL-safe characters")]
    InvalidId,
    #[error("fixture root must be an absolute owner-only temporary path: {0}")]
    UnsafeRoot(PathBuf),
    #[error("fixture presentation request is invalid")]
    InvalidPresentationRequest,
}

/// Low-level fixture/replay IPC and storage validation failures. Details are
/// intentionally generic to keep paths, capabilities, and raw frames out of
/// CLI diagnostics and logs.
#[derive(Debug, Error)]
pub enum DiagnosticSecurityError {
    #[error("diagnostic temporary root is unsafe")]
    UnsafeRoot,
    #[error("diagnostic socket is unsafe")]
    UnsafeSocket,
    #[error("diagnostic socket peer could not be verified")]
    InvalidPeer,
    #[error("diagnostic fixture capability is invalid")]
    InvalidCapability,
    #[error("diagnostic frame is invalid")]
    InvalidFrame,
    #[error("diagnostic frame is too large")]
    FrameTooLarge,
    #[error("diagnostic frame MAC is invalid")]
    InvalidMac,
    #[error("diagnostic payload contains unsafe data")]
    UnsafePayload,
    #[error("diagnostic manifest is invalid")]
    InvalidManifest,
    #[error("diagnostic request would widen authority")]
    AuthorityViolation,
    #[error("diagnostic local path is outside its temporary root")]
    PathEscape,
    #[error("diagnostic id is invalid")]
    InvalidId,
    #[error("diagnostic storage operation failed")]
    Io,
    #[error("diagnostic local IPC is unsupported on this platform")]
    UnsupportedPlatform,
}

/// A deterministic scenario registry must be decoded and validated as a
/// fixed zero-authority contract before it can be listed or described.
#[derive(Debug, Error)]
pub enum DiagnosticScenarioRegistryError {
    #[error("diagnostic scenario registry encoding is invalid")]
    InvalidEncoding,
    #[error("diagnostic scenario registry violates the fixed zero-authority contract")]
    InvalidContract,
}

fn unique_registry_slugs<'a>(
    values: &'a [String],
) -> Result<BTreeSet<&'a str>, DiagnosticScenarioRegistryError> {
    let unique = values.iter().map(String::as_str).collect::<BTreeSet<_>>();
    if unique.len() != values.len() || !unique.iter().all(|value| valid_registry_slug(value)) {
        return Err(DiagnosticScenarioRegistryError::InvalidContract);
    }
    Ok(unique)
}

fn unique_nonempty_registry_slugs<'a>(
    values: &'a [String],
) -> Result<BTreeSet<&'a str>, DiagnosticScenarioRegistryError> {
    let unique = unique_registry_slugs(values)?;
    if unique.is_empty() {
        return Err(DiagnosticScenarioRegistryError::InvalidContract);
    }
    Ok(unique)
}

fn valid_registry_slug(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && !value.starts_with('-')
        && !value.ends_with('-')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn valid_locale(value: &str) -> bool {
    let mut parts = value.split(['-', '_']);
    let Some(language) = parts.next() else {
        return false;
    };
    if !(2..=3).contains(&language.len())
        || !language.bytes().all(|byte| byte.is_ascii_alphabetic())
    {
        return false;
    }
    parts.all(|part| {
        (2..=8).contains(&part.len()) && part.bytes().all(|byte| byte.is_ascii_alphanumeric())
    })
}

fn required_evidence_is_complete(values: &[String]) -> bool {
    const REQUIRED: [&str; 12] = [
        "run-manifest",
        "event-transcript",
        "result",
        "structured-log",
        "screenshot",
        "accessibility-snapshot",
        "layout-snapshot",
        "visible-copy",
        "icon-snapshot",
        "animation-snapshot",
        "authority-snapshot",
        "hash-manifest",
    ];
    let evidence = values.iter().map(String::as_str).collect::<BTreeSet<_>>();
    evidence.len() == values.len()
        && evidence.len() == REQUIRED.len()
        && evidence == REQUIRED.into_iter().collect()
}

fn frame_mac_input(frame: &DiagnosticIpcFrame) -> Result<Vec<u8>, DiagnosticSecurityError> {
    canonical_json_bytes(&serde_json::json!({
        "schemaVersion": frame.schema_version,
        "sessionID": frame.session_id,
        "sequence": frame.sequence,
        "issuedAtMS": frame.issued_at_ms,
        "nonce": frame.nonce,
        "payload": frame.payload,
    }))
    .map_err(|_| DiagnosticSecurityError::InvalidFrame)
}

fn validate_id(value: &str) -> Result<(), FixtureValidationError> {
    validate_diagnostic_id(value).map_err(|_| FixtureValidationError::InvalidId)
}

fn validate_diagnostic_id(value: &str) -> Result<(), DiagnosticSecurityError> {
    let valid = !value.is_empty()
        && value.len() <= 128
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        });
    if valid {
        Ok(())
    } else {
        Err(DiagnosticSecurityError::InvalidId)
    }
}

fn validate_nonce(value: &str) -> Result<(), DiagnosticSecurityError> {
    let valid = (16..=256).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    if valid {
        Ok(())
    } else {
        Err(DiagnosticSecurityError::InvalidFrame)
    }
}

fn validate_path_inside_root(root: &Path, path: &Path) -> Result<(), DiagnosticSecurityError> {
    if !root.is_absolute() || !path.is_absolute() || root == Path::new("/") {
        return Err(DiagnosticSecurityError::PathEscape);
    }
    let root = lexical_normalize(root)?;
    let path = lexical_normalize(path)?;
    if path.starts_with(&root) {
        Ok(())
    } else {
        Err(DiagnosticSecurityError::PathEscape)
    }
}

fn lexical_normalize(path: &Path) -> Result<PathBuf, DiagnosticSecurityError> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) => return Err(DiagnosticSecurityError::PathEscape),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => return Err(DiagnosticSecurityError::PathEscape),
            Component::Normal(component) => normalized.push(component),
        }
    }
    Ok(normalized)
}

fn valid_lowercase_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_safe_diagnostic_json(value: &Value, depth: usize) -> bool {
    if depth > 12 {
        return false;
    }
    match value {
        Value::Null | Value::Bool(_) => true,
        Value::Number(number) => number.as_f64().is_some_and(f64::is_finite),
        Value::String(value) => value.len() <= 12 * 1024,
        Value::Array(values) => {
            values.len() <= 256
                && values
                    .iter()
                    .all(|value| valid_safe_diagnostic_json(value, depth + 1))
        }
        Value::Object(values) => {
            values.len() <= 64
                && values.iter().all(|(key, value)| {
                    key.len() <= 1_024
                        && !credential_like_key(key)
                        && valid_safe_diagnostic_json(value, depth + 1)
                })
        }
    }
}

fn credential_like_key(key: &str) -> bool {
    let normalized = key.to_ascii_lowercase().replace('_', "");
    [
        "token",
        "secret",
        "password",
        "authorization",
        "cookie",
        "credential",
        "callbackurl",
        "bearer",
    ]
    .iter()
    .any(|needle| normalized.contains(needle))
}

#[cfg(unix)]
fn set_owner_only_mode(path: &Path) -> Result<(), DiagnosticSecurityError> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|_| DiagnosticSecurityError::Io)
}

#[cfg(not(unix))]
fn set_owner_only_mode(_: &Path) -> Result<(), DiagnosticSecurityError> {
    Err(DiagnosticSecurityError::UnsupportedPlatform)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    const RELEASE_HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn fixture(root: &OwnerOnlyTemporaryRoot) -> FixtureScenario {
        FixtureScenario {
            fixture_id: "chat.overlay.empty".to_string(),
            surface_id: "chat.overlay".to_string(),
            state_id: "empty".to_string(),
            seed: 42,
            lane: DiagnosticLane::PresentationFixture,
            fixture_root: root.path().to_path_buf(),
            simulated: true,
            billable: false,
            authority: "none".to_string(),
        }
    }

    #[test]
    fn fixture_lane_cannot_be_billable_or_authoritative() {
        let root = OwnerOnlyTemporaryRoot::new("whisply-fixture").expect("secure root");
        let fixture = fixture(&root);

        assert!(fixture.validate().is_ok());
        assert!(!DiagnosticLane::PresentationFixture.can_initialize_real_authority());
        assert!(DiagnosticLane::RealIntegration.can_initialize_real_authority());
        assert!(DiagnosticLane::PresentationFixture.network_is_denied());
    }

    #[test]
    fn fixture_rejects_real_authority_marker() {
        let root = OwnerOnlyTemporaryRoot::new("whisply-fixture").expect("secure root");
        let mut fixture = fixture(&root);
        fixture.authority = "account".to_string();

        assert!(matches!(
            fixture.validate(),
            Err(FixtureValidationError::AuthorityViolation)
        ));
    }

    #[test]
    fn force_ui_requires_the_non_themeable_fixture_marker() {
        let root = OwnerOnlyTemporaryRoot::new("whisply-fixture").expect("secure root");
        let mut request = ForceUiRequest {
            scenario: fixture(&root),
            width: 1_280,
            height: 720,
            appearance: FixtureAppearance::Dark,
            theme: FixtureTheme::System,
            presentation: FixturePresentation {
                locale: "en_US".to_string(),
                layout_direction: FixtureLayoutDirection::LeftToRight,
                font_scale: FixtureFontScale::Standard,
                reduce_motion: true,
                increased_contrast: false,
            },
            display: FixtureDisplay::default(),
            capture: FixtureCapturePoint::Initial,
            watermark: PRESENTATION_FIXTURE_MARKER.to_string(),
            fixture_thread: None,
            fixture_replay: None,
        };
        assert!(request.validate().is_ok());
        request.watermark = "normal app".to_string();
        assert!(matches!(
            request.validate(),
            Err(FixtureValidationError::InvalidPresentationRequest)
        ));
        request.watermark = PRESENTATION_FIXTURE_MARKER.to_string();
        request.presentation.locale = "en--US".to_string();
        assert!(matches!(
            request.validate(),
            Err(FixtureValidationError::InvalidPresentationRequest)
        ));
    }

    #[test]
    fn direct_static_fixture_capture_is_an_exact_reviewed_state_machine() {
        let root = OwnerOnlyTemporaryRoot::new("whisply-fixture").expect("secure root");
        let mut request = ForceUiRequest {
            scenario: fixture(&root),
            width: 1_280,
            height: 720,
            appearance: FixtureAppearance::Dark,
            theme: FixtureTheme::System,
            presentation: FixturePresentation {
                locale: "en_US".to_string(),
                layout_direction: FixtureLayoutDirection::LeftToRight,
                font_scale: FixtureFontScale::Standard,
                reduce_motion: true,
                increased_contrast: false,
            },
            display: FixtureDisplay::default(),
            capture: FixtureCapturePoint::Initial,
            watermark: PRESENTATION_FIXTURE_MARKER.to_string(),
            fixture_thread: None,
            fixture_replay: None,
        };

        assert!(request.validate().is_ok());

        request.capture = FixtureCapturePoint::Terminal;
        assert!(matches!(
            request.validate(),
            Err(FixtureValidationError::InvalidPresentationRequest)
        ));

        request.scenario.state_id = "streaming".to_string();
        request.capture = FixtureCapturePoint::NamedEvent {
            event_id: CHAT_OVERLAY_STREAMING_CAPTURE_EVENT_ID.to_string(),
        };
        assert!(request.validate().is_ok());

        request.capture = FixtureCapturePoint::NamedEvent {
            event_id: "caller-supplied-event".to_string(),
        };
        assert!(matches!(
            request.validate(),
            Err(FixtureValidationError::InvalidPresentationRequest)
        ));

        request.scenario.state_id = "needs-input".to_string();
        request.capture = FixtureCapturePoint::Terminal;
        assert!(request.validate().is_ok());

        request.scenario.surface_id = "history".to_string();
        request.scenario.state_id = "empty".to_string();
        assert!(matches!(
            request.validate(),
            Err(FixtureValidationError::InvalidPresentationRequest)
        ));
        request.capture = FixtureCapturePoint::Initial;
        assert!(request.validate().is_ok());
    }

    #[test]
    fn force_ui_rejects_unbound_or_invalid_display_controls() {
        let root = OwnerOnlyTemporaryRoot::new("whisply-fixture").expect("secure root");
        let mut request = ForceUiRequest {
            scenario: fixture(&root),
            width: 1_280,
            height: 720,
            appearance: FixtureAppearance::Dark,
            theme: FixtureTheme::System,
            presentation: FixturePresentation {
                locale: "en_US".to_string(),
                layout_direction: FixtureLayoutDirection::LeftToRight,
                font_scale: FixtureFontScale::Standard,
                reduce_motion: true,
                increased_contrast: false,
            },
            display: FixtureDisplay {
                display_identifier: None,
                backing_scale_milli: Some(2_000),
            },
            capture: FixtureCapturePoint::Initial,
            watermark: PRESENTATION_FIXTURE_MARKER.to_string(),
            fixture_thread: None,
            fixture_replay: None,
        };

        assert!(matches!(
            request.validate(),
            Err(FixtureValidationError::InvalidPresentationRequest)
        ));

        request.display = FixtureDisplay {
            display_identifier: Some(0),
            backing_scale_milli: None,
        };
        assert!(matches!(
            request.validate(),
            Err(FixtureValidationError::InvalidPresentationRequest)
        ));

        request.display = FixtureDisplay {
            display_identifier: Some(42),
            backing_scale_milli: Some(499),
        };
        assert!(matches!(
            request.validate(),
            Err(FixtureValidationError::InvalidPresentationRequest)
        ));

        request.display = FixtureDisplay {
            display_identifier: Some(42),
            backing_scale_milli: Some(2_000),
        };
        assert!(request.validate().is_ok());
    }

    #[test]
    fn session_rejects_tampered_duplicate_and_out_of_order_frames() {
        let root = OwnerOnlyTemporaryRoot::new("whisply-fixture").expect("secure root");
        let mut session = DiagnosticSessionAuthenticator::new(
            &root,
            RELEASE_HASH,
            1_000,
            1_000 + DEFAULT_FIXTURE_SESSION_TTL_MS,
            [7; 32],
        )
        .expect("session");

        let valid = session
            .sign_frame(
                1,
                1_001,
                "abcdefghijklmno_pqrstuvwxyz012345".to_string(),
                serde_json::json!({"event": "fixture_rendered"}),
            )
            .expect("frame");
        let mut tampered = valid.clone();
        tampered.payload = serde_json::json!({"event": "other"});
        assert!(matches!(
            session.verify_frame(&tampered, 1_002),
            Err(DiagnosticSecurityError::InvalidMac)
        ));
        session.verify_frame(&valid, 1_002).expect("valid frame");
        assert!(matches!(
            session.verify_frame(&valid, 1_003),
            Err(DiagnosticSecurityError::InvalidFrame)
        ));
        let out_of_order = session
            .sign_frame(
                3,
                1_004,
                "abcdefghijklmno_pqrstuvwxyz012346".to_string(),
                serde_json::json!({"event": "later"}),
            )
            .expect("frame");
        assert!(matches!(
            session.verify_frame(&out_of_order, 1_004),
            Err(DiagnosticSecurityError::InvalidFrame)
        ));
    }

    #[test]
    fn unsafe_payload_and_outside_root_fail_closed() {
        let root = OwnerOnlyTemporaryRoot::new("whisply-fixture").expect("secure root");
        let session =
            DiagnosticSessionAuthenticator::new(&root, RELEASE_HASH, 1_000, 2_000, [9; 32])
                .expect("session");
        assert!(matches!(
            session.sign_frame(
                1,
                1_001,
                "abcdefghijklmno_pqrstuvwxyz012345".to_string(),
                serde_json::json!({"access_token": "not allowed"}),
            ),
            Err(DiagnosticSecurityError::UnsafePayload)
        ));
        assert!(matches!(
            validate_path_inside_root(root.path(), Path::new("/private/tmp/not-this-root")),
            Err(DiagnosticSecurityError::PathEscape)
        ));
    }

    #[test]
    fn launch_manifest_requires_owner_root_and_bounded_expiry() {
        let root = OwnerOnlyTemporaryRoot::new("whisply-fixture").expect("secure root");
        let manifest = DiagnosticLaunchManifest {
            schema_version: DIAGNOSTIC_PROTOCOL_SCHEMA_VERSION,
            lane: DiagnosticLane::PresentationFixture,
            release_manifest_sha256: RELEASE_HASH.to_string(),
            fixture_root: root.path().to_path_buf(),
            socket_path: root.child("fixture.sock").expect("child socket"),
            expires_at_ms: 2_000,
        };
        assert!(manifest.validate(1_000).is_ok());
    }

    #[test]
    fn canonical_frame_mac_input_is_stable() {
        let frame = DiagnosticIpcFrame {
            schema_version: 1,
            session_id: "00000000-0000-4000-8000-000000000000".to_string(),
            sequence: 1,
            issued_at_ms: 1,
            nonce: "abcdefghijklmno_pqrstuvwxyz012345".to_string(),
            payload: serde_json::json!({"b": 2, "a": 1}),
            mac_base64_url: "ignored".to_string(),
        };
        assert_eq!(
            String::from_utf8(frame_mac_input(&frame).expect("canonical input"))
                .expect("utf8 canonical JSON"),
            r#"{"issuedAtMS":1,"nonce":"abcdefghijklmno_pqrstuvwxyz012345","payload":{"a":1,"b":2},"schemaVersion":1,"sequence":1,"sessionID":"00000000-0000-4000-8000-000000000000"}"#,
        );
    }
}

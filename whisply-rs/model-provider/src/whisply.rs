//! Direct Whisply hosted-provider boundary.
//!
//! The runtime never adapts a user-supplied provider entry into the Whisply
//! product provider. Gateway routing and account credentials are supplied only
//! by Whisply-owned launch and broker code.

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use codex_whisply::CatalogEnvelope;
use codex_whisply::CatalogError;
use codex_whisply::CatalogModel;
use codex_whisply::CatalogVerificationKeySet;
use codex_whisply::GatewayDescriptorSnapshot;
use codex_whisply::ManagedGatewayClient;
use codex_whisply::ManagedGatewayError;
use codex_whisply::ModelAvailability;
use codex_whisply::ModelCatalog;
use codex_whisply::SubscriptionAvailability;
use codex_whisply::WHISPLY_GENERAL_ASSISTANT_IDENTITY;
use codex_whisply::WHISPLY_RUNTIME_VERSION;
use codex_whisply::load_catalog_key_set_for_current_runtime;
use codex_whisply::managed_gateway_client_from_environment;
use codex_whisply::user_agent;
use http::HeaderMap;
use http::HeaderValue;
use tokio::time::timeout;
use uuid::Uuid;
use whisply_api::ApiError;
use whisply_api::AuthError;
use whisply_api::AuthProvider;
use whisply_api::Provider;
use whisply_api::SharedAuthProvider;
use whisply_api::TransportError;
use whisply_client::Request;
use whisply_http_client::ClientRouteClass;
use whisply_http_client::HttpClientBuilder;
use whisply_http_client::HttpClientFactory;
use whisply_login::AuthManager;
use whisply_login::CodexAuth;
use whisply_model_provider_info::ModelProviderInfo;
use whisply_model_provider_info::WireApi;
use whisply_models_manager::manager::ModelsEndpointClient;
use whisply_models_manager::manager::ModelsEndpointFuture;
use whisply_models_manager::manager::OpenAiModelsManager;
use whisply_models_manager::manager::SharedModelsManager;
use whisply_protocol::config_types::ReasoningSummary;
use whisply_protocol::error::CodexErr;
use whisply_protocol::error::Result as CoreResult;
use whisply_protocol::openai_models::ConfigShellToolType;
use whisply_protocol::openai_models::InputModality;
use whisply_protocol::openai_models::ModelAvailabilityNux;
use whisply_protocol::openai_models::ModelInfo;
use whisply_protocol::openai_models::ModelMessages;
use whisply_protocol::openai_models::ModelVisibility;
use whisply_protocol::openai_models::ModelsResponse;
use whisply_protocol::openai_models::ReasoningEffort;
use whisply_protocol::openai_models::ReasoningEffortPreset;
use whisply_protocol::openai_models::ToolMode;
use whisply_protocol::openai_models::TruncationPolicyConfig;
use whisply_protocol::openai_models::WebSearchToolType;
use whisply_protocol::protocol::MultiAgentVersion;

use crate::ModelProvider;
use crate::ModelProviderFuture;
use crate::ProviderAccountResult;
use crate::ProviderAccountState;
use crate::ProviderCapabilities;
use crate::RemoteCompactionSupport;

const WHISPLY_PROVIDER_NAME: &str = "Whisply";
const DIRECT_PROVIDER_HEADER: &str = "x-whisply-runtime";
const DIRECT_PROVIDER_VALUE: &str = "direct";
const WHISPLY_APPROVAL_REVIEW_MODEL: &str = "gpt-5.6-luna";
const MODEL_CATALOG_PATH: &str = "model-catalog";
const MODEL_CATALOG_REFRESH_TIMEOUT: Duration = Duration::from_secs(10);
const INSTALLATION_ID_FILENAME: &str = "installation_id";

/// Bounded connection establishment for the release-owned gateway. This is
/// deliberately not a request-lifetime cutoff: after dispatch, only the
/// gateway has enough Usage-ledger state to resolve cancellation or settlement.
pub const WHISPLY_GATEWAY_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Maximum period without any gateway transport activity. SSE comments count
/// as activity on the direct route, while remaining invisible to model event
/// consumers.
pub const WHISPLY_GATEWAY_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

pub use codex_whisply::ManagedGatewaySessionAvailability as WhisplyManagedSessionAvailability;

/// Reports whether this process has a complete broker-managed launch. Launch
/// descriptors are process-shared because the native launcher delivers them
/// through one-shot pipes.
pub fn whisply_managed_session_availability() -> WhisplyManagedSessionAvailability {
    ManagedGatewayClient::availability_from_environment()
}

/// Returns the non-user-configurable provider metadata installed by Whisply.
pub fn whisply_provider_info() -> ModelProviderInfo {
    ModelProviderInfo {
        name: WHISPLY_PROVIDER_NAME.to_string(),
        base_url: None,
        env_key: None,
        env_key_instructions: None,
        experimental_bearer_token: None,
        auth: None,
        aws: None,
        wire_api: WireApi::Responses,
        query_params: None,
        http_headers: Some(
            [
                (
                    DIRECT_PROVIDER_HEADER.to_string(),
                    DIRECT_PROVIDER_VALUE.to_string(),
                ),
                ("x-whisply-protocol-version".to_string(), "1".to_string()),
                (
                    "x-whisply-runtime-version".to_string(),
                    WHISPLY_RUNTIME_VERSION.to_string(),
                ),
                (
                    "user-agent".to_string(),
                    user_agent(WHISPLY_RUNTIME_VERSION),
                ),
            ]
            .into_iter()
            .collect(),
        ),
        env_http_headers: None,
        request_max_retries: Some(0),
        stream_max_retries: Some(0),
        stream_idle_timeout_ms: Some(WHISPLY_GATEWAY_IDLE_TIMEOUT.as_millis() as u64),
        websocket_connect_timeout_ms: None,
        requires_openai_auth: false,
        supports_websockets: false,
        supports_standalone_web_search: false,
    }
}

/// Returns whether provider metadata is the built-in managed Whisply route.
///
/// Checking the direct marker as well as the display name prevents a generic
/// user-configured provider from accidentally receiving the managed gateway's
/// restricted wire behavior.
pub fn is_whisply_provider(info: &ModelProviderInfo) -> bool {
    info.name == WHISPLY_PROVIDER_NAME
        && info
            .http_headers
            .as_ref()
            .and_then(|headers| headers.get(DIRECT_PROVIDER_HEADER))
            .is_some_and(|value| value == DIRECT_PROVIDER_VALUE)
}

/// Dynamic auth provider backed by the managed descriptor source. Every actual
/// HTTP request obtains a descriptor that is still valid or triggers the
/// capability-authenticated native refresh protocol before a bearer header is
/// built. `add_auth_headers` intentionally performs no I/O for telemetry.
#[derive(Clone, Debug)]
struct WhisplyBrokeredAuthProvider {
    gateway: Arc<ManagedGatewayClient>,
}

impl WhisplyBrokeredAuthProvider {
    fn add_snapshot_header(
        snapshot: &GatewayDescriptorSnapshot,
        headers: &mut HeaderMap,
    ) -> Result<(), AuthError> {
        let header = HeaderValue::from_str(&format!("Bearer {}", snapshot.bearer()))
            .map_err(|_| AuthError::Build("managed gateway descriptor is invalid".to_string()))?;
        let _ = headers.insert(http::header::AUTHORIZATION, header);
        Ok(())
    }
}

impl AuthProvider for WhisplyBrokeredAuthProvider {
    fn add_auth_headers(&self, headers: &mut HeaderMap) {
        if let Ok(snapshot) = self.gateway.snapshot() {
            let _ = Self::add_snapshot_header(&snapshot, headers);
        }
    }

    fn apply_auth(&self, request: Request) -> whisply_api::AuthProviderFuture<'_> {
        let gateway = Arc::clone(&self.gateway);
        Box::pin(async move {
            let snapshot = tokio::task::spawn_blocking(move || gateway.ensure_fresh())
                .await
                .map_err(|_| {
                    AuthError::Transient("managed gateway refresh task failed".to_string())
                })?
                .map_err(|_| {
                    AuthError::Transient("managed gateway refresh is unavailable".to_string())
                })?;
            let mut request = request;
            Self::add_snapshot_header(&snapshot, &mut request.headers)?;
            Ok(request)
        })
    }
}

/// Signed, first-party model-catalog transport for the managed Whisply
/// runtime. It intentionally carries broker state in memory only and treats a
/// missing descriptor, installation identifier, verification key, non-2xx
/// response, invalid signature, or expired catalog as an unavailable catalog.
struct WhisplyCatalogModelsEndpoint {
    gateway: Result<Option<Arc<ManagedGatewayClient>>, ManagedGatewayError>,
    codex_home: PathBuf,
    key_set: Result<CatalogVerificationKeySet, CatalogError>,
}

impl fmt::Debug for WhisplyCatalogModelsEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WhisplyCatalogModelsEndpoint")
            .field("gateway", &self.gateway.as_ref().map(|_| "managed"))
            .field("codex_home", &self.codex_home)
            .field("key_set", &self.key_set.as_ref().map(|_| "loaded"))
            .finish()
    }
}

impl WhisplyCatalogModelsEndpoint {
    fn new(
        gateway: Result<Option<Arc<ManagedGatewayClient>>, ManagedGatewayError>,
        codex_home: PathBuf,
    ) -> Self {
        Self {
            key_set: catalog_key_set_for_gateway(&gateway),
            gateway,
            codex_home,
        }
    }

    fn unavailable_catalog_error() -> CodexErr {
        CodexErr::InvalidRequest(
            "Whisply model catalog is unavailable. Restart Whisply or check your connection."
                .to_string(),
        )
    }

    async fn fresh_gateway_descriptor(&self) -> CoreResult<GatewayDescriptorSnapshot> {
        let gateway = self
            .gateway
            .as_ref()
            .map_err(|_| Self::unavailable_catalog_error())?
            .as_ref()
            .ok_or_else(Self::unavailable_catalog_error)?;
        let gateway = Arc::clone(gateway);
        tokio::task::spawn_blocking(move || gateway.ensure_fresh())
            .await
            .map_err(|_| Self::unavailable_catalog_error())?
            .map_err(|_| Self::unavailable_catalog_error())
    }

    fn installation_id(&self) -> CoreResult<String> {
        // `models_manager_without_cache` cannot return an initialization
        // error. It uses an empty path to represent a missing managed home;
        // never let that turn into a relative `installation_id` lookup in the
        // caller's current working directory.
        if self.codex_home.as_os_str().is_empty() {
            return Err(Self::unavailable_catalog_error());
        }
        let contents = std::fs::read_to_string(self.codex_home.join(INSTALLATION_ID_FILENAME))
            .map_err(|_| Self::unavailable_catalog_error())?;
        let installation_id = contents.trim();
        let installation_id =
            Uuid::parse_str(installation_id).map_err(|_| Self::unavailable_catalog_error())?;
        Ok(installation_id.to_string())
    }

    fn current_unix_seconds() -> CoreResult<i64> {
        let seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| Self::unavailable_catalog_error())?
            .as_secs();
        i64::try_from(seconds).map_err(|_| Self::unavailable_catalog_error())
    }

    async fn fetch_catalog(
        &self,
        http_client_factory: HttpClientFactory,
    ) -> CoreResult<ModelCatalog> {
        let gateway = self.fresh_gateway_descriptor().await?;
        let key_set = self
            .key_set
            .as_ref()
            .map_err(|_| Self::unavailable_catalog_error())?;
        let installation_id = self.installation_id()?;
        let request_url = format!("{}/{MODEL_CATALOG_PATH}", gateway.api_base_url());
        let request_id = Uuid::new_v4().to_string();

        let catalog_request_url = request_url.clone();
        let client = timeout(
            MODEL_CATALOG_REFRESH_TIMEOUT,
            tokio::task::spawn_blocking(move || {
                // Catalog authentication carries the same brokered session as
                // `/v1/responses`, so a redirect must never turn the
                // release-owned origin into a credential-forwarding hop.
                HttpClientBuilder::new()
                    .without_redirects()
                    .without_request_logging()
                    .connect_timeout(WHISPLY_GATEWAY_CONNECT_TIMEOUT)
                    .build_respecting_outbound_proxy_policy(
                        &http_client_factory,
                        &catalog_request_url,
                        ClientRouteClass::Api,
                    )
                    .map_err(std::io::Error::from)
            }),
        )
        .await
        .map_err(|_| Self::unavailable_catalog_error())?
        .map_err(|_| Self::unavailable_catalog_error())?
        .map_err(|_| Self::unavailable_catalog_error())?;
        let response = timeout(
            MODEL_CATALOG_REFRESH_TIMEOUT,
            client
                .get(&request_url)
                .bearer_auth(gateway.bearer())
                .header(DIRECT_PROVIDER_HEADER, DIRECT_PROVIDER_VALUE)
                .header("x-whisply-protocol-version", "1")
                .header("x-whisply-runtime-version", WHISPLY_RUNTIME_VERSION)
                .header("x-whisply-install-id", installation_id)
                .header("x-whisply-request-id", request_id)
                .header("user-agent", user_agent(WHISPLY_RUNTIME_VERSION))
                .send(),
        )
        .await
        .map_err(|_| Self::unavailable_catalog_error())?
        .map_err(|_| Self::unavailable_catalog_error())?;
        if !response.status().is_success() {
            return Err(Self::unavailable_catalog_error());
        }
        let envelope = timeout(
            MODEL_CATALOG_REFRESH_TIMEOUT,
            response.json::<CatalogEnvelope>(),
        )
        .await
        .map_err(|_| Self::unavailable_catalog_error())?
        .map_err(|_| Self::unavailable_catalog_error())?;
        envelope
            .verify_at(key_set, Self::current_unix_seconds()?)
            .map_err(|_| Self::unavailable_catalog_error())?;
        Ok(envelope.catalog)
    }

    async fn fetch_models(
        &self,
        http_client_factory: HttpClientFactory,
    ) -> CoreResult<(Vec<ModelInfo>, Option<String>)> {
        let catalog = self.fetch_catalog(http_client_factory).await?;
        let catalog_revision = catalog.catalog_revision.clone();
        let models = model_infos_from_catalog(&catalog)?;

        Ok((models, Some(catalog_revision)))
    }
}

/// Fetches one fresh, signature-verified customer model catalog through the
/// native broker descriptor. The returned catalog contains only stable product
/// identifiers and signed presentation metadata; it never exposes an upstream
/// route or reads a user-configured provider endpoint.
pub async fn fetch_verified_whisply_catalog(
    gateway: Option<Arc<ManagedGatewayClient>>,
    codex_home: PathBuf,
    http_client_factory: HttpClientFactory,
) -> CoreResult<ModelCatalog> {
    WhisplyCatalogModelsEndpoint::new(Ok(gateway), codex_home)
        .fetch_catalog(http_client_factory)
        .await
}

fn catalog_key_set_for_gateway(
    _gateway: &Result<Option<Arc<ManagedGatewayClient>>, ManagedGatewayError>,
) -> Result<CatalogVerificationKeySet, CatalogError> {
    #[cfg(feature = "test-support")]
    if let Ok(Some(gateway)) = _gateway
        && let Some(key_set) = gateway.test_catalog_key_set()
    {
        return Ok(key_set);
    }

    load_catalog_key_set_for_current_runtime()
}

impl ModelsEndpointClient for WhisplyCatalogModelsEndpoint {
    fn has_command_auth(&self) -> bool {
        false
    }

    fn requires_authoritative_catalog_refresh(&self) -> bool {
        true
    }

    fn uses_codex_backend(&self) -> ModelsEndpointFuture<'_, bool> {
        Box::pin(async { false })
    }

    fn list_models<'a>(
        &'a self,
        _client_version: &'a str,
        http_client_factory: HttpClientFactory,
    ) -> ModelsEndpointFuture<'a, CoreResult<(Vec<ModelInfo>, Option<String>)>> {
        Box::pin(self.fetch_models(http_client_factory))
    }
}

fn model_infos_from_catalog(catalog: &ModelCatalog) -> CoreResult<Vec<ModelInfo>> {
    catalog
        .validate()
        .map_err(|_| WhisplyCatalogModelsEndpoint::unavailable_catalog_error())?;

    catalog
        .models
        .iter()
        .enumerate()
        .map(|(index, model)| model_info_from_catalog_model(index, model))
        .collect()
}

fn model_info_from_catalog_model(index: usize, model: &CatalogModel) -> CoreResult<ModelInfo> {
    let input_modalities = input_modalities_from_catalog_model(model)?;
    let (default_reasoning_level, supported_reasoning_levels) =
        reasoning_efforts_from_catalog_model(model)?;
    let context_limit = i64::try_from(model.capabilities.context_limit)
        .map_err(|_| WhisplyCatalogModelsEndpoint::unavailable_catalog_error())?;
    let output_limit = i64::try_from(model.capabilities.output_limit)
        .map_err(|_| WhisplyCatalogModelsEndpoint::unavailable_catalog_error())?;
    let available = model.availability == ModelAvailability::Available
        && model.subscription_availability == SubscriptionAvailability::Available;
    let priority = if model.recommended_default {
        0
    } else {
        i32::try_from(index.saturating_add(1))
            .map_err(|_| WhisplyCatalogModelsEndpoint::unavailable_catalog_error())?
    };
    let availability_nux = if available {
        None
    } else {
        let message = match model.subscription_availability {
            SubscriptionAvailability::UpgradeRequired => {
                "This model requires a Whisply plan with access."
            }
            SubscriptionAvailability::Unavailable => {
                "This model is not available to this Whisply account."
            }
            SubscriptionAvailability::Available => match model.availability {
                ModelAvailability::TemporarilyUnavailable => {
                    "This Whisply model is temporarily unavailable."
                }
                ModelAvailability::Deprecated => "This Whisply model is no longer available.",
                ModelAvailability::Available => {
                    return Err(WhisplyCatalogModelsEndpoint::unavailable_catalog_error());
                }
            },
        };
        Some(ModelAvailabilityNux {
            message: message.to_string(),
        })
    };

    Ok(ModelInfo {
        slug: model.id.as_str().to_string(),
        display_name: model.display_name.clone(),
        description: Some(model.description.clone()),
        default_reasoning_level,
        supported_reasoning_levels,
        shell_type: if model.capabilities.supports_tool_calls {
            ConfigShellToolType::Default
        } else {
            ConfigShellToolType::Disabled
        },
        visibility: if available {
            ModelVisibility::List
        } else {
            ModelVisibility::Hide
        },
        supported_in_api: available,
        priority,
        additional_speed_tiers: Vec::new(),
        service_tiers: Vec::new(),
        default_service_tier: None,
        availability_nux,
        upgrade: None,
        model_messages: Some(ModelMessages {
            instructions_template: Some(WHISPLY_GENERAL_ASSISTANT_IDENTITY.to_string()),
            instructions_variables: None,
            approvals: None,
            collaboration_modes: None,
            auto_review: None,
            permissions: None,
            token_budget: None,
        }),
        include_skills_usage_instructions: true,
        include_plugin_usage_instructions: true,
        include_apps_usage_instructions: true,
        supports_reasoning_summary_parameter: model.capabilities.supports_safe_reasoning_summary,
        default_reasoning_summary: ReasoningSummary::Auto,
        support_verbosity: false,
        default_verbosity: None,
        apply_patch_tool_type: None,
        web_search_tool_type: WebSearchToolType::Text,
        truncation_policy: TruncationPolicyConfig::tokens(output_limit),
        supports_parallel_tool_calls: model.capabilities.supports_tool_calls,
        supports_image_detail_original: model.capabilities.supports_images,
        context_window: Some(context_limit),
        max_context_window: Some(context_limit),
        auto_compact_token_limit: Some((context_limit / 10) * 9),
        comp_hash: Some(format!("whisply:{}", model.route_revision)),
        effective_context_window_percent: 95,
        experimental_supported_tools: Vec::new(),
        input_modalities,
        used_fallback_model_metadata: false,
        supports_search_tool: false,
        use_responses_lite: true,
        auto_review_model_override: None,
        model_specialty: None,
        tool_mode: Some(ToolMode::Direct),
        // The verified catalog's tool-call capability is the existing
        // route-authority signal for whether a model may run tool-driven V2
        // collaboration. Local config catalogs never reach this projection.
        multi_agent_version: model
            .capabilities
            .supports_tool_calls
            .then_some(MultiAgentVersion::V2),
    })
}

fn input_modalities_from_catalog_model(model: &CatalogModel) -> CoreResult<Vec<InputModality>> {
    let mut input_modalities = Vec::with_capacity(model.capabilities.input_modalities.len());
    for modality in &model.capabilities.input_modalities {
        let modality = match modality.as_str() {
            "text" => InputModality::Text,
            "image" => InputModality::Image,
            "audio" => InputModality::Audio,
            _ => return Err(WhisplyCatalogModelsEndpoint::unavailable_catalog_error()),
        };
        if input_modalities.contains(&modality) {
            return Err(WhisplyCatalogModelsEndpoint::unavailable_catalog_error());
        }
        input_modalities.push(modality);
    }
    if input_modalities.is_empty()
        || model.capabilities.supports_images != input_modalities.contains(&InputModality::Image)
    {
        return Err(WhisplyCatalogModelsEndpoint::unavailable_catalog_error());
    }
    Ok(input_modalities)
}

fn reasoning_efforts_from_catalog_model(
    model: &CatalogModel,
) -> CoreResult<(Option<ReasoningEffort>, Vec<ReasoningEffortPreset>)> {
    let mut supported_reasoning_levels = Vec::with_capacity(model.allowed_reasoning_efforts.len());
    for effort_name in &model.allowed_reasoning_efforts {
        let effort = effort_name
            .parse::<ReasoningEffort>()
            .map_err(|_| WhisplyCatalogModelsEndpoint::unavailable_catalog_error())?;
        if supported_reasoning_levels
            .iter()
            .any(|preset: &ReasoningEffortPreset| preset.effort == effort)
        {
            return Err(WhisplyCatalogModelsEndpoint::unavailable_catalog_error());
        }
        supported_reasoning_levels.push(ReasoningEffortPreset {
            effort,
            description: "Available Whisply reasoning level.".to_string(),
        });
    }
    let default_reasoning_level = supported_reasoning_levels
        .iter()
        .find(|preset| preset.effort == ReasoningEffort::Medium)
        .or_else(|| supported_reasoning_levels.first())
        .map(|preset| preset.effort.clone());

    Ok((default_reasoning_level, supported_reasoning_levels))
}

/// First-party provider selected by the Whisply runtime default.
#[derive(Clone, Debug)]
pub(crate) struct WhisplyModelProvider {
    info: ModelProviderInfo,
    gateway: Result<Option<Arc<ManagedGatewayClient>>, ManagedGatewayError>,
}

impl WhisplyModelProvider {
    pub(crate) fn new(info: ModelProviderInfo) -> Self {
        let gateway = managed_gateway_client_from_environment();
        Self { info, gateway }
    }

    pub(crate) fn with_managed_gateway(
        info: ModelProviderInfo,
        gateway: Arc<ManagedGatewayClient>,
    ) -> Self {
        Self {
            info,
            gateway: Ok(Some(gateway)),
        }
    }

    #[cfg(test)]
    fn without_gateway(info: ModelProviderInfo) -> Self {
        Self {
            info,
            gateway: Ok(None),
        }
    }

    async fn fresh_gateway_descriptor(&self) -> CoreResult<GatewayDescriptorSnapshot> {
        let gateway = self
            .gateway
            .as_ref()
            .map_err(|_| {
                CodexErr::InvalidRequest(
                    "Whisply gateway is unavailable. Start Whisply from its managed app bundle."
                        .to_string(),
                )
            })?
            .as_ref()
            .ok_or_else(|| {
                CodexErr::InvalidRequest(
                    "Whisply account session is unavailable. Sign in through Whisply before starting a model request."
                        .to_string(),
                )
            })?;
        let gateway = Arc::clone(gateway);
        tokio::task::spawn_blocking(move || gateway.ensure_fresh())
            .await
            .map_err(|_| {
                CodexErr::InvalidRequest("Whisply gateway refresh is unavailable.".to_string())
            })?
            .map_err(|_| {
                CodexErr::InvalidRequest("Whisply gateway refresh is unavailable.".to_string())
            })
    }

    fn managed_gateway(&self) -> CoreResult<Arc<ManagedGatewayClient>> {
        self.gateway
            .as_ref()
            .map_err(|_| {
                CodexErr::InvalidRequest(
                    "Whisply gateway is unavailable. Start Whisply from its managed app bundle."
                        .to_string(),
                )
            })?
            .as_ref()
            .cloned()
            .ok_or_else(|| {
                CodexErr::InvalidRequest(
                    "Whisply account session is unavailable. Sign in through Whisply before starting a model request."
                        .to_string(),
                )
            })
    }
}

fn map_managed_gateway_terminal_error(error: &ApiError) -> Option<CodexErr> {
    let ApiError::Transport(TransportError::Http {
        body: Some(body), ..
    }) = error
    else {
        return None;
    };
    let payload = serde_json::from_str::<serde_json::Value>(body).ok()?;
    let error = payload.get("error")?.as_object()?;
    let code = error.get("code")?.as_str()?;
    if !matches!(
        code,
        "whisply_auth_expired"
            | "whisply_subscription_required"
            | "whisply_usage_limited"
            | "whisply_model_unavailable"
            | "whisply_refused"
            | "whisply_cancelled"
            | "whisply_stopped"
    ) {
        return None;
    }
    let message = error
        .get("message")
        .and_then(serde_json::Value::as_str)
        .filter(|message| !message.trim().is_empty())
        .unwrap_or("Whisply could not complete this request.");
    Some(CodexErr::InvalidRequest(message.to_string()))
}

impl ModelProvider for WhisplyModelProvider {
    fn info(&self) -> &ModelProviderInfo {
        &self.info
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            namespace_tools: true,
            image_generation: false,
            web_search: false,
            external_web_access: false,
            remote_compaction: RemoteCompactionSupport::Unsupported,
        }
    }

    fn approval_review_preferred_model(&self) -> &'static str {
        WHISPLY_APPROVAL_REVIEW_MODEL
    }

    fn auth_manager(&self) -> Option<Arc<AuthManager>> {
        None
    }

    fn auth(&self) -> ModelProviderFuture<'_, Option<CodexAuth>> {
        Box::pin(async { None })
    }

    fn account_state(&self) -> ProviderAccountResult {
        Ok(ProviderAccountState {
            account: None,
            requires_openai_auth: false,
        })
    }

    fn map_api_error(&self, error: ApiError) -> CodexErr {
        map_managed_gateway_terminal_error(&error)
            .unwrap_or_else(|| whisply_api::map_api_error(error))
    }

    fn api_provider(&self) -> ModelProviderFuture<'_, whisply_protocol::error::Result<Provider>> {
        Box::pin(async move {
            let descriptor = self.fresh_gateway_descriptor().await?;
            let mut info = self.info.clone();
            info.base_url = Some(descriptor.api_base_url().to_string());
            info.to_api_provider(/*auth_mode*/ None)
        })
    }

    fn api_auth(
        &self,
    ) -> ModelProviderFuture<'_, whisply_protocol::error::Result<SharedAuthProvider>> {
        Box::pin(async move {
            let gateway = self.managed_gateway()?;
            let _ = self.fresh_gateway_descriptor().await?;
            Ok(Arc::new(WhisplyBrokeredAuthProvider { gateway }) as SharedAuthProvider)
        })
    }

    fn refresh_after_unauthorized(
        &self,
    ) -> ModelProviderFuture<'_, whisply_protocol::error::Result<bool>> {
        Box::pin(async move {
            let gateway = self.managed_gateway()?;
            tokio::task::spawn_blocking(move || gateway.refresh_now())
                .await
                .map_err(|_| {
                    CodexErr::InvalidRequest("Whisply gateway refresh is unavailable.".to_string())
                })?
                .map_err(|_| {
                    CodexErr::InvalidRequest("Whisply gateway refresh is unavailable.".to_string())
                })?;
            Ok(true)
        })
    }

    fn models_manager(
        &self,
        codex_home: PathBuf,
        _config_model_catalog: Option<ModelsResponse>,
    ) -> SharedModelsManager {
        // A local config catalog must never replace the signed Whisply catalog
        // or seed an upstream model fallback. The manager begins empty and
        // becomes usable only after the authenticated gateway returns a fresh,
        // verified catalog signed by a release-pinned public key.
        Arc::new(OpenAiModelsManager::new_authoritative_without_cache(
            Arc::new(WhisplyCatalogModelsEndpoint::new(
                self.gateway.clone(),
                codex_home,
            )),
        ))
    }

    fn models_manager_without_cache(
        &self,
        _config_model_catalog: Option<ModelsResponse>,
    ) -> SharedModelsManager {
        // Callers that intentionally disable caches use the same signed,
        // authoritative catalog behavior. The managed runtime's catalog is
        // never persisted through an unscoped generic cache.
        Arc::new(OpenAiModelsManager::new_authoritative_without_cache(
            Arc::new(WhisplyCatalogModelsEndpoint::new(
                self.gateway.clone(),
                whisply_utils_home_dir::find_whisply_home()
                    .map(whisply_utils_absolute_path::AbsolutePathBuf::into_path_buf)
                    .unwrap_or_default(),
            )),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_whisply::MODEL_CATALOG_SCHEMA_VERSION;
    use codex_whisply::ModelCapabilities;
    use codex_whisply::ModelId;
    use codex_whisply::WHISPLY_PROVIDER_ID;

    fn signed_catalog_model() -> CatalogModel {
        CatalogModel {
            id: ModelId::parse("gpt-5.6-terra").expect("stable public model id"),
            display_name: "GPT 5.6 Terra".to_string(),
            short_name: "GPT Terra".to_string(),
            description: "OpenAI fast model for everyday work".to_string(),
            provider_family: "openai".to_string(),
            route_revision: "route-2026-08-07".to_string(),
            capabilities: ModelCapabilities {
                input_modalities: vec!["text".to_string(), "image".to_string()],
                output_modalities: vec!["text".to_string()],
                supports_tool_calls: true,
                supports_images: true,
                supports_structured_output: true,
                supports_safe_reasoning_summary: true,
                supports_computer_use: true,
                supports_native_visible_progress: false,
                context_limit: 1_050_000,
                output_limit: 16_384,
            },
            allowed_profiles: vec!["general".to_string()],
            allowed_reasoning_efforts: vec!["low".to_string(), "medium".to_string()],
            availability: ModelAvailability::Available,
            subscription_availability: SubscriptionAvailability::Available,
            rate_card_revision: "rate-card-2026-08".to_string(),
            usage_multiplier_millis: 1_000,
            recommended_default: true,
            response_start_timeout_seconds: 120,
            response_idle_timeout_seconds: 120,
        }
    }

    #[test]
    fn direct_provider_has_no_user_configured_credential_path() {
        let info = whisply_provider_info();

        assert!(is_whisply_provider(&info));
        assert_eq!(WHISPLY_PROVIDER_ID, "whisply");
        assert!(info.env_key.is_none());
        assert!(info.experimental_bearer_token.is_none());
        assert!(info.auth.is_none());
        assert!(!info.requires_openai_auth);
        assert_eq!(info.request_max_retries, Some(0));
        assert_eq!(info.stream_max_retries, Some(0));
        assert_eq!(
            info.stream_idle_timeout_ms,
            Some(WHISPLY_GATEWAY_IDLE_TIMEOUT.as_millis() as u64)
        );
        assert_eq!(
            WhisplyModelProvider::without_gateway(info).approval_review_preferred_model(),
            "gpt-5.6-luna"
        );
    }

    #[test]
    fn managed_terminal_http_errors_are_not_retried() {
        let provider = WhisplyModelProvider::without_gateway(whisply_provider_info());

        for code in [
            "whisply_refused",
            "whisply_cancelled",
            "whisply_stopped",
            "whisply_usage_limited",
        ] {
            let expected = format!("public {code} message");
            let error = ApiError::Transport(TransportError::Http {
                status: http::StatusCode::BAD_REQUEST,
                url: Some("https://gateway.invalid/v1/responses".to_string()),
                headers: None,
                body: Some(
                    serde_json::json!({
                        "error": {
                            "code": code,
                            "message": expected.clone(),
                            "provider_detail": "must-not-control-client-policy",
                        },
                    })
                    .to_string(),
                ),
            });

            let mapped = provider.map_api_error(error);
            assert!(!mapped.is_retryable(), "{code} should not be retried");
            assert_eq!(mapped.to_string(), expected, "{code}");
        }
    }

    #[tokio::test]
    async fn provider_fails_closed_without_the_brokered_session() {
        let provider = WhisplyModelProvider::without_gateway(whisply_provider_info());

        assert!(provider.api_provider().await.is_err());
        let error = match provider.api_auth().await {
            Ok(_) => panic!("session is required"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("Whisply account session"));
    }

    #[test]
    fn signed_catalog_projects_only_stable_public_model_metadata() {
        let catalog = ModelCatalog {
            schema_version: MODEL_CATALOG_SCHEMA_VERSION,
            catalog_revision: "catalog-2026-08-07.1".to_string(),
            generated_at_unix_seconds: 1_786_126_400,
            expires_at_unix_seconds: 1_786_212_800,
            models: vec![signed_catalog_model()],
        };

        let models = model_infos_from_catalog(&catalog).expect("valid signed catalog projection");
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].slug, "gpt-5.6-terra");
        assert_eq!(models[0].display_name, "GPT 5.6 Terra");
        assert_eq!(models[0].visibility, ModelVisibility::List);
        assert_eq!(
            models[0].default_reasoning_level,
            Some(ReasoningEffort::Medium)
        );
        assert_eq!(
            models[0].input_modalities,
            vec![InputModality::Text, InputModality::Image]
        );
        assert!(models[0].use_responses_lite);
        assert!(models[0].supports_parallel_tool_calls);
        assert_eq!(
            models[0].multi_agent_version,
            Some(MultiAgentVersion::V2),
            "the signed tool-call capability enables worker delegation"
        );
    }

    #[test]
    fn signed_catalog_without_tool_calls_does_not_enable_multi_agent_v2() {
        let mut model = signed_catalog_model();
        model.capabilities.supports_tool_calls = false;
        let catalog = ModelCatalog {
            schema_version: MODEL_CATALOG_SCHEMA_VERSION,
            catalog_revision: "catalog-2026-08-07.2".to_string(),
            generated_at_unix_seconds: 1_786_126_400,
            expires_at_unix_seconds: 1_786_212_800,
            models: vec![model],
        };

        let models = model_infos_from_catalog(&catalog).expect("valid signed catalog projection");
        assert_eq!(models[0].multi_agent_version, None);
    }

    #[test]
    fn signed_catalog_preserves_each_models_direct_wire_reasoning_subset() {
        let mut model = signed_catalog_model();
        model.allowed_reasoning_efforts =
            vec!["minimal".to_string(), "high".to_string(), "max".to_string()];
        let catalog = ModelCatalog {
            schema_version: MODEL_CATALOG_SCHEMA_VERSION,
            catalog_revision: "catalog-2026-08-08.1".to_string(),
            generated_at_unix_seconds: 1_786_126_400,
            expires_at_unix_seconds: 1_786_212_800,
            models: vec![model],
        };

        let models = model_infos_from_catalog(&catalog).expect("valid signed catalog projection");
        assert_eq!(
            models[0]
                .supported_reasoning_levels
                .iter()
                .map(|preset| preset.effort.as_str())
                .collect::<Vec<_>>(),
            vec!["minimal", "high", "max"]
        );
        assert_eq!(
            models[0].default_reasoning_level,
            Some(ReasoningEffort::Minimal)
        );
    }

    #[test]
    fn signed_catalog_rejects_lossy_ultra_reasoning_advertising() {
        let mut model = signed_catalog_model();
        model.allowed_reasoning_efforts = vec!["ultra".to_string()];
        let catalog = ModelCatalog {
            schema_version: MODEL_CATALOG_SCHEMA_VERSION,
            catalog_revision: "catalog-2026-08-08.1".to_string(),
            generated_at_unix_seconds: 1_786_126_400,
            expires_at_unix_seconds: 1_786_212_800,
            models: vec![model],
        };

        assert!(model_infos_from_catalog(&catalog).is_err());
    }

    #[test]
    fn signed_catalog_rejects_unknown_input_modalities() {
        let mut model = signed_catalog_model();
        model
            .capabilities
            .input_modalities
            .push("video".to_string());
        let catalog = ModelCatalog {
            schema_version: MODEL_CATALOG_SCHEMA_VERSION,
            catalog_revision: "catalog-2026-08-07.1".to_string(),
            generated_at_unix_seconds: 1_786_126_400,
            expires_at_unix_seconds: 1_786_212_800,
            models: vec![model],
        };

        assert!(model_infos_from_catalog(&catalog).is_err());
    }
}

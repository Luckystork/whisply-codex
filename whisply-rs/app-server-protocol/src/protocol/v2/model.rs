use super::shared::v2_enum_from_core;
use crate::JsonSchema;
use crate::TS;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value as JsonValue;
use whisply_protocol::openai_models::InputModality;
use whisply_protocol::openai_models::ModelAvailabilityNux as CoreModelAvailabilityNux;
use whisply_protocol::openai_models::ReasoningEffort;
use whisply_protocol::openai_models::default_input_modalities;
use whisply_protocol::protocol::ModelRerouteReason as CoreModelRerouteReason;
use whisply_protocol::protocol::ModelVerification as CoreModelVerification;

v2_enum_from_core!(
    pub enum ModelRerouteReason from CoreModelRerouteReason {
        HighRiskCyberActivity
    }
);

v2_enum_from_core!(
    pub enum ModelVerification from CoreModelVerification {
        TrustedAccessForCyber
    }
);

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ModelProviderCapabilitiesReadParams {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ModelProviderCapabilitiesReadResponse {
    pub namespace_tools: bool,
    pub image_generation: bool,
    pub web_search: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ModelListParams {
    /// Opaque pagination cursor returned by a previous call.
    #[ts(optional = nullable)]
    pub cursor: Option<String>,
    /// Optional page size; defaults to a reasonable server-side value.
    #[ts(optional = nullable)]
    pub limit: Option<u32>,
    /// When true, include models that are hidden from the default picker list.
    #[ts(optional = nullable)]
    pub include_hidden: Option<bool>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ModelAvailabilityNux {
    pub message: String,
}

impl From<CoreModelAvailabilityNux> for ModelAvailabilityNux {
    fn from(value: CoreModelAvailabilityNux) -> Self {
        Self {
            message: value.message,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ModelServiceTier {
    pub id: String,
    pub name: String,
    pub description: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct Model {
    pub id: String,
    pub model: String,
    pub upgrade: Option<String>,
    pub upgrade_info: Option<ModelUpgradeInfo>,
    pub availability_nux: Option<ModelAvailabilityNux>,
    pub display_name: String,
    pub description: String,
    #[serde(default)]
    pub model_specialty: Option<String>,
    pub hidden: bool,
    pub supported_reasoning_efforts: Vec<ReasoningEffortOption>,
    pub default_reasoning_effort: ReasoningEffort,
    #[serde(default = "default_input_modalities")]
    pub input_modalities: Vec<InputModality>,
    #[serde(default)]
    pub supports_personality: bool,
    /// Deprecated: use `serviceTiers` instead.
    #[serde(default)]
    pub additional_speed_tiers: Vec<String>,
    #[serde(default)]
    pub service_tiers: Vec<ModelServiceTier>,
    /// Catalog default service tier id for this model, when one is configured.
    #[serde(default)]
    pub default_service_tier: Option<String>,
    // Only one model should be marked as default.
    pub is_default: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ModelUpgradeInfo {
    pub model: String,
    pub upgrade_copy: Option<String>,
    pub model_link: Option<String>,
    pub migration_markdown: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ReasoningEffortOption {
    pub reasoning_effort: ReasoningEffort,
    pub description: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ModelListResponse {
    pub data: Vec<Model>,
    /// Opaque cursor to pass to the next call to continue after the last item.
    /// If None, there are no more items to return.
    pub next_cursor: Option<String>,
}

/// Reads the signed customer model catalog used by the Whisply native UI.
/// Unlike `model/list`, this carries the complete verified presentation and
/// capability projection needed by the native selector.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export_to = "v2/")]
pub struct WhisplyModelCatalogReadParams {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "v2/")]
pub enum WhisplyModelCatalogAvailability {
    Available,
    TemporarilyUnavailable,
    Deprecated,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "v2/")]
pub enum WhisplyModelCatalogSubscriptionAvailability {
    Available,
    UpgradeRequired,
    Unavailable,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export_to = "v2/")]
pub struct WhisplyModelCatalogCapabilities {
    pub input_modalities: Vec<String>,
    pub output_modalities: Vec<String>,
    pub supports_tool_calls: bool,
    pub supports_images: bool,
    pub supports_structured_output: bool,
    pub supports_safe_reasoning_summary: bool,
    pub supports_computer_use: bool,
    pub supports_native_visible_progress: bool,
    pub context_limit: u64,
    pub output_limit: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export_to = "v2/")]
pub struct WhisplyModelCatalogModel {
    pub id: String,
    pub display_name: String,
    pub short_name: String,
    pub description: String,
    pub provider_family: String,
    pub route_revision: String,
    pub capabilities: WhisplyModelCatalogCapabilities,
    pub allowed_profiles: Vec<String>,
    pub allowed_reasoning_efforts: Vec<String>,
    pub availability: WhisplyModelCatalogAvailability,
    pub subscription_availability: WhisplyModelCatalogSubscriptionAvailability,
    pub rate_card_revision: String,
    pub usage_multiplier_millis: u32,
    pub recommended_default: bool,
    pub response_start_timeout_seconds: u64,
    pub response_idle_timeout_seconds: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export_to = "v2/")]
pub struct WhisplyModelCatalogReadResponse {
    pub schema_version: u16,
    pub catalog_revision: String,
    pub generated_at_unix_seconds: i64,
    pub expires_at_unix_seconds: i64,
    pub models: Vec<WhisplyModelCatalogModel>,
}

/// Reads the layered prompt-composition record for a thread's most recent turn.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export_to = "v2/")]
pub struct WhisplyPromptCompositionReadParams {
    pub thread_id: String,
}

/// Which side contributed a prompt layer.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "v2/")]
pub enum WhisplyPromptLayerSource {
    NativeRuntime,
    WhisplyProduct,
}

/// Which prompt profile the composition ran under.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "v2/")]
pub enum WhisplyPromptProfile {
    General,
    Workspace,
}

/// One layer's revision-safe fingerprint.
///
/// Deliberately a digest rather than the contributed text: a client can prove
/// which prompt revision ran, and compare it across surfaces and releases,
/// without the composed instructions leaving the runtime.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export_to = "v2/")]
pub struct WhisplyPromptLayerDigest {
    pub layer_id: String,
    pub precedence: u8,
    pub source: WhisplyPromptLayerSource,
    pub origin_id: String,
    pub byte_length: u64,
    pub text_sha256: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export_to = "v2/")]
pub struct WhisplyPromptCompositionReadResponse {
    pub contract_version: u16,
    pub profile: WhisplyPromptProfile,
    pub mode_id: Option<String>,
    pub layers: Vec<WhisplyPromptLayerDigest>,
}

/// Reads what can be said about one conversation being shortened.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export_to = "v2/")]
pub struct WhisplyCompactionReadParams {
    pub thread_id: String,
}

/// Who asked for a conversation to be shortened.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "v2/")]
pub enum WhisplyCompactionTrigger {
    /// The runtime reached its budget and shortened the thread on its own.
    Automatic,
    /// A person asked for it.
    Requested,
}

/// Which mechanism shortened a conversation.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "v2/")]
pub enum WhisplyCompactionMechanism {
    /// The thread was summarized and the summary replaced its history.
    Summarized,
    /// A fresh context window was installed without a summarization turn.
    WindowReset,
}

/// How a shortening ended.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "v2/")]
pub enum WhisplyCompactionOutcome {
    Completed,
    Interrupted,
    Failed,
}

/// Which tokens count against the automatic-shortening limit.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "v2/")]
pub enum WhisplyCompactionLimitScope {
    Total,
    BodyAfterPrefix,
}

/// How close a conversation is to being shortened.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export_to = "v2/")]
pub struct WhisplyCompactionBudget {
    pub context_window_tokens: Option<i64>,
    pub auto_compact_limit_tokens: Option<i64>,
    pub limit_scope: WhisplyCompactionLimitScope,
    pub active_context_tokens: i64,
    pub scope_tokens: i64,
    pub tokens_remaining: Option<i64>,
    pub threshold_reached: bool,
    /// How much of the budget is spent, in hundredths of a percent, or absent
    /// when nothing bounds the conversation.
    pub used_basis_points: Option<u32>,
}

/// One shortening that happened to a conversation.
///
/// Deliberately a digest rather than the summary: the summary is the person's
/// conversation in compressed form, and a diagnostic surface that carried it
/// would be a way to read a chat without opening it. The digest is enough to
/// show that a resumed thread holds the summary compaction wrote.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export_to = "v2/")]
pub struct WhisplyCompactionRecord {
    pub sequence: u32,
    pub trigger: WhisplyCompactionTrigger,
    pub mechanism: WhisplyCompactionMechanism,
    pub outcome: WhisplyCompactionOutcome,
    pub active_context_tokens_before: i64,
    pub active_context_tokens_after: i64,
    pub summary_tokens: Option<i64>,
    pub summary_sha256: Option<String>,
    /// Whether the shortened history reached the thread's stored record. A
    /// shortening that was not written down is read back long on the next
    /// resume.
    pub persisted: bool,
    pub recorded_at_unix_ms: i64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export_to = "v2/")]
pub struct WhisplyCompactionReadResponse {
    pub contract_version: u16,
    /// Absent until the thread has run a turn, because until then nothing has
    /// measured the window.
    pub budget: Option<WhisplyCompactionBudget>,
    pub compaction_count: u32,
    /// The most recent shortenings, oldest first.
    pub records: Vec<WhisplyCompactionRecord>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ModelReroutedNotification {
    pub thread_id: String,
    pub turn_id: String,
    pub from_model: String,
    pub to_model: String,
    pub reason: ModelRerouteReason,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ModelVerificationNotification {
    pub thread_id: String,
    pub turn_id: String,
    pub verifications: Vec<ModelVerification>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct TurnModerationMetadataNotification {
    pub thread_id: String,
    pub turn_id: String,
    pub metadata: JsonValue,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ModelSafetyBufferingUpdatedNotification {
    pub thread_id: String,
    pub turn_id: String,
    pub model: String,
    pub use_cases: Vec<String>,
    pub reasons: Vec<String>,
    pub show_buffering_ui: bool,
    pub faster_model: Option<String>,
}

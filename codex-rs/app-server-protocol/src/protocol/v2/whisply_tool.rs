//! Canonical app-server wire contract for first-party Whisply tools.
//!
//! `arguments` is the only model-visible portion of a call. All account,
//! lease, task, confirmation, and nonce data remains in the trusted envelope.
//! These two server-initiated request shapes intentionally do not reuse the
//! generic dynamic-tool request: native and server owners must validate the
//! envelope and strictest policy before a handler can run.

use crate::JsonSchema;
use crate::TS;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value as JsonValue;

/// Closed trusted lease phases. Unknown values fail deserialization rather
/// than becoming an execution permission.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export_to = "v2/")]
pub enum WhisplyToolLeasePhase {
    Prepare,
    Execute,
    Verify,
    Commit,
    Resume,
}

/// Model-visible first-party tool call. Authority data is deliberately absent.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export_to = "v2/")]
pub struct WhisplyToolModelCall {
    #[serde(rename = "executionID")]
    pub execution_id: String,
    #[serde(rename = "toolID")]
    pub tool_id: String,
    pub schema_version: u16,
    pub arguments: JsonValue,
}

/// Trusted data that is never embedded in model-visible tool arguments.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export_to = "v2/")]
pub struct WhisplyExecutionEnvelope {
    pub account_epoch: String,
    #[serde(rename = "serverIntentGrantID")]
    pub server_intent_grant_id: Option<String>,
    #[serde(rename = "taskID")]
    pub task_id: Option<String>,
    pub task_version: Option<i64>,
    pub lease_token: Option<String>,
    pub lease_phase: Option<WhisplyToolLeasePhase>,
    #[serde(rename = "deviceID")]
    pub device_id: String,
    #[serde(rename = "installationID")]
    pub installation_id: String,
    #[serde(rename = "threadID")]
    pub thread_id: String,
    #[serde(rename = "turnID")]
    pub turn_id: String,
    #[serde(rename = "activityID")]
    pub activity_id: String,
    pub confirmation_hash: Option<String>,
    pub origin_digest: Option<String>,
    pub content_digest: Option<String>,
    #[serde(rename = "skillRunID")]
    pub skill_run_id: Option<String>,
    #[serde(rename = "expiresAtMS")]
    pub expires_at_ms: i64,
    pub nonce: String,
}

/// An independently-produced policy decision. Aggregation must only make a
/// decision stricter; a call may not use this wire object to relax policy.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export_to = "v2/")]
pub enum WhisplyToolPolicyDecision {
    Allowed,
    SetupRequired { route: String },
    ConfirmationRequired,
    Denied { reason: String },
    Unavailable { reason: String },
}

impl WhisplyToolPolicyDecision {
    /// Combines independently-issued decisions without allowing a less
    /// restrictive owner to weaken any other owner. This is shared semantics,
    /// not a model-visible policy authority.
    pub fn strictest(values: impl IntoIterator<Item = Self>) -> Self {
        values
            .into_iter()
            .max_by_key(Self::severity)
            .unwrap_or(Self::Allowed)
    }

    const fn severity(&self) -> u8 {
        match self {
            Self::Allowed => 0,
            Self::ConfirmationRequired => 1,
            Self::SetupRequired { .. } => 2,
            Self::Unavailable { .. } => 3,
            Self::Denied { .. } => 4,
        }
    }
}

/// Per-owner policy inputs for exactly one execution.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export_to = "v2/")]
pub struct WhisplyToolPolicyInput {
    pub upstream_sandbox_decision: WhisplyToolPolicyDecision,
    pub subscription_decision: WhisplyToolPolicyDecision,
    pub connector_decision: WhisplyToolPolicyDecision,
    pub native_permission_decision: WhisplyToolPolicyDecision,
    pub target_or_file_decision: WhisplyToolPolicyDecision,
    pub mode_decision: WhisplyToolPolicyDecision,
    pub server_decision: WhisplyToolPolicyDecision,
    pub final_confirmation_decision: WhisplyToolPolicyDecision,
}

impl Default for WhisplyToolPolicyInput {
    fn default() -> Self {
        Self {
            upstream_sandbox_decision: WhisplyToolPolicyDecision::Allowed,
            subscription_decision: WhisplyToolPolicyDecision::Allowed,
            connector_decision: WhisplyToolPolicyDecision::Allowed,
            native_permission_decision: WhisplyToolPolicyDecision::Allowed,
            target_or_file_decision: WhisplyToolPolicyDecision::Allowed,
            mode_decision: WhisplyToolPolicyDecision::Allowed,
            server_decision: WhisplyToolPolicyDecision::Allowed,
            final_confirmation_decision: WhisplyToolPolicyDecision::ConfirmationRequired,
        }
    }
}

impl WhisplyToolPolicyInput {
    /// Calculates the strictest value for each independently-owned policy
    /// input. The runtime subsequently applies descriptor-specific policy.
    pub fn strictest(inputs: impl IntoIterator<Item = Self>) -> Self {
        let inputs = inputs.into_iter().collect::<Vec<_>>();
        if inputs.is_empty() {
            return Self::default();
        }
        Self {
            upstream_sandbox_decision: WhisplyToolPolicyDecision::strictest(
                inputs
                    .iter()
                    .map(|input| input.upstream_sandbox_decision.clone()),
            ),
            subscription_decision: WhisplyToolPolicyDecision::strictest(
                inputs
                    .iter()
                    .map(|input| input.subscription_decision.clone()),
            ),
            connector_decision: WhisplyToolPolicyDecision::strictest(
                inputs.iter().map(|input| input.connector_decision.clone()),
            ),
            native_permission_decision: WhisplyToolPolicyDecision::strictest(
                inputs
                    .iter()
                    .map(|input| input.native_permission_decision.clone()),
            ),
            target_or_file_decision: WhisplyToolPolicyDecision::strictest(
                inputs
                    .iter()
                    .map(|input| input.target_or_file_decision.clone()),
            ),
            mode_decision: WhisplyToolPolicyDecision::strictest(
                inputs.iter().map(|input| input.mode_decision.clone()),
            ),
            server_decision: WhisplyToolPolicyDecision::strictest(
                inputs.iter().map(|input| input.server_decision.clone()),
            ),
            final_confirmation_decision: WhisplyToolPolicyDecision::strictest(
                inputs
                    .iter()
                    .map(|input| input.final_confirmation_decision.clone()),
            ),
        }
    }
}

/// Terminal result delivered through the normal typed item/event path.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export_to = "v2/")]
pub struct WhisplyToolResult {
    #[serde(rename = "executionID")]
    pub execution_id: String,
    pub status: WhisplyToolTerminalStatus,
    pub content: Option<JsonValue>,
    pub safe_summary: String,
    pub setup_route: Option<String>,
    #[serde(rename = "receiptID")]
    pub receipt_id: Option<String>,
}

/// Terminal execution states. Setup and confirmation are suspension points,
/// not successful authority escalation.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "v2/")]
pub enum WhisplyToolTerminalStatus {
    Succeeded,
    Failed,
    Cancelled,
    SetupRequired,
    ConfirmationRequired,
    Unavailable,
}

/// Progress delivered through the normal typed item/event path.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export_to = "v2/")]
pub struct WhisplyToolProgress {
    #[serde(rename = "executionID")]
    pub execution_id: String,
    pub label: String,
    pub fraction: Option<f64>,
}

/// Trusted server-to-native execution request for `whisply/tool/execute`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export_to = "v2/")]
pub struct WhisplyToolAppServerExecuteRequest {
    pub call: WhisplyToolModelCall,
    pub envelope: WhisplyExecutionEnvelope,
    pub policy_inputs: WhisplyToolPolicyInput,
}

/// Trusted cancellation request for `whisply/tool/cancel`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export_to = "v2/")]
pub struct WhisplyToolAppServerCancelRequest {
    #[serde(rename = "executionID")]
    pub execution_id: String,
    pub envelope: WhisplyExecutionEnvelope,
}

/// Typed cancellation acknowledgement. It never contains handler secrets or
/// authority data.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export_to = "v2/")]
pub struct WhisplyToolAppServerCancelAcknowledgement {
    #[serde(rename = "executionID")]
    pub execution_id: String,
    pub accepted: bool,
}

/// The only first-party native tool request method names. These are distinct
/// from the generic dynamic-tool protocol and are intentionally shared with
/// the runtime so no second wire vocabulary can emerge.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhisplyToolAppServerRequestMethod {
    #[serde(rename = "whisply/tool/execute")]
    Execute,
    #[serde(rename = "whisply/tool/cancel")]
    Cancel,
}

impl WhisplyToolAppServerRequestMethod {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Execute => "whisply/tool/execute",
            Self::Cancel => "whisply/tool/cancel",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn wire_preserves_native_initialisms_and_closed_lease_phase() {
        let value = serde_json::json!({
            "accountEpoch": "5ce47c6b-387a-4f92-9dd9-0bdb1082bc07",
            "serverIntentGrantID": null,
            "taskID": null,
            "taskVersion": null,
            "leaseToken": null,
            "leasePhase": "execute",
            "deviceID": "device_20260808_0001",
            "installationID": "install_20260808_0001",
            "threadID": "thread_20260808_0001",
            "turnID": "turn_20260808_0001",
            "activityID": "activity_20260808_0001",
            "confirmationHash": null,
            "originDigest": null,
            "contentDigest": null,
            "skillRunID": null,
            "expiresAtMS": 1786126460000i64,
            "nonce": "nonce_20260808_0001"
        });
        let envelope: WhisplyExecutionEnvelope =
            serde_json::from_value(value.clone()).expect("native envelope");
        assert_eq!(serde_json::to_value(envelope).expect("serialize"), value);

        let invalid = serde_json::json!({
            "accountEpoch": "5ce47c6b-387a-4f92-9dd9-0bdb1082bc07",
            "serverIntentGrantID": null,
            "taskID": null,
            "taskVersion": null,
            "leaseToken": null,
            "leasePhase": "expand",
            "deviceID": "device_20260808_0001",
            "installationID": "install_20260808_0001",
            "threadID": "thread_20260808_0001",
            "turnID": "turn_20260808_0001",
            "activityID": "activity_20260808_0001",
            "confirmationHash": null,
            "originDigest": null,
            "contentDigest": null,
            "skillRunID": null,
            "expiresAtMS": 1786126460000i64,
            "nonce": "nonce_20260808_0001"
        });
        assert!(serde_json::from_value::<WhisplyExecutionEnvelope>(invalid).is_err());
    }
}

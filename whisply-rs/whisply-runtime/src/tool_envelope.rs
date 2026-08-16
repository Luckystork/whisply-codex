//! Trusted first-party tool call contracts shared with the Mac tool broker.
//!
//! Model-visible `WhisplyToolModelCall.arguments` carry only descriptor schema
//! values. Account epoch, task lease, confirmation, origin/content bindings,
//! and expiry are a separate trusted envelope that cannot be model-authored.

pub use codex_app_server_protocol::WhisplyExecutionEnvelope;
pub use codex_app_server_protocol::WhisplyToolAppServerCancelAcknowledgement;
pub use codex_app_server_protocol::WhisplyToolAppServerCancelRequest;
pub use codex_app_server_protocol::WhisplyToolAppServerExecuteRequest;
pub use codex_app_server_protocol::WhisplyToolAppServerRequestMethod;
pub use codex_app_server_protocol::WhisplyToolLeasePhase;
pub use codex_app_server_protocol::WhisplyToolModelCall;
pub use codex_app_server_protocol::WhisplyToolPolicyDecision;
pub use codex_app_server_protocol::WhisplyToolPolicyInput;
pub use codex_app_server_protocol::WhisplyToolProgress;
pub use codex_app_server_protocol::WhisplyToolResult;
pub use codex_app_server_protocol::WhisplyToolTerminalStatus;
use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

use crate::ActionClass;
use crate::ToolDescriptor;

/// Current outer app-server tool contract version.
pub const TOOL_EXECUTION_ENVELOPE_SCHEMA_VERSION: u16 = 1;
/// Maximum permitted remaining lifetime for an envelope, in milliseconds.
pub const MAX_TOOL_ENVELOPE_LIFETIME_MS: i64 = 300_000;

/// Compatibility alias for runtime call sites that previously used the
/// generic name. Its JSON wire representation remains the native envelope.
pub type ToolExecutionEnvelope = WhisplyExecutionEnvelope;

/// Compiles shared policy inputs against the runtime-owned static descriptor.
/// The app-server protocol owns the wire type and strictest aggregation;
/// descriptor action class remains runtime-owned because it never comes from a
/// remote model call.
pub fn compile_policy_for(
    policy_inputs: &WhisplyToolPolicyInput,
    descriptor: &ToolDescriptor,
) -> WhisplyToolPolicyDecision {
    let mut decisions = vec![
        policy_inputs.upstream_sandbox_decision.clone(),
        policy_inputs.subscription_decision.clone(),
        policy_inputs.connector_decision.clone(),
        policy_inputs.native_permission_decision.clone(),
        policy_inputs.target_or_file_decision.clone(),
        policy_inputs.mode_decision.clone(),
        policy_inputs.server_decision.clone(),
    ];
    match descriptor.action_class {
        ActionClass::Automatic => {}
        ActionClass::FinalConfirmationRequired => {
            decisions.push(policy_inputs.final_confirmation_decision.clone());
        }
        ActionClass::Unavailable => {
            decisions.push(WhisplyToolPolicyDecision::Unavailable {
                reason: "This tool is unavailable by product policy.".to_string(),
            });
        }
    }
    WhisplyToolPolicyDecision::strictest(decisions)
}

/// Owner verification is separate from static checking and must validate the
/// native/server MAC, nonce sequence, peer identity, and issuer binding.
pub trait TrustedEnvelopeVerifier {
    fn verify(&self, envelope: &WhisplyExecutionEnvelope) -> Result<(), ToolEnvelopeError>;
}

/// Validates an execute request before dispatch. This checks descriptor/model
/// schema, trusted envelope, strictest policy, and owner verification in the
/// required fail-closed order.
pub fn validate_execute_request<V: TrustedEnvelopeVerifier>(
    request: &WhisplyToolAppServerExecuteRequest,
    descriptor: &ToolDescriptor,
    now_unix_ms: i64,
    verifier: &V,
) -> Result<WhisplyToolPolicyDecision, ToolEnvelopeError> {
    if request.call.tool_id != descriptor.id
        || request.call.schema_version != descriptor.schema_version
        || validate_opaque_id(&request.call.execution_id).is_err()
    {
        return Err(ToolEnvelopeError::InvalidCall);
    }
    validate_arguments(&request.call.arguments, &descriptor.input_schema)?;
    validate_envelope_static(&request.envelope, descriptor, now_unix_ms)?;
    verifier.verify(&request.envelope)?;
    Ok(compile_policy_for(&request.policy_inputs, descriptor))
}

/// Validates an owner result before it is made model-visible. In particular,
/// trusted screen bytes and unredacted file material cannot escape through a
/// descriptor that only declares a safe summary output.
pub fn validate_result(
    result: &WhisplyToolResult,
    call: &WhisplyToolModelCall,
    descriptor: &ToolDescriptor,
) -> Result<(), ToolEnvelopeError> {
    if result.execution_id != call.execution_id || validate_opaque_id(&result.execution_id).is_err()
    {
        return Err(ToolEnvelopeError::InvalidResult);
    }
    validate_summary(&result.safe_summary)?;
    match result.status {
        WhisplyToolTerminalStatus::Succeeded => {
            let content = result
                .content
                .as_ref()
                .ok_or(ToolEnvelopeError::InvalidResult)?;
            validate_arguments(content, &descriptor.output_schema)?;
            if result.setup_route.is_some() {
                return Err(ToolEnvelopeError::InvalidResult);
            }
        }
        WhisplyToolTerminalStatus::SetupRequired => {
            let route = result
                .setup_route
                .as_deref()
                .ok_or(ToolEnvelopeError::InvalidResult)?;
            validate_setup_route(route)?;
            if result.content.is_some() {
                return Err(ToolEnvelopeError::InvalidResult);
            }
        }
        WhisplyToolTerminalStatus::Failed
        | WhisplyToolTerminalStatus::Cancelled
        | WhisplyToolTerminalStatus::ConfirmationRequired
        | WhisplyToolTerminalStatus::Unavailable => {
            if result.content.is_some() || result.setup_route.is_some() {
                return Err(ToolEnvelopeError::InvalidResult);
            }
        }
    }
    if let Some(receipt_id) = result.receipt_id.as_deref() {
        validate_opaque_id(receipt_id)?;
    }
    Ok(())
}

/// Validates a progress event before it enters regular item/event transport.
pub fn validate_progress(progress: &WhisplyToolProgress) -> Result<(), ToolEnvelopeError> {
    validate_opaque_id(&progress.thread_id)?;
    validate_opaque_id(&progress.turn_id)?;
    validate_opaque_id(&progress.execution_id)?;
    validate_summary(&progress.label)?;
    if progress
        .fraction
        .is_some_and(|fraction| !fraction.is_finite() || !(0.0..=1.0).contains(&fraction))
    {
        return Err(ToolEnvelopeError::InvalidResult);
    }
    Ok(())
}

/// Static envelope validation before owner MAC/nonce verification.
pub fn validate_envelope_static(
    envelope: &WhisplyExecutionEnvelope,
    descriptor: &ToolDescriptor,
    now_unix_ms: i64,
) -> Result<(), ToolEnvelopeError> {
    Uuid::parse_str(&envelope.account_epoch).map_err(|_| ToolEnvelopeError::InvalidEnvelope)?;
    if envelope.expires_at_ms <= now_unix_ms
        || envelope.expires_at_ms.saturating_sub(now_unix_ms) > MAX_TOOL_ENVELOPE_LIFETIME_MS
        || envelope.task_version.is_some_and(|version| version < 0)
        || envelope.lease_token.is_some() != envelope.lease_phase.is_some()
    {
        return Err(ToolEnvelopeError::InvalidEnvelope);
    }
    for value in [
        &envelope.device_id,
        &envelope.installation_id,
        &envelope.thread_id,
        &envelope.turn_id,
        &envelope.activity_id,
        &envelope.nonce,
    ] {
        validate_opaque_id(value)?;
    }
    for value in [
        envelope.server_intent_grant_id.as_deref(),
        envelope.task_id.as_deref(),
        envelope.lease_token.as_deref(),
        envelope.confirmation_hash.as_deref(),
        envelope.origin_digest.as_deref(),
        envelope.content_digest.as_deref(),
        envelope.skill_run_id.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        validate_opaque_id(value)?;
    }
    if descriptor.requires_target_selection && envelope.server_intent_grant_id.is_none() {
        return Err(ToolEnvelopeError::TargetBindingRequired);
    }
    match descriptor.action_class {
        ActionClass::Automatic => {
            if envelope.confirmation_hash.is_some() {
                return Err(ToolEnvelopeError::UnexpectedConfirmation);
            }
        }
        ActionClass::FinalConfirmationRequired => {
            if envelope.confirmation_hash.is_none() {
                return Err(ToolEnvelopeError::FinalConfirmationRequired);
            }
        }
        ActionClass::Unavailable => return Err(ToolEnvelopeError::ToolUnavailable),
    }
    Ok(())
}

fn validate_arguments(arguments: &Value, schema: &Value) -> Result<(), ToolEnvelopeError> {
    let arguments = arguments
        .as_object()
        .ok_or(ToolEnvelopeError::InvalidCall)?;
    let schema = schema.as_object().ok_or(ToolEnvelopeError::InvalidCall)?;
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .ok_or(ToolEnvelopeError::InvalidCall)?;
    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        for name in required {
            let name = name.as_str().ok_or(ToolEnvelopeError::InvalidCall)?;
            if !arguments.contains_key(name) {
                return Err(ToolEnvelopeError::InvalidCall);
            }
        }
    }
    for (name, value) in arguments {
        let property = properties.get(name).ok_or(ToolEnvelopeError::InvalidCall)?;
        let expected_type = property
            .get("type")
            .and_then(Value::as_str)
            .ok_or(ToolEnvelopeError::InvalidCall)?;
        let type_matches = match expected_type {
            "string" => value.is_string(),
            "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
            "number" => value.is_number(),
            "object" => value.is_object(),
            "array" => value.is_array(),
            "boolean" => value.is_boolean(),
            "null" => value.is_null(),
            _ => false,
        };
        if !type_matches {
            return Err(ToolEnvelopeError::InvalidCall);
        }
        if let Some(values) = property.get("enum").and_then(Value::as_array)
            && !values.contains(value)
        {
            return Err(ToolEnvelopeError::InvalidCall);
        }
    }
    Ok(())
}

fn validate_summary(value: &str) -> Result<(), ToolEnvelopeError> {
    if value.trim().is_empty() || value.len() > 4_096 {
        Err(ToolEnvelopeError::InvalidResult)
    } else {
        Ok(())
    }
}

fn validate_opaque_id(value: &str) -> Result<(), ToolEnvelopeError> {
    let valid = (8..=256).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'));
    if valid {
        Ok(())
    } else {
        Err(ToolEnvelopeError::InvalidEnvelope)
    }
}

/// Setup routes are either an opaque owner route or a bounded local Settings
/// deep link emitted by the native Mac owner. The latter is deliberately not
/// a general URL: no host, path, query, fragment, or percent-encoding is
/// accepted after the fixed scheme.
fn validate_setup_route(value: &str) -> Result<(), ToolEnvelopeError> {
    if validate_opaque_id(value).is_ok() {
        return Ok(());
    }

    let Some(setting) = value.strip_prefix("settings://") else {
        return Err(ToolEnvelopeError::InvalidResult);
    };
    let valid = (1..=128).contains(&setting.len())
        && setting
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    if valid {
        Ok(())
    } else {
        Err(ToolEnvelopeError::InvalidResult)
    }
}

/// Turns a validated setup route into the instruction a user can act on.
///
/// Kept beside `validate_setup_route` on purpose: the accepted shapes and the
/// rendered text have to move together, or a route shape added later renders as
/// something meaningless and nobody notices until a user is stuck.
///
/// The Settings pane name is derived from the slug rather than looked up in a
/// table. A table is a promise to update it every time the Mac owner adds a
/// pane, and the failure mode of forgetting is telling the user to open a pane
/// that does not exist. Deriving is occasionally less polished and never wrong.
///
/// Returns `None` for an opaque owner route: it is a token, not a place, and
/// reading it aloud to a user would be noise dressed as instruction.
pub fn describe_setup_route(route: &str) -> Option<String> {
    let setting = route.strip_prefix("settings://")?;
    if validate_setup_route(route).is_err() {
        return None;
    }

    let pane = setting
        .split('-')
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut characters = word.chars();
            match characters.next() {
                Some(first) => first.to_ascii_uppercase().to_string() + characters.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    (!pane.is_empty()).then(|| format!("Whisply Settings > {pane}"))
}

/// Fail-closed errors shared by native/server tool boundaries.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ToolEnvelopeError {
    #[error("the Whisply tool call is malformed or does not match its descriptor")]
    InvalidCall,
    #[error("the Whisply trusted execution envelope is malformed or expired")]
    InvalidEnvelope,
    #[error("the Whisply tool requires an owner-bound target intent")]
    TargetBindingRequired,
    #[error("the Whisply tool requires final user confirmation")]
    FinalConfirmationRequired,
    #[error("the Whisply tool must not carry final confirmation")]
    UnexpectedConfirmation,
    #[error("the Whisply tool is currently unavailable")]
    ToolUnavailable,
    #[error("the Whisply trusted execution envelope signature did not validate")]
    InvalidSignature,
    #[error("the Whisply tool result is malformed or exposes undeclared data")]
    InvalidResult,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ActionClass;
    use crate::ToolRegistryArtifactAvailability;
    use crate::first_party_tool_registry;
    use pretty_assertions::assert_eq;
    use serde::Deserialize;

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct ToolLifecycleFixture {
        schema_version: u16,
        lifecycle_revision: String,
        descriptor: ToolLifecycleDescriptor,
        scenarios: Vec<ToolLifecycleScenario>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct ToolLifecycleDescriptor {
        id: String,
        availability: ToolRegistryArtifactAvailability,
        action_class: ActionClass,
        supports_cancellation: bool,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct ToolLifecycleScenario {
        id: String,
        policy: WhisplyToolPolicyDecision,
        result: WhisplyToolResult,
    }

    struct AcceptingVerifier;

    impl TrustedEnvelopeVerifier for AcceptingVerifier {
        fn verify(&self, _: &WhisplyExecutionEnvelope) -> Result<(), ToolEnvelopeError> {
            Ok(())
        }
    }

    fn envelope() -> WhisplyExecutionEnvelope {
        WhisplyExecutionEnvelope {
            account_epoch: "5ce47c6b-387a-4f92-9dd9-0bdb1082bc07".to_string(),
            server_intent_grant_id: Some("intent_20260808_0001".to_string()),
            task_id: Some("task_20260808_0001".to_string()),
            task_version: Some(1),
            lease_token: Some("lease_20260808_0001".to_string()),
            lease_phase: Some(WhisplyToolLeasePhase::Execute),
            device_id: "device_20260808_0001".to_string(),
            installation_id: "install_20260808_0001".to_string(),
            thread_id: "thread_20260808_0001".to_string(),
            turn_id: "turn_20260808_0001".to_string(),
            activity_id: "activity_20260808_0001".to_string(),
            confirmation_hash: Some("confirmation_20260808_0001".to_string()),
            origin_digest: Some("origin_20260808_0001".to_string()),
            content_digest: Some("content_20260808_0001".to_string()),
            skill_run_id: None,
            expires_at_ms: 1_786_126_460_000,
            nonce: "nonce_20260808_0001".to_string(),
        }
    }

    fn call(descriptor: &ToolDescriptor) -> WhisplyToolModelCall {
        WhisplyToolModelCall {
            execution_id: "execution_20260808_0001".to_string(),
            tool_id: descriptor.id.clone(),
            schema_version: descriptor.schema_version,
            arguments: serde_json::json!({"action": "click", "targetID": "target_20260808_0001"}),
        }
    }

    #[test]
    fn final_action_cannot_bypass_owner_confirmation() {
        let registry = first_party_tool_registry();
        let descriptor = registry
            .get("whisply.computer_use")
            .expect("computer-use descriptor");
        let mut envelope = envelope();
        envelope.confirmation_hash = None;

        assert_eq!(
            validate_envelope_static(&envelope, descriptor, 1_786_126_401_000),
            Err(ToolEnvelopeError::FinalConfirmationRequired)
        );
    }

    #[test]
    fn execute_request_keeps_authority_out_of_model_arguments() {
        let registry = first_party_tool_registry();
        let descriptor = registry
            .get("whisply.computer_use")
            .expect("computer-use descriptor");
        let request = WhisplyToolAppServerExecuteRequest {
            call: call(descriptor),
            envelope: envelope(),
            policy_inputs: WhisplyToolPolicyInput {
                final_confirmation_decision: WhisplyToolPolicyDecision::Allowed,
                ..WhisplyToolPolicyInput::default()
            },
        };

        assert_eq!(
            validate_execute_request(&request, descriptor, 1_786_126_401_000, &AcceptingVerifier),
            Ok(WhisplyToolPolicyDecision::Allowed)
        );
        let value = serde_json::to_value(&request.call).expect("model call");
        assert!(value.get("accountEpoch").is_none());
        assert!(value.get("leaseToken").is_none());
    }

    #[test]
    fn unknown_lease_phase_fails_closed() {
        let mut value = serde_json::to_value(envelope()).expect("envelope");
        value["leasePhase"] = serde_json::json!("expand");
        assert!(serde_json::from_value::<WhisplyExecutionEnvelope>(value).is_err());
    }

    #[test]
    fn policy_decision_uses_the_shared_snake_case_wire_tag() {
        let value = serde_json::to_value(WhisplyToolPolicyDecision::SetupRequired {
            route: "connector_setup_20260808".to_string(),
        })
        .expect("policy decision");
        assert_eq!(value["kind"], "setup_required");
        assert!(
            serde_json::from_value::<WhisplyToolPolicyDecision>(serde_json::json!({
                "kind": "setupRequired",
                "route": "connector_setup_20260808"
            }))
            .is_err()
        );
    }

    #[test]
    fn wire_uses_the_native_initialism_field_names() {
        let value = serde_json::to_value(envelope()).expect("envelope");
        for name in [
            "serverIntentGrantID",
            "taskID",
            "deviceID",
            "installationID",
            "threadID",
            "turnID",
            "activityID",
            "skillRunID",
            "expiresAtMS",
        ] {
            assert!(value.get(name).is_some(), "missing native field {name}");
        }
        for incorrect_name in [
            "serverIntentGrantId",
            "taskId",
            "deviceId",
            "installationId",
            "threadId",
            "turnId",
            "activityId",
            "skillRunId",
            "expiresAtMs",
        ] {
            assert!(
                value.get(incorrect_name).is_none(),
                "non-native field {incorrect_name} leaked"
            );
        }

        let call = WhisplyToolModelCall {
            execution_id: "execution_20260808_0001".to_string(),
            tool_id: "whisply.screen.context".to_string(),
            schema_version: 1,
            arguments: serde_json::json!({"scope": "exact_window"}),
        };
        let call_value = serde_json::to_value(call).expect("call");
        assert!(call_value.get("executionID").is_some());
        assert!(call_value.get("toolID").is_some());
        assert!(call_value.get("executionId").is_none());
        assert!(call_value.get("toolId").is_none());
    }

    #[test]
    fn results_cannot_bypass_descriptor_output_schema() {
        let registry = first_party_tool_registry();
        let descriptor = registry
            .get("whisply.computer_use")
            .expect("computer-use descriptor");
        let result = WhisplyToolResult {
            execution_id: "execution_20260808_0001".to_string(),
            status: WhisplyToolTerminalStatus::Succeeded,
            content: Some(serde_json::json!({"rawScreenshot": "not declared"})),
            safe_summary: "Completed the approved action.".to_string(),
            setup_route: None,
            receipt_id: None,
        };

        assert_eq!(
            validate_result(&result, &call(descriptor), descriptor),
            Err(ToolEnvelopeError::InvalidCall)
        );
    }

    #[test]
    fn shared_tool_lifecycle_fixture_matches_the_runtime_contract() {
        let fixture: ToolLifecycleFixture = serde_json::from_str(include_str!(
            "../../../../contracts/fixtures/whisply-tool-lifecycle-v1.json"
        ))
        .expect("shared tool lifecycle fixture");
        assert_eq!(fixture.schema_version, 1);
        assert_eq!(fixture.lifecycle_revision, "tool-lifecycle-2026-08-11.1");

        let registry = first_party_tool_registry();
        let descriptor = registry
            .get(&fixture.descriptor.id)
            .expect("fixture descriptor in runtime registry");
        let artifact = registry
            .artifact_descriptor(&fixture.descriptor.id)
            .expect("fixture descriptor in packaged registry artifact");
        assert_eq!(artifact.availability, fixture.descriptor.availability);
        assert_eq!(artifact.action_class, fixture.descriptor.action_class);
        assert_eq!(
            artifact.supports_cancellation,
            fixture.descriptor.supports_cancellation
        );

        let expected_scenarios = [
            ("setup_required", WhisplyToolTerminalStatus::SetupRequired),
            (
                "confirmation_required",
                WhisplyToolTerminalStatus::ConfirmationRequired,
            ),
            ("cancelled", WhisplyToolTerminalStatus::Cancelled),
            (
                "succeeded_with_receipt",
                WhisplyToolTerminalStatus::Succeeded,
            ),
        ];
        assert_eq!(fixture.scenarios.len(), expected_scenarios.len());
        for (scenario, (expected_id, expected_status)) in
            fixture.scenarios.iter().zip(expected_scenarios)
        {
            assert_eq!(scenario.id, expected_id);
            assert_eq!(scenario.result.status, expected_status);
            let call = WhisplyToolModelCall {
                execution_id: scenario.result.execution_id.clone(),
                tool_id: descriptor.id.clone(),
                schema_version: descriptor.schema_version,
                arguments: serde_json::json!({
                    "action": "click",
                    "targetID": "target_20260811_0001",
                }),
            };
            assert_eq!(
                validate_result(&scenario.result, &call, descriptor),
                Ok(()),
                "fixture scenario {} must remain model-safe",
                scenario.id
            );
        }

        let setup = &fixture.scenarios[0];
        assert_eq!(
            setup.policy,
            WhisplyToolPolicyDecision::SetupRequired {
                route: "settings://screen-recording".to_string(),
            }
        );
        assert_eq!(
            setup.result.setup_route.as_deref(),
            Some("settings://screen-recording")
        );
        assert_eq!(
            fixture.scenarios[1].policy,
            WhisplyToolPolicyDecision::ConfirmationRequired
        );
        assert!(fixture.scenarios[2].result.content.is_none());
        assert!(fixture.scenarios[2].result.receipt_id.is_none());
        assert_eq!(
            fixture.scenarios[3].result.receipt_id.as_deref(),
            Some("receipt_20260811_0001")
        );

        let mut unsafe_setup = setup.result.clone();
        unsafe_setup.setup_route = Some("settings://screen-recording?unreviewed=true".to_string());
        let unsafe_call = WhisplyToolModelCall {
            execution_id: unsafe_setup.execution_id.clone(),
            tool_id: descriptor.id.clone(),
            schema_version: descriptor.schema_version,
            arguments: serde_json::json!({
                "action": "click",
                "targetID": "target_20260811_0001",
            }),
        };
        assert_eq!(
            validate_result(&unsafe_setup, &unsafe_call, descriptor),
            Err(ToolEnvelopeError::InvalidResult)
        );
    }

    /// The routes the Mac owner actually emits today.
    #[test]
    fn the_shipped_routes_name_a_pane_a_user_can_find() {
        assert_eq!(
            describe_setup_route("settings://screen-recording").as_deref(),
            Some("Whisply Settings > Screen Recording")
        );
        assert_eq!(
            describe_setup_route("settings://file-access").as_deref(),
            Some("Whisply Settings > File Access")
        );
    }

    /// Derived rather than looked up, so a pane added by the Mac owner next
    /// month renders sensibly instead of falling off a table nobody updated.
    #[test]
    fn a_route_added_later_still_renders_without_anyone_updating_a_table() {
        assert_eq!(
            describe_setup_route("settings://microphone").as_deref(),
            Some("Whisply Settings > Microphone")
        );
        assert_eq!(
            describe_setup_route("settings://accessibility-control").as_deref(),
            Some("Whisply Settings > Accessibility Control")
        );
    }

    #[test]
    fn an_opaque_owner_route_has_no_instruction_to_give() {
        // It is a token, not a place. "Open owner-route-abcdefgh" is noise
        // wearing the shape of help.
        assert_eq!(describe_setup_route("owner-route-abcdefgh"), None);
    }

    /// The renderer must not accept what the validator rejects, or a route that
    /// cannot legally appear in a result would still be read out to a user.
    #[test]
    fn the_renderer_refuses_anything_the_validator_refuses() {
        for rejected in [
            "settings://screen-recording?unreviewed=true",
            "settings://Screen-Recording",
            "settings://",
            "https://example.invalid/settings",
        ] {
            assert!(validate_setup_route(rejected).is_err(), "{rejected}");
            assert_eq!(describe_setup_route(rejected), None, "{rejected}");
        }
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct PolicyOutcomeFixture {
        schema_version: u32,
        fixture_revision: String,
        decision_scenarios: Vec<PolicyDecisionScenario>,
        input_merge_scenarios: Vec<PolicyInputMergeScenario>,
        compile_scenarios: Vec<PolicyCompileScenario>,
    }

    #[derive(Deserialize)]
    struct PolicyDecisionScenario {
        id: String,
        decisions: Vec<WhisplyToolPolicyDecision>,
        expected: WhisplyToolPolicyDecision,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct PolicyInputMergeScenario {
        id: String,
        runtime: WhisplyToolPolicyInput,
        local: WhisplyToolPolicyInput,
        expected_compiled: WhisplyToolPolicyDecision,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct PolicyCompileScenario {
        id: String,
        tool_id: String,
        policy_inputs: WhisplyToolPolicyInput,
        expected: WhisplyToolPolicyDecision,
    }

    #[test]
    fn shared_tool_policy_outcome_fixture_matches_runtime_compiler() {
        let fixture: PolicyOutcomeFixture = serde_json::from_str(include_str!(
            "../../../../contracts/fixtures/whisply-tool-policy-outcomes-v1.json"
        ))
        .expect("shared tool policy outcome fixture");
        assert_eq!(fixture.schema_version, 1);
        assert_eq!(
            fixture.fixture_revision,
            "tool-policy-outcomes-2026-08-14.1"
        );

        let registry = first_party_tool_registry();
        for scenario in &fixture.decision_scenarios {
            assert_eq!(
                WhisplyToolPolicyDecision::strictest(scenario.decisions.clone()),
                scenario.expected,
                "decision scenario {}",
                scenario.id
            );
        }

        for scenario in &fixture.input_merge_scenarios {
            let merged = WhisplyToolPolicyInput::strictest([
                scenario.runtime.clone(),
                scenario.local.clone(),
            ]);
            let descriptor = registry
                .get("whisply.files")
                .expect("files descriptor for merge scenarios");
            assert_eq!(
                compile_policy_for(&merged, descriptor),
                scenario.expected_compiled,
                "input merge scenario {}",
                scenario.id
            );
        }

        for scenario in &fixture.compile_scenarios {
            let descriptor = registry
                .get(&scenario.tool_id)
                .expect("compile scenario descriptor");
            assert_eq!(
                compile_policy_for(&scenario.policy_inputs, descriptor),
                scenario.expected,
                "compile scenario {}",
                scenario.id
            );
        }
    }
}

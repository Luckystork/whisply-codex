//! One Browser-owned structured sample through the existing model client.
//! There is no agent/session loop, tool router, MCP, directory hydration or
//! history persistence in this path. The native controller owns all actions.

use crate::client::ModelClient;
use crate::client_common::{Prompt, ResponseEvent};
use crate::config::Config;
use crate::responses_metadata::{CodexResponsesMetadata, CodexResponsesRequestKind};
use crate::thread_manager::ThreadManager;

pub struct BrowserSampleResult {
    pub output_json: String,
    pub receipt_id: String,
}
use codex_whisply::BROWSER_STRUCTURED_OUTPUT_BYTES;
use codex_whisply::ValidatedBrowserSamplingRequest;
use futures::StreamExt;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use whisply_login::auth::AgentIdentityAuthPolicy;
use whisply_otel::SessionTelemetry;
use whisply_protocol::ThreadId;
use whisply_protocol::config_types::ReasoningSummary as ReasoningSummaryConfig;
use whisply_protocol::error::CodexErr;
use whisply_protocol::error::Result;
use whisply_protocol::models::{BaseInstructions, ContentItem, MessagePhase, ResponseItem};
use whisply_rollout_trace::InferenceTraceContext;

pub async fn sample_browser_step(
    manager: &ThreadManager,
    config: &Config,
    request: Arc<ValidatedBrowserSamplingRequest>,
    signed_output_limit: u64,
    cancellation: CancellationToken,
) -> Result<BrowserSampleResult> {
    let payload = request.payload();
    if u64::from(payload.maximum_output_tokens) != signed_output_limit
        || !request.proof().is_current()
    {
        return Err(invalid_browser_request());
    }
    let mut model = manager
        .get_models_manager()
        .get_model_info(&payload.model_id, &config.to_models_manager_config())
        .await;
    let default_effort = model
        .default_reasoning_level
        .as_ref()
        .map(serde_json::to_value)
        .transpose()
        .map_err(|_| invalid_browser_request())?;
    if model.used_fallback_model_metadata
        || model.slug != payload.model_id
        || default_effort.as_ref().and_then(Value::as_str) != payload.reasoning_effort.as_deref()
    {
        return Err(invalid_browser_request());
    }
    // Standard Responses has one explicit instruction field and one input
    // message. Lite would synthesize additional_tools/developer input items.
    model.use_responses_lite = false;
    model.support_verbosity = false;
    let thread_id =
        ThreadId::from_string(request.proof().task_id()).map_err(|_| invalid_browser_request())?;
    let source = manager.session_source();
    let client = ModelClient::new_with_model_provider(
        manager.model_provider_for_config(config),
        AgentIdentityAuthPolicy::JwtOnly,
        thread_id,
        source.clone(),
        "whisply-browser".to_string(),
        None,
        false,
        false,
        None,
        false,
        None,
        config.http_client_factory(),
    );
    let mut session = client.new_browser_session(Arc::clone(&request))?;
    let telemetry = SessionTelemetry::new(
        thread_id,
        &model.slug,
        &model.slug,
        None,
        None,
        None,
        "whisply-browser".to_string(),
        false,
        "native-browser".to_string(),
        source,
    );
    let mut metadata = CodexResponsesMetadata::new(
        request.proof().installation_id().to_string(),
        request.proof().task_id().to_string(),
        request.proof().task_id().to_string(),
        request.proof().request_id().to_string(),
    );
    metadata.turn_id = Some(request.proof().request_id().to_string());
    metadata.request_kind = Some(CodexResponsesRequestKind::Turn);
    let mut prompt = Prompt::default();
    prompt.base_instructions = BaseInstructions {
        text: payload.system_prompt.clone(),
    };
    prompt.input = browser_input(&request);
    prompt.output_schema = Some(
        serde_json::from_str(&payload.output_schema_json).map_err(|_| invalid_browser_request())?,
    );
    prompt.output_schema_strict = true;
    // Hard assertion at the client boundary as well: no instruction-only
    // promise can substitute for an empty tool list and no tool executor.
    if !prompt.tools.is_empty() {
        return Err(invalid_browser_request());
    }
    let remaining = request
        .proof()
        .expires_at_ms()
        .saturating_sub(chrono::Utc::now().timestamp_millis());
    if remaining <= 0 {
        return Err(CodexErr::TurnAborted);
    }
    let deadline = tokio::time::Instant::now() + Duration::from_millis(remaining as u64);
    let trace = InferenceTraceContext::disabled();
    let mut stream = tokio::select! {
        _ = cancellation.cancelled() => return Err(CodexErr::TurnAborted),
        _ = tokio::time::sleep_until(deadline) => return Err(CodexErr::TurnAborted),
        result = session.stream(&prompt, &model, &telemetry, model.default_reasoning_level.clone(),
            ReasoningSummaryConfig::None, None, &metadata, &trace) => result?,
    };
    let mut completed_text: Option<String> = None;
    let mut completed_item_id: Option<String> = None;
    let mut streamed_bytes = 0usize;
    loop {
        let event = tokio::select! {
            _ = cancellation.cancelled() => return Err(CodexErr::TurnAborted),
            _ = tokio::time::sleep_until(deadline) => return Err(CodexErr::TurnAborted),
            event = stream.next() => event.ok_or_else(invalid_browser_request)??,
        };
        match event {
            ResponseEvent::OutputTextDelta(text) => {
                streamed_bytes = streamed_bytes.saturating_add(text.len());
                if streamed_bytes > BROWSER_STRUCTURED_OUTPUT_BYTES {
                    return Err(invalid_browser_request());
                }
            }
            ResponseEvent::OutputItemDone(ResponseItem::Message {
                id,
                role,
                content,
                phase,
                ..
            }) => {
                if role != "assistant" || phase == Some(MessagePhase::Commentary) {
                    continue;
                }
                let mut text = String::new();
                for item in content {
                    match item {
                        ContentItem::OutputText { text: value } => text.push_str(&value),
                        _ => return Err(invalid_browser_request()),
                    }
                }
                if text.is_empty() || text.len() > BROWSER_STRUCTURED_OUTPUT_BYTES {
                    return Err(invalid_browser_request());
                }
                let item_id = id.map(|id| id.to_string());
                if let Some(previous) = completed_text.as_ref() {
                    if previous != &text || item_id != completed_item_id {
                        return Err(invalid_browser_request());
                    }
                } else {
                    completed_item_id = item_id;
                    completed_text = Some(text);
                }
            }
            ResponseEvent::OutputItemAdded(item) | ResponseEvent::OutputItemDone(item) => {
                if !matches!(
                    item,
                    ResponseItem::Message { .. } | ResponseItem::Reasoning { .. }
                ) {
                    return Err(invalid_browser_request());
                }
            }
            ResponseEvent::ToolCallInputDelta { .. } => return Err(invalid_browser_request()),
            ResponseEvent::Completed {
                token_usage,
                end_turn,
                whisply_browser_receipt,
                ..
            } => {
                if token_usage.is_none() || end_turn == Some(false) || !request.proof().is_current()
                {
                    return Err(invalid_browser_request());
                }
                let receipt = whisply_browser_receipt.ok_or_else(unverified_browser_receipt)?;
                if receipt.status != "settled"
                    || receipt.request_id != request.proof().request_id()
                    || receipt.component_id != request.proof().component_id()
                    || !uuid::Uuid::parse_str(&receipt.receipt_id)
                        .is_ok_and(|id| !id.is_nil() && id.to_string() == receipt.receipt_id)
                {
                    return Err(unverified_browser_receipt());
                }
                let text = completed_text.ok_or_else(invalid_browser_request)?;
                if !serde_json::from_str::<Value>(&text).is_ok_and(|value| value.is_object()) {
                    return Err(invalid_browser_request());
                }
                return Ok(BrowserSampleResult {
                    output_json: text,
                    receipt_id: receipt.receipt_id,
                });
            }
            _ => {}
        }
    }
}

fn unverified_browser_receipt() -> CodexErr {
    CodexErr::InvalidRequest(
        "The Browser step ended before its Usage receipt could be verified.".to_string(),
    )
}

pub(crate) fn browser_input(request: &ValidatedBrowserSamplingRequest) -> Vec<ResponseItem> {
    let payload = request.payload();
    let mut content = vec![ContentItem::InputText {
        text: payload.input_json.clone(),
    }];
    if let Some(image_url) = payload.visual_jpeg_data_url.as_ref() {
        content.push(ContentItem::InputImage {
            image_url: image_url.clone(),
            detail: None,
        });
    }
    vec![ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content,
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }]
}

pub(crate) fn invalid_browser_request() -> CodexErr {
    CodexErr::InvalidRequest(
        "Whisply could not validate the Browser planner request or its complete structured answer."
            .to_string(),
    )
}

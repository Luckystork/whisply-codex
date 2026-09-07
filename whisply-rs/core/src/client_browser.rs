//! Browser-only constraints applied after normal ModelClient serialization.
use super::*;

impl ModelClientSession {
    pub(super) fn browser_request_body(
        &self,
        request: &ResponsesApiRequest,
        prompt: &Prompt,
        browser: &codex_whisply::ValidatedBrowserSamplingRequest,
        metadata: &CodexResponsesMetadata,
    ) -> Result<serde_json::Value> {
        let invalid = crate::browser_sampling::invalid_browser_request;
        let payload = browser.payload();
        let proof = browser.proof();
        let schema: serde_json::Value =
            serde_json::from_str(&payload.output_schema_json).map_err(|_| invalid())?;
        if !self.client.is_whisply_direct_provider()
            || !proof.is_current()
            || !prompt.tools.is_empty()
            || prompt.parallel_tool_calls
            || request.model != payload.model_id
            || request.instructions != payload.system_prompt
            || request.input != crate::browser_sampling::browser_input(browser)
            || prompt.output_schema.as_ref() != Some(&schema)
            || !prompt.output_schema_strict
            || request.parallel_tool_calls
            || request.store
            || !request.stream
            || request.tool_choice != "auto"
            || request.service_tier.is_some()
            || request.stream_options.is_some()
            || request.prompt_cache_key.as_deref() != Some(proof.task_id())
            || request.include != ["reasoning.encrypted_content"]
            || metadata.installation_id != proof.installation_id()
            || metadata.session_id != proof.task_id()
            || metadata.thread_id != proof.task_id()
            || metadata.turn_id.as_deref() != Some(proof.request_id())
            || metadata.window_id != proof.request_id()
            || metadata.exam_turn_reference.is_some()
            || metadata.parent_thread_id.is_some()
            || metadata.parent_turn_id.is_some()
            || !metadata.workspaces.is_empty()
            || !metadata.extra.is_empty()
            || metadata.code_mode_tool_names.is_some()
            || !matches!(
                metadata.request_kind,
                Some(crate::responses_metadata::CodexResponsesRequestKind::Turn)
            )
            || metadata.forked_from_thread_id.is_some()
            || metadata.subagent_header.is_some()
            || metadata.subagent_kind.is_some()
            || metadata.thread_source.is_some()
            || metadata.sandbox.is_some()
            || metadata.turn_started_at_unix_ms.is_some()
            || request.client_metadata != metadata.whisply_client_metadata()
        {
            return Err(invalid());
        }
        let mut body = serde_json::to_value(request).map_err(|_| invalid())?;
        if !body
            .get("tools")
            .is_some_and(|tools| tools.as_array().is_some_and(Vec::is_empty))
        {
            return Err(invalid());
        }
        let reasoning = body
            .get("reasoning")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(invalid)?;
        if reasoning
            .keys()
            .any(|key| !["effort", "summary", "context"].contains(&key.as_str()))
            || reasoning
                .get("summary")
                .is_some_and(|value| !value.is_null())
            || reasoning
                .get("context")
                .is_some_and(|value| !value.is_null())
            || reasoning.get("effort").and_then(serde_json::Value::as_str)
                != payload.reasoning_effort.as_deref()
        {
            return Err(invalid());
        }
        let expected_text = serde_json::json!({"format": {
            "type": "json_schema", "name": "codex_output_schema", "schema": schema, "strict": true
        }});
        if body.get("text") != Some(&expected_text) {
            return Err(invalid());
        }
        // The ordinary SDK request has no per-call output ceiling. This exact
        // bound belongs only to the already admitted Browser component.
        body["max_output_tokens"] = payload.maximum_output_tokens.into();
        Ok(body)
    }

    pub(super) fn add_browser_request_headers(
        &self,
        headers: &mut ApiHeaderMap,
        browser: &codex_whisply::ValidatedBrowserSamplingRequest,
    ) -> Result<()> {
        let proof = browser.proof();
        if !proof.is_current() {
            return Err(crate::browser_sampling::invalid_browser_request());
        }
        let purpose = match browser.payload().purpose {
            codex_app_server_protocol::WhisplyBrowserSamplingPurpose::Planning => "planning",
            codex_app_server_protocol::WhisplyBrowserSamplingPurpose::VisualVerification => {
                "visual_verification"
            }
        };
        for (name, value) in [
            // An older gateway rejects this before any ordinary admission.
            // Only the task-child Browser route may accept this wire shape.
            ("x-whisply-protocol-version", "1-browser-sampling"),
            ("x-whisply-request-id", proof.request_id()),
            ("x-whisply-contextual-task-id", proof.task_id()),
            ("x-whisply-contextual-execution-lease", proof.lease_token()),
            ("x-whisply-browser-component-id", proof.component_id()),
            ("x-whisply-browser-payload-digest", proof.payload_digest()),
            ("x-whisply-browser-purpose", purpose),
        ] {
            let mut value = HeaderValue::from_str(value)
                .map_err(|_| crate::browser_sampling::invalid_browser_request())?;
            if name == "x-whisply-contextual-execution-lease" {
                value.set_sensitive(true);
            }
            headers.insert(name, value);
        }
        Ok(())
    }
}

use crate::RuntimeBrowserStepProof;
use base64::Engine as _;
use codex_app_server_protocol::WhisplyBrowserSamplingPayload;
use codex_app_server_protocol::WhisplyBrowserSamplingPurpose;
use serde_json::Value;
use sha2::Digest;
use sha2::Sha256;
use thiserror::Error;

pub const BROWSER_STRUCTURED_OUTPUT_BYTES: usize = 64_000;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum BrowserSamplingContractError {
    #[error("The Browser request could not be validated.")]
    InvalidPayload,
    #[error("This Browser step is no longer available.")]
    InvalidProof,
}

/// Constructible only from a current broker proof and its exact immutable
/// model payload. Neither the proof nor the payload is exposed through Debug.
pub struct ValidatedBrowserSamplingRequest {
    payload: WhisplyBrowserSamplingPayload,
    proof: RuntimeBrowserStepProof,
}

impl std::fmt::Debug for ValidatedBrowserSamplingRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ValidatedBrowserSamplingRequest([REDACTED])")
    }
}

impl ValidatedBrowserSamplingRequest {
    pub fn new(
        payload: WhisplyBrowserSamplingPayload,
        proof: RuntimeBrowserStepProof,
    ) -> Result<Self, BrowserSamplingContractError> {
        if !proof.is_current()
            || browser_sampling_payload_digest(&payload, proof.request_id(), proof.task_id())?
                != proof.payload_digest()
        {
            return Err(BrowserSamplingContractError::InvalidProof);
        }
        Ok(Self { payload, proof })
    }
    pub fn payload(&self) -> &WhisplyBrowserSamplingPayload {
        &self.payload
    }
    pub fn proof(&self) -> &RuntimeBrowserStepProof {
        &self.proof
    }
}

pub fn browser_sampling_payload_digest(
    payload: &WhisplyBrowserSamplingPayload,
    request_id: &str,
    task_id: &str,
) -> Result<String, BrowserSamplingContractError> {
    validate_browser_sampling_payload(payload)?;
    for id in [request_id, task_id] {
        if !uuid::Uuid::parse_str(id).is_ok_and(|value| !value.is_nil() && value.to_string() == id)
        {
            return Err(BrowserSamplingContractError::InvalidPayload);
        }
    }
    let purpose = match payload.purpose {
        WhisplyBrowserSamplingPurpose::Planning => "planning",
        WhisplyBrowserSamplingPurpose::VisualVerification => "visual_verification",
    };
    let maximum_output = payload.maximum_output_tokens.to_string();
    let fields = [
        "1",
        request_id,
        task_id,
        purpose,
        &payload.model_id,
        &maximum_output,
        payload.reasoning_effort.as_deref().unwrap_or(""),
        &payload.system_prompt,
        &payload.input_json,
        &payload.output_schema_json,
        payload.visual_jpeg_data_url.as_deref().unwrap_or(""),
    ];
    let mut digest = Sha256::new();
    for field in fields {
        let count =
            u32::try_from(field.len()).map_err(|_| BrowserSamplingContractError::InvalidPayload)?;
        digest.update(count.to_be_bytes());
        digest.update(field.as_bytes());
    }
    Ok(format!("{:x}", digest.finalize()))
}

pub fn validate_browser_sampling_payload(
    payload: &WhisplyBrowserSamplingPayload,
) -> Result<(), BrowserSamplingContractError> {
    let valid_effort = payload.reasoning_effort.as_deref().is_none_or(|effort| {
        [
            "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
        ]
        .contains(&effort)
    });
    if payload.model_id.is_empty()
        || payload.model_id.len() > 128
        || !payload.model_id.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"-._".contains(&byte)
        })
        || !(1..=100_000).contains(&payload.maximum_output_tokens)
        || !valid_effort
        || payload.system_prompt.is_empty()
        || payload.system_prompt.len() > 32_768
        || payload.input_json.is_empty()
        || payload.input_json.len() > 1_048_576
        || payload.output_schema_json.len() > 48_000
        || !serde_json::from_str::<Value>(&payload.input_json).is_ok_and(|value| value.is_object())
    {
        return Err(BrowserSamplingContractError::InvalidPayload);
    }
    let schema: Value = serde_json::from_str(&payload.output_schema_json)
        .map_err(|_| BrowserSamplingContractError::InvalidPayload)?;
    if !schema.is_object() || canonical_browser_schema_json(&schema)? != payload.output_schema_json
    {
        return Err(BrowserSamplingContractError::InvalidPayload);
    }
    match payload.visual_jpeg_data_url.as_deref() {
        Some(value) => {
            let encoded = value
                .strip_prefix("data:image/jpeg;base64,")
                .ok_or(BrowserSamplingContractError::InvalidPayload)?;
            if value.len() > 700_000
                || !base64::engine::general_purpose::STANDARD
                    .decode(encoded)
                    .is_ok_and(|bytes| !bytes.is_empty())
            {
                return Err(BrowserSamplingContractError::InvalidPayload);
            }
        }
        None if payload.purpose == WhisplyBrowserSamplingPurpose::VisualVerification => {
            return Err(BrowserSamplingContractError::InvalidPayload);
        }
        None => {}
    }
    Ok(())
}

pub fn canonical_browser_schema_json(
    value: &Value,
) -> Result<String, BrowserSamplingContractError> {
    match value {
        Value::Null => Ok("null".to_string()),
        Value::Bool(value) => Ok(value.to_string()),
        Value::String(value) => {
            serde_json::to_string(value).map_err(|_| BrowserSamplingContractError::InvalidPayload)
        }
        Value::Number(value) => {
            let n = value
                .as_f64()
                .ok_or(BrowserSamplingContractError::InvalidPayload)?;
            if !n.is_finite() || n.trunc() != n || n.abs() > 9_007_199_254_740_991.0 {
                return Err(BrowserSamplingContractError::InvalidPayload);
            }
            Ok((n as i64).to_string())
        }
        Value::Array(values) => Ok(format!(
            "[{}]",
            values
                .iter()
                .map(canonical_browser_schema_json)
                .collect::<Result<Vec<_>, _>>()?
                .join(",")
        )),
        Value::Object(values) => {
            let mut keys = values.keys().collect::<Vec<_>>();
            keys.sort_unstable();
            let mut fields = Vec::with_capacity(keys.len());
            for key in keys {
                if key.is_empty() || key.len() > 128 || !key.is_ascii() {
                    return Err(BrowserSamplingContractError::InvalidPayload);
                }
                let encoded_key = serde_json::to_string(key)
                    .map_err(|_| BrowserSamplingContractError::InvalidPayload)?;
                fields.push(format!(
                    "{}:{}",
                    encoded_key,
                    canonical_browser_schema_json(&values[key])?
                ));
            }
            Ok(format!("{{{}}}", fields.join(",")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vector() -> Value {
        serde_json::from_str(include_str!(
            "../fixtures/browser-runtime-sampling-v1-vector.json"
        ))
        .expect("fixture")
    }

    #[test]
    fn browser_digest_matches_cross_language_utf8_vector() {
        let vector = vector();
        let payload = serde_json::from_value(vector["payload"].clone()).expect("payload");
        let digest = browser_sampling_payload_digest(
            &payload,
            vector["requestID"].as_str().unwrap(),
            vector["taskID"].as_str().unwrap(),
        )
        .expect("digest");
        assert_eq!(
            digest,
            "cc1622dbe05445bac198c12bd9a9b23a0a6f184cb6c8d0a4a1c170b74d3ae481"
        );
        assert_eq!(digest, vector["sha256"].as_str().unwrap());
        assert!(!format!("{payload:?}").contains("café"));
    }

    #[test]
    fn browser_payload_rejects_extra_model_input_or_authority_fields() {
        for field in [
            "tools",
            "messages",
            "instructions",
            "additionalContext",
            "executionLeaseToken",
            "accountID",
            "taskID",
        ] {
            let mut payload = vector()["payload"].clone();
            payload[field] = serde_json::json!("unadmitted extra content");
            assert!(
                serde_json::from_value::<WhisplyBrowserSamplingPayload>(payload).is_err(),
                "{field}"
            );
        }
    }

    #[test]
    fn browser_schema_has_one_canonical_integer_boolean_encoding() {
        assert_eq!(
            canonical_browser_schema_json(
                &serde_json::json!({"z":true,"a":[1,2],"url":"https://example.invalid/"})
            )
            .unwrap(),
            r#"{"a":[1,2],"url":"https://example.invalid/","z":true}"#
        );
        assert!(canonical_browser_schema_json(&serde_json::json!({"minimum":1.5})).is_err());
        assert!(canonical_browser_schema_json(&serde_json::json!({"café":true})).is_err());
    }

    #[test]
    fn browser_request_digest_binds_every_field() {
        let vector = vector();
        let expected = vector["sha256"].as_str().unwrap();
        let request = vector["requestID"].as_str().unwrap();
        let task = vector["taskID"].as_str().unwrap();
        for (key, value) in [
            ("modelID", serde_json::json!("fable-5.1")),
            ("maximumOutputTokens", serde_json::json!(8192)),
            ("reasoningEffort", serde_json::json!("medium")),
            ("systemPrompt", serde_json::json!("Different instructions")),
            ("inputJSON", serde_json::json!("{}")),
            (
                "outputSchemaJSON",
                serde_json::json!("{\"type\":\"object\"}"),
            ),
            (
                "visualJPEGDataURL",
                serde_json::json!("data:image/jpeg;base64,/9j/2Q=="),
            ),
        ] {
            let mut payload = vector["payload"].clone();
            payload[key] = value;
            let payload = serde_json::from_value(payload).unwrap();
            assert_ne!(
                browser_sampling_payload_digest(&payload, request, task).unwrap(),
                expected,
                "{key}"
            );
        }
        let payload = serde_json::from_value(vector["payload"].clone()).unwrap();
        assert_ne!(
            browser_sampling_payload_digest(&payload, task, request).unwrap(),
            expected
        );
    }

    #[test]
    fn browser_input_size_images_and_schema_are_checked_before_sampling() {
        for (key, value) in [
            ("inputJSON", serde_json::json!("[]")),
            ("inputJSON", serde_json::json!("x".repeat(1_048_577))),
            ("systemPrompt", serde_json::json!("")),
            (
                "outputSchemaJSON",
                serde_json::json!("{ \"type\": \"object\" }"),
            ),
            (
                "visualJPEGDataURL",
                serde_json::json!("https://example.invalid/image.jpg"),
            ),
            (
                "visualJPEGDataURL",
                serde_json::json!("data:image/jpeg;base64,not base64"),
            ),
            ("purpose", serde_json::json!("visual_verification")),
        ] {
            let mut payload = vector()["payload"].clone();
            payload[key] = value;
            let payload = serde_json::from_value(payload).unwrap();
            assert!(
                validate_browser_sampling_payload(&payload).is_err(),
                "{key}"
            );
        }
    }
}

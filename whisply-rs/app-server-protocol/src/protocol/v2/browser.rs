use crate::JsonSchema;
use crate::TS;
use serde::Deserialize;
use serde::Serialize;

/// Browser-only model material. No task, account, lease or native command
/// authority can be supplied through this payload.
#[derive(Serialize, Deserialize, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export_to = "v2/")]
pub struct WhisplyBrowserSamplingPayload {
    pub purpose: WhisplyBrowserSamplingPurpose,
    #[serde(rename = "modelID")]
    pub model_id: String,
    pub maximum_output_tokens: u32,
    pub reasoning_effort: Option<String>,
    pub system_prompt: String,
    #[serde(rename = "inputJSON")]
    pub input_json: String,
    #[serde(rename = "outputSchemaJSON")]
    pub output_schema_json: String,
    #[serde(rename = "visualJPEGDataURL")]
    pub visual_jpeg_data_url: Option<String>,
}

impl std::fmt::Debug for WhisplyBrowserSamplingPayload {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("WhisplyBrowserSamplingPayload([REDACTED])")
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "v2/")]
pub enum WhisplyBrowserSamplingPurpose {
    Planning,
    VisualVerification,
}

/// The reference is resolved through the existing native broker for this
/// runtime process. A matching digest is required before model sampling.
#[derive(Serialize, Deserialize, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export_to = "v2/")]
pub struct WhisplyBrowserSampleParams {
    pub reference: String,
    pub payload: WhisplyBrowserSamplingPayload,
}

impl std::fmt::Debug for WhisplyBrowserSampleParams {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("WhisplyBrowserSampleParams([REDACTED])")
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export_to = "v2/")]
pub struct WhisplyBrowserSampleResponse {
    pub request_id: String,
    pub component_id: String,
    pub model_id: String,
    #[serde(rename = "outputJSON")]
    pub output_json: String,
    pub receipt: WhisplyBrowserSampleReceipt,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export_to = "v2/")]
pub struct WhisplyBrowserSampleReceipt {
    pub status: WhisplyBrowserReceiptStatus,
    pub receipt_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "v2/")]
pub enum WhisplyBrowserReceiptStatus {
    Settled,
}

impl std::fmt::Debug for WhisplyBrowserSampleResponse {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("WhisplyBrowserSampleResponse([REDACTED])")
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export_to = "v2/")]
pub struct WhisplyBrowserCancelParams {
    pub reference: String,
}

impl std::fmt::Debug for WhisplyBrowserCancelParams {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("WhisplyBrowserCancelParams([REDACTED])")
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export_to = "v2/")]
pub struct WhisplyBrowserCancelResponse {
    pub accepted: bool,
}

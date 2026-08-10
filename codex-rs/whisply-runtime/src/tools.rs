//! Canonical first-party Whisply tool registry shared with the Mac broker.
//!
//! The registry declares only stable descriptors and model-visible schemas.
//! It never claims that a local/native/server handler is available; handler
//! availability is a separate, owner-authenticated runtime state.

use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use sha2::Digest;
use sha2::Sha256;
use thiserror::Error;

/// The currently supported generated registry wire schema.
pub const TOOL_REGISTRY_SCHEMA_VERSION: u16 = 1;

/// The authority boundary that owns a tool's execution.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToolOwner {
    Native,
    Server,
    LocalRuntime,
}

/// Whisply's fixed action class. Model output cannot lower this value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionClass {
    Automatic,
    FinalConfirmationRequired,
    Unavailable,
}

/// Dynamic availability comes from a handler owner, never from the static
/// descriptor. It is intentionally separate so registry refresh cannot claim
/// a permission or setup state that has not been independently verified.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ToolAvailability {
    Ready,
    SetupRequired { route: String },
    TemporarilyUnavailable { reason: String },
    PolicyBlocked { reason: String },
}

/// A generated-source descriptor for one first-party capability. Field names
/// and types match `WhisplyToolDescriptor` in the native contract exactly.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolDescriptor {
    pub id: String,
    pub schema_version: u16,
    pub display_name: String,
    pub short_description: String,
    #[serde(rename = "iconID")]
    pub icon_id: String,
    pub owner: ToolOwner,
    pub input_schema: Value,
    pub output_schema: Value,
    pub supports_progress: bool,
    pub supports_cancellation: bool,
    pub timeout_seconds: f64,
    pub retry_limit: u16,
    pub action_class: ActionClass,
    pub requires_target_selection: bool,
    pub requires_file_approval: bool,
    pub requires_connector_scope: bool,
    pub requires_native_permission: bool,
    pub has_usage_impact: bool,
    pub redaction_class: String,
    pub minimum_runtime_version: String,
    pub minimum_app_version: String,
}

/// The immutable direct-runtime registry.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolRegistry {
    descriptors: Vec<ToolDescriptor>,
}

/// Server/native/CLI/UI projection of the immutable first-party registry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolRegistrySnapshot {
    pub schema_version: u16,
    pub registry_revision: String,
    pub descriptors: Vec<ToolDescriptor>,
    pub registry_sha256: String,
}

impl ToolRegistry {
    /// Validates the source-of-truth registry before exposing it to a client.
    pub fn new(descriptors: Vec<ToolDescriptor>) -> Result<Self, ToolRegistryError> {
        let mut ids = std::collections::HashSet::with_capacity(descriptors.len());
        for descriptor in &descriptors {
            let valid = !descriptor.id.trim().is_empty()
                && descriptor.schema_version == TOOL_REGISTRY_SCHEMA_VERSION
                && !descriptor.display_name.trim().is_empty()
                && !descriptor.short_description.trim().is_empty()
                && !descriptor.icon_id.trim().is_empty()
                && descriptor.timeout_seconds.is_finite()
                && descriptor.timeout_seconds > 0.0
                && !descriptor.redaction_class.trim().is_empty()
                && !descriptor.minimum_runtime_version.trim().is_empty()
                && !descriptor.minimum_app_version.trim().is_empty()
                && descriptor.input_schema.is_object()
                && descriptor.output_schema.is_object()
                && ids.insert(descriptor.id.clone());
            if !valid {
                return Err(ToolRegistryError::InvalidDescriptor(descriptor.id.clone()));
            }
        }
        Ok(Self { descriptors })
    }

    /// Descriptors in stable registration order.
    pub fn descriptors(&self) -> &[ToolDescriptor] {
        &self.descriptors
    }

    /// Looks up a stable first-party tool ID.
    pub fn get(&self, id: &str) -> Option<&ToolDescriptor> {
        self.descriptors
            .iter()
            .find(|descriptor| descriptor.id == id)
    }

    /// Hash included in runtime manifests and cross-surface compatibility checks.
    pub fn hash(&self) -> Result<String, ToolRegistryError> {
        let bytes = serde_json::to_vec(&self.descriptors).map_err(ToolRegistryError::Serialize)?;
        Ok(Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect())
    }

    /// Emits the typed registry artifact consumed by other Whisply owners.
    pub fn snapshot(
        &self,
        registry_revision: impl Into<String>,
    ) -> Result<ToolRegistrySnapshot, ToolRegistryError> {
        let registry_revision = registry_revision.into();
        if registry_revision.trim().is_empty() {
            return Err(ToolRegistryError::InvalidRegistryRevision);
        }
        Ok(ToolRegistrySnapshot {
            schema_version: TOOL_REGISTRY_SCHEMA_VERSION,
            registry_revision,
            descriptors: self.descriptors.clone(),
            registry_sha256: self.hash()?,
        })
    }
}

/// Returns Whisply's fixed first-party registry. The descriptor owner is not a
/// handler claim: native and server owners attach typed implementations only
/// after policy, account epoch, and availability verification.
pub fn first_party_tool_registry() -> ToolRegistry {
    let registry = ToolRegistry::new(vec![
        descriptor(
            "whisply.screen.context",
            "Screen",
            "View the selected app/window or an explicitly selected display.",
            "screen.context",
            ToolOwner::Native,
            ActionClass::Automatic,
            object_schema(
                &["scope"],
                serde_json::json!({
                    "scope": {"type": "string", "enum": ["exact_window", "selected_display"]},
                    "targetID": {"type": "string"},
                    "displayID": {"type": "string"},
                }),
            ),
            summary_output_schema(),
            DescriptorRequirements::new()
                .target()
                .native_permission()
                .usage_impact(),
        ),
        descriptor(
            "whisply.files",
            "Files",
            "Read or change explicitly approved workspace files and folders.",
            "folder",
            ToolOwner::Native,
            ActionClass::Automatic,
            object_schema(
                &["operation"],
                serde_json::json!({
                    "operation": {"type": "string"},
                    "grant_id": {"type": "string"},
                    "relative_path": {"type": "string"},
                    "query": {"type": "string"},
                    "maximum_bytes": {"type": "integer"},
                    "limit": {"type": "integer"},
                    "source_path": {"type": "string"},
                    "destination_path": {"type": "string"},
                    "content": {"type": "string"},
                    "requested_path": {"type": "string"},
                    "requested_access": {"type": "string"},
                }),
            ),
            object_schema(
                &["summary"],
                serde_json::json!({
                    "summary": {"type": "string"},
                    "material": {"type": "string"},
                    "materialKind": {"type": "string"},
                }),
            ),
            DescriptorRequirements::new().file_approval(),
        ),
        descriptor(
            "whisply.computer_use",
            "Computer Use",
            "Act only on the approved app or window target.",
            "computer.use",
            ToolOwner::Native,
            ActionClass::FinalConfirmationRequired,
            object_schema(
                &["action", "targetID"],
                serde_json::json!({
                    "action": {"type": "string"},
                    "targetID": {"type": "string"},
                    "arguments": {"type": "object"},
                }),
            ),
            summary_output_schema(),
            DescriptorRequirements::new()
                .target()
                .native_permission()
                .usage_impact(),
        ),
        descriptor(
            "whisply.browser",
            "Browser",
            "Use Whisply's isolated spawned browser profile.",
            "browser.spawned",
            ToolOwner::Native,
            ActionClass::Automatic,
            object_schema(
                &["operation"],
                serde_json::json!({
                    "operation": {"type": "string"},
                    "url": {"type": "string"},
                    "tabID": {"type": "string"},
                }),
            ),
            summary_output_schema(),
            DescriptorRequirements::new()
                .native_permission()
                .usage_impact(),
        ),
        descriptor(
            "whisply.chrome",
            "Google Chrome",
            "Use a live, signed connected Chrome profile.",
            "browser.chrome",
            ToolOwner::Native,
            ActionClass::Automatic,
            object_schema(
                &["operation"],
                serde_json::json!({
                    "operation": {"type": "string"},
                    "tabID": {"type": "string"},
                    "url": {"type": "string"},
                }),
            ),
            summary_output_schema(),
            DescriptorRequirements::new()
                .native_permission()
                .usage_impact(),
        ),
        server_descriptor(
            "whisply.connector.gmail",
            "Gmail",
            "connector.gmail",
            "Read Gmail through the connected server-authoritative account.",
            true,
        ),
        server_descriptor(
            "whisply.connector.github",
            "GitHub",
            "connector.github",
            "Read GitHub through the connected server-authoritative account.",
            true,
        ),
        server_descriptor(
            "whisply.memory",
            "Memory",
            "memory",
            "Use server-authoritative long-term memory.",
            false,
        ),
        descriptor(
            "whisply.skills",
            "Skills",
            "Use the registered Whisply and workspace skill catalog.",
            "skills",
            ToolOwner::LocalRuntime,
            ActionClass::Automatic,
            object_schema(
                &["operation"],
                serde_json::json!({
                    "operation": {"type": "string"},
                    "skillID": {"type": "string"},
                }),
            ),
            summary_output_schema(),
            DescriptorRequirements::new(),
        ),
        server_descriptor(
            "whisply.tasks",
            "Tasks",
            "tasks",
            "Use server-authoritative task and receipt state.",
            false,
        ),
        server_descriptor(
            "whisply.web_search",
            "Web Search",
            "web.search",
            "Search the web and retain source citations.",
            false,
        ),
        descriptor(
            "whisply.diagnostics",
            "Diagnostics",
            "Inspect bounded, redacted runtime diagnostics.",
            "diagnostics",
            ToolOwner::LocalRuntime,
            ActionClass::Automatic,
            object_schema(
                &["operation"],
                serde_json::json!({"operation": {"type": "string"}}),
            ),
            summary_output_schema(),
            DescriptorRequirements::new(),
        ),
    ]);
    let Ok(registry) = registry else {
        unreachable!("the fixed first-party tool registry contains an invalid descriptor")
    };
    registry
}

fn server_descriptor(
    id: &str,
    display_name: &str,
    icon_id: &str,
    short_description: &str,
    connector: bool,
) -> ToolDescriptor {
    let requirements = if connector {
        DescriptorRequirements::new()
            .connector_scope()
            .usage_impact()
    } else {
        DescriptorRequirements::new().usage_impact()
    };
    descriptor(
        id,
        display_name,
        short_description,
        icon_id,
        ToolOwner::Server,
        ActionClass::Automatic,
        object_schema(
            &["operation"],
            serde_json::json!({
                "operation": {"type": "string"},
                "query": {"type": "string"},
            }),
        ),
        summary_output_schema(),
        requirements,
    )
}

#[derive(Clone, Copy, Debug, Default)]
struct DescriptorRequirements {
    target: bool,
    file_approval: bool,
    connector_scope: bool,
    native_permission: bool,
    usage_impact: bool,
}

impl DescriptorRequirements {
    const fn new() -> Self {
        Self {
            target: false,
            file_approval: false,
            connector_scope: false,
            native_permission: false,
            usage_impact: false,
        }
    }

    const fn target(mut self) -> Self {
        self.target = true;
        self
    }

    const fn file_approval(mut self) -> Self {
        self.file_approval = true;
        self
    }

    const fn connector_scope(mut self) -> Self {
        self.connector_scope = true;
        self
    }

    const fn native_permission(mut self) -> Self {
        self.native_permission = true;
        self
    }

    const fn usage_impact(mut self) -> Self {
        self.usage_impact = true;
        self
    }
}

#[allow(clippy::too_many_arguments)]
fn descriptor(
    id: &str,
    display_name: &str,
    short_description: &str,
    icon_id: &str,
    owner: ToolOwner,
    action_class: ActionClass,
    input_schema: Value,
    output_schema: Value,
    requirements: DescriptorRequirements,
) -> ToolDescriptor {
    ToolDescriptor {
        id: id.to_string(),
        schema_version: TOOL_REGISTRY_SCHEMA_VERSION,
        display_name: display_name.to_string(),
        short_description: short_description.to_string(),
        icon_id: icon_id.to_string(),
        owner,
        input_schema,
        output_schema,
        supports_progress: true,
        supports_cancellation: true,
        timeout_seconds: 90.0,
        retry_limit: 0,
        action_class,
        requires_target_selection: requirements.target,
        requires_file_approval: requirements.file_approval,
        requires_connector_scope: requirements.connector_scope,
        requires_native_permission: requirements.native_permission,
        has_usage_impact: requirements.usage_impact,
        redaction_class: "safe-summary".to_string(),
        minimum_runtime_version: "1".to_string(),
        minimum_app_version: "1".to_string(),
    }
}

fn object_schema(required: &[&str], properties: Value) -> Value {
    let mut schema = serde_json::Map::new();
    schema.insert("type".to_string(), Value::String("object".to_string()));
    schema.insert("properties".to_string(), properties);
    if !required.is_empty() {
        schema.insert(
            "required".to_string(),
            Value::Array(
                required
                    .iter()
                    .map(|value| Value::String((*value).to_string()))
                    .collect(),
            ),
        );
    }
    Value::Object(schema)
}

fn summary_output_schema() -> Value {
    object_schema(
        &["summary"],
        serde_json::json!({"summary": {"type": "string"}}),
    )
}

/// Registry validation failures are release-manifest blockers.
#[derive(Debug, Error)]
pub enum ToolRegistryError {
    #[error("invalid or duplicate Whisply tool descriptor `{0}`")]
    InvalidDescriptor(String),
    #[error("Whisply tool registry revision must be non-empty")]
    InvalidRegistryRevision,
    #[error("failed to serialize the Whisply tool registry: {0}")]
    Serialize(serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn first_party_tools_match_the_native_contract_and_remain_distinct() {
        let registry = first_party_tool_registry();

        assert_eq!(registry.descriptors().len(), 12);
        assert_eq!(
            registry
                .get("whisply.screen.context")
                .expect("screen")
                .owner,
            ToolOwner::Native
        );
        assert_eq!(
            registry.get("whisply.skills").expect("skills").owner,
            ToolOwner::LocalRuntime
        );
        assert_ne!(
            registry.get("whisply.chrome").expect("chrome").id,
            registry.get("whisply.browser").expect("browser").id
        );
        assert_eq!(registry.hash().expect("registry hash").len(), 64);
        let snapshot = registry.snapshot("2026-08-08.1").expect("snapshot");
        assert_eq!(snapshot.schema_version, TOOL_REGISTRY_SCHEMA_VERSION);
        assert_eq!(snapshot.descriptors.len(), 12);
    }

    #[test]
    fn screen_and_files_schemas_preserve_native_handler_contracts() {
        let registry = first_party_tool_registry();
        let screen = registry.get("whisply.screen.context").expect("screen");
        assert_eq!(
            screen.input_schema["properties"]["scope"]["enum"],
            serde_json::json!(["exact_window", "selected_display"])
        );
        let files = registry.get("whisply.files").expect("files");
        assert_eq!(
            files.input_schema["required"],
            serde_json::json!(["operation"])
        );
        assert_eq!(
            files.output_schema["properties"]["materialKind"]["type"],
            "string"
        );
    }

    #[test]
    fn duplicate_tool_ids_are_rejected() {
        let descriptor = first_party_tool_registry().descriptors()[0].clone();
        assert!(ToolRegistry::new(vec![descriptor.clone(), descriptor]).is_err());
    }

    #[test]
    fn registry_wire_objects_reject_unknown_fields() {
        let snapshot = first_party_tool_registry()
            .snapshot("2026-08-08.1")
            .expect("snapshot");
        let mut value = serde_json::to_value(snapshot).expect("serialize snapshot");
        value["descriptors"][0]["unreviewedAuthority"] = serde_json::json!(true);

        assert!(serde_json::from_value::<ToolRegistrySnapshot>(value).is_err());
    }

    #[test]
    fn descriptor_wire_preserves_native_icon_id_spelling() {
        let descriptor = first_party_tool_registry().descriptors()[0].clone();
        let value = serde_json::to_value(descriptor).expect("descriptor");
        assert!(value.get("iconID").is_some());
        assert!(value.get("iconId").is_none());
    }
}

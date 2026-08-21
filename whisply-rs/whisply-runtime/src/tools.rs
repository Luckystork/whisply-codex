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

/// The owner spelling used by the packaged registry artifact.  This is kept
/// separate from [`ToolOwner`]: the latter is the native broker's internal
/// contract, while this type is the stable cross-runtime release projection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolRegistryArtifactOwner {
    WhisplyNative,
    WhisplyServer,
    WhisplyLocalRuntime,
}

impl From<ToolOwner> for ToolRegistryArtifactOwner {
    fn from(owner: ToolOwner) -> Self {
        match owner {
            ToolOwner::Native => Self::WhisplyNative,
            ToolOwner::Server => Self::WhisplyServer,
            ToolOwner::LocalRuntime => Self::WhisplyLocalRuntime,
        }
    }
}

/// The artifact never claims a ready handler.  Readiness, consent, connector
/// grants, and native permissions remain runtime state resolved by the owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolRegistryArtifactAvailability {
    OwnerAuthenticated,
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

/// One descriptor in the manifest-verified release artifact.
///
/// This intentionally uses the public schema's field and enum spelling rather
/// than the Swift/native broker spelling above.  It is the only representation
/// accepted by the packager and copied into a shipped Runtime tree.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolRegistryArtifactDescriptor {
    pub id: String,
    pub schema_version: u16,
    pub display_name: String,
    pub short_description: String,
    pub icon_id: String,
    pub owner: ToolRegistryArtifactOwner,
    pub input_schema: Value,
    pub output_schema: Value,
    pub supports_progress: bool,
    pub supports_cancellation: bool,
    pub timeout_seconds: f64,
    pub retry_limit: u16,
    pub action_class: ActionClass,
    pub requires_target: bool,
    pub requires_file_approval: bool,
    pub requires_connector_scope: bool,
    pub requires_native_permission: bool,
    pub has_usage_impact: bool,
    pub availability: ToolRegistryArtifactAvailability,
    pub diagnostic_redaction: String,
    pub minimum_runtime_version: String,
    pub minimum_app_version: String,
}

impl From<&ToolDescriptor> for ToolRegistryArtifactDescriptor {
    fn from(descriptor: &ToolDescriptor) -> Self {
        Self {
            id: descriptor.id.clone(),
            schema_version: descriptor.schema_version,
            display_name: descriptor.display_name.clone(),
            short_description: descriptor.short_description.clone(),
            icon_id: descriptor.icon_id.clone(),
            owner: descriptor.owner.into(),
            input_schema: descriptor.input_schema.clone(),
            output_schema: descriptor.output_schema.clone(),
            supports_progress: descriptor.supports_progress,
            supports_cancellation: descriptor.supports_cancellation,
            timeout_seconds: descriptor.timeout_seconds,
            retry_limit: descriptor.retry_limit,
            action_class: descriptor.action_class,
            requires_target: descriptor.requires_target_selection,
            requires_file_approval: descriptor.requires_file_approval,
            requires_connector_scope: descriptor.requires_connector_scope,
            requires_native_permission: descriptor.requires_native_permission,
            has_usage_impact: descriptor.has_usage_impact,
            availability: ToolRegistryArtifactAvailability::OwnerAuthenticated,
            diagnostic_redaction: descriptor.redaction_class.clone(),
            minimum_runtime_version: descriptor.minimum_runtime_version.clone(),
            minimum_app_version: descriptor.minimum_app_version.clone(),
        }
    }
}

/// The canonical artifact emitted by the exact bundled runtime during a
/// release.  The descriptor hash is over `descriptors`, not this outer object.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolRegistryArtifactSnapshot {
    pub schema_version: u16,
    pub registry_revision: String,
    pub descriptors: Vec<ToolRegistryArtifactDescriptor>,
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

    /// Returns the packaged-artifact projection for one stable tool ID.
    pub fn artifact_descriptor(&self, id: &str) -> Option<ToolRegistryArtifactDescriptor> {
        self.get(id).map(ToolRegistryArtifactDescriptor::from)
    }

    /// Hash of the canonical artifact descriptor array.
    pub fn artifact_hash(&self) -> Result<String, ToolRegistryError> {
        let descriptors = self.canonical_artifact_descriptors();
        let bytes = serde_json::to_vec(&descriptors).map_err(ToolRegistryError::Serialize)?;
        Ok(Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect())
    }

    /// Emits the strict package artifact.  Static descriptors deliberately use
    /// `owner_authenticated` rather than making a live availability claim.
    pub fn artifact_snapshot(
        &self,
        registry_revision: impl Into<String>,
    ) -> Result<ToolRegistryArtifactSnapshot, ToolRegistryError> {
        let registry_revision = registry_revision.into();
        if registry_revision.trim().is_empty() {
            return Err(ToolRegistryError::InvalidRegistryRevision);
        }
        let descriptors = self.canonical_artifact_descriptors();
        let bytes = serde_json::to_vec(&descriptors).map_err(ToolRegistryError::Serialize)?;
        let registry_sha256 = Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        Ok(ToolRegistryArtifactSnapshot {
            schema_version: TOOL_REGISTRY_SCHEMA_VERSION,
            registry_revision,
            descriptors,
            registry_sha256,
        })
    }

    fn canonical_artifact_descriptors(&self) -> Vec<ToolRegistryArtifactDescriptor> {
        self.descriptors
            .iter()
            .map(|descriptor| {
                let mut artifact = ToolRegistryArtifactDescriptor::from(descriptor);
                canonicalize_json_value(&mut artifact.input_schema);
                canonicalize_json_value(&mut artifact.output_schema);
                artifact
            })
            .collect()
    }
}

/// `serde_json::Map` can be insertion ordered when another workspace crate
/// enables `preserve_order`. The release artifact must not vary with that
/// feature unification, so normalize only schema values before serialization.
fn canonicalize_json_value(value: &mut Value) {
    match value {
        Value::Object(object) => {
            let mut entries = std::mem::take(object).into_iter().collect::<Vec<_>>();
            entries.sort_unstable_by(|left, right| left.0.cmp(&right.0));
            for (_, child) in &mut entries {
                canonicalize_json_value(child);
            }
            object.extend(entries);
        }
        Value::Array(values) => {
            for child in values {
                canonicalize_json_value(child);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
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
            screen_context_output_schema(),
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
        with_timeout(
            descriptor(
                "whisply.computer_use",
                "Computer Use",
                "Act only on the approved app or window target. Prefer perform_actions for two or more stable indexed buttons from one current observation.",
                "computer.use",
                ToolOwner::Native,
                ActionClass::FinalConfirmationRequired,
                object_schema(
                    &["action"],
                    serde_json::json!({
                        // Closed so the model cannot name an action the Mac
                        // does not implement and read the refusal as the app
                        // being broken.
                        "action": {
                            "type": "string",
                            "enum": COMPUTER_USE_ACTIONS,
                            "description": "list_apps may omit targetID; every other action requires the exact approved app identifier. perform_actions executes 2-12 ordered click steps from one pinned observation and returns one fresh final state."
                        },
                        "targetID": {
                            "type": "string",
                            "description": "Exact app identifier from the user's request or a prior list_apps result."
                        },
                        "arguments": {
                            "type": "object",
                            "description": "Action-specific arguments. For perform_actions use {actions:[{action:\"click\",element_index:N}, ...]}; every step must be a stable indexed button from the latest get_app_state result."
                        },
                    }),
                ),
                computer_use_output_schema(),
                DescriptorRequirements::new()
                    .target()
                    .native_permission()
                    .usage_impact(),
            ),
            // An action that changes something waits for a person to approve
            // the exact target and then to allow the action itself. The default
            // tool deadline would abandon the call while the confirmation is
            // still on screen, and the person's answer would land on a call
            // nobody is waiting for.
            LONG_RUNNING_TOOL_TIMEOUT_SECONDS,
        ),
        with_timeout(
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
            LONG_RUNNING_TOOL_TIMEOUT_SECONDS,
        ),
        with_timeout(
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
            LONG_RUNNING_TOOL_TIMEOUT_SECONDS,
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

/// How long a call may stay open when the tool is working the whole time.
///
/// Most tools answer in one action, so the default ceiling is short. A browser
/// task is different in kind: its duration is the duration of the work someone
/// asked for, and cutting it off at ninety seconds would report failure to the
/// model while the browser carried on. This is only the absolute ceiling —
/// whether a call is still alive is decided by progress reports, so a tool that
/// has gone silent still ends quickly.
/// The Computer Use actions the Mac owner implements. Kept in the same order
/// as the native catalog so the two descriptions stay comparable.
const COMPUTER_USE_ACTIONS: [&str; 11] = [
    "list_apps",
    "get_app_state",
    "perform_actions",
    "click",
    "drag",
    "perform_secondary_action",
    "press_key",
    "scroll",
    "select_text",
    "set_value",
    "type_text",
];

const DEFAULT_TOOL_TIMEOUT_SECONDS: f64 = 90.0;
const LONG_RUNNING_TOOL_TIMEOUT_SECONDS: f64 = 1_800.0;

/// Overrides the default ceiling for a tool whose work is not bounded by a
/// single action.
fn with_timeout(mut descriptor: ToolDescriptor, seconds: f64) -> ToolDescriptor {
    descriptor.timeout_seconds = seconds;
    descriptor
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
        timeout_seconds: DEFAULT_TOOL_TIMEOUT_SECONDS,
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

fn screen_context_output_schema() -> Value {
    object_schema(
        &[
            "summary",
            "observedText",
            "targetID",
            "scope",
            "width",
            "height",
        ],
        serde_json::json!({
            "summary": {"type": "string"},
            "observedText": {"type": "string"},
            "targetID": {"type": "string"},
            "scope": {"type": "string", "enum": ["exact_window", "selected_display"]},
            "width": {"type": "integer"},
            "height": {"type": "integer"},
        }),
    )
}

fn computer_use_output_schema() -> Value {
    object_schema(
        &["summary"],
        serde_json::json!({
            "summary": {"type": "string"},
            "material": {"type": "string"},
            "materialKind": {"type": "string"},
        }),
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

        let artifact = registry
            .artifact_snapshot("2026-08-10.1")
            .expect("artifact snapshot");
        assert_eq!(
            artifact.registry_sha256,
            registry.artifact_hash().expect("artifact hash")
        );
        assert_eq!(artifact.descriptors.len(), 12);
        assert_eq!(
            artifact
                .descriptors
                .iter()
                .find(|descriptor| descriptor.id == "whisply.skills")
                .expect("skills artifact descriptor")
                .owner,
            ToolRegistryArtifactOwner::WhisplyLocalRuntime
        );
        assert!(artifact.descriptors.iter().all(|descriptor| {
            descriptor.availability == ToolRegistryArtifactAvailability::OwnerAuthenticated
        }));
    }

    #[test]
    fn screen_and_files_schemas_preserve_native_handler_contracts() {
        let registry = first_party_tool_registry();
        let screen = registry.get("whisply.screen.context").expect("screen");
        assert_eq!(
            screen.input_schema["properties"]["scope"]["enum"],
            serde_json::json!(["exact_window", "selected_display"])
        );
        assert_eq!(
            screen.output_schema["required"],
            serde_json::json!([
                "summary",
                "observedText",
                "targetID",
                "scope",
                "width",
                "height"
            ])
        );
        let computer = registry.get("whisply.computer_use").expect("computer use");
        assert_eq!(
            computer.input_schema["required"],
            serde_json::json!(["action"])
        );
        assert_eq!(
            computer.output_schema["required"],
            serde_json::json!(["summary"])
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
    fn work_that_waits_on_a_person_is_not_capped_at_the_length_of_a_one_shot_tool() {
        let registry = first_party_tool_registry();
        // Computer Use is here because it waits twice on a person: once to
        // approve the target, once to allow the action itself. A one-shot
        // ceiling would abandon the call while its confirmation is on screen.
        for id in ["whisply.browser", "whisply.chrome", "whisply.computer_use"] {
            assert_eq!(
                registry
                    .get(id)
                    .expect("long-running owner")
                    .timeout_seconds,
                LONG_RUNNING_TOOL_TIMEOUT_SECONDS,
                "{id} runs for as long as the work does, so it cannot inherit the ceiling of a \
                 tool that answers in one action"
            );
        }
        for id in ["whisply.screen.context", "whisply.files"] {
            assert_eq!(
                registry.get(id).expect("one-shot owner").timeout_seconds,
                DEFAULT_TOOL_TIMEOUT_SECONDS,
                "{id} answers in one action and must not inherit a browser's ceiling"
            );
        }
    }

    #[test]
    fn descriptor_wire_preserves_native_icon_id_spelling() {
        let descriptor = first_party_tool_registry().descriptors()[0].clone();
        let value = serde_json::to_value(descriptor).expect("descriptor");
        assert!(value.get("iconID").is_some());
        assert!(value.get("iconId").is_none());
    }

    #[test]
    fn artifact_wire_uses_the_packaged_schema_and_rejects_unknown_fields() {
        let artifact = first_party_tool_registry()
            .artifact_snapshot("2026-08-10.1")
            .expect("artifact");
        let mut value = serde_json::to_value(&artifact).expect("serialize artifact");
        let descriptor = &value["descriptors"][0];
        assert_eq!(descriptor["owner"], "whisply_native");
        assert_eq!(descriptor["availability"], "owner_authenticated");
        assert!(descriptor.get("iconId").is_some());
        assert!(descriptor.get("iconID").is_none());
        assert_eq!(
            descriptor["inputSchema"]["properties"]
                .as_object()
                .expect("canonical properties")
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            vec![
                "displayID".to_string(),
                "scope".to_string(),
                "targetID".to_string(),
            ]
        );
        value["descriptors"][0]["unreviewedAuthority"] = serde_json::json!(true);
        assert!(serde_json::from_value::<ToolRegistryArtifactSnapshot>(value).is_err());
    }
}

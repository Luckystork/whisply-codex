//! Model-callable server-owned first-party Whisply tools.
//!
//! Unlike native capabilities, these descriptors do not require a one-turn
//! admission. Core carries only the model-visible call and thread/turn
//! correlation to the app-server dispatcher, which mints a placeholder trusted
//! envelope and routes execution to the Mac client over `whisply/tool/execute`.

use codex_app_server_protocol::WhisplyToolModelCall;
use codex_app_server_protocol::WhisplyToolResult;
use codex_whisply::ToolDescriptor;
use codex_whisply::ToolOwner;
use codex_whisply::first_party_tool_registry;
use futures::future::BoxFuture;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

const MAX_CORRELATION_ID_BYTES: usize = 256;

/// Fixed server-owned descriptors exposed to the model when a dispatcher is
/// installed.
pub const SERVER_MODEL_TOOL_IDS: [&str; 5] = [
    "whisply.memory",
    "whisply.web_search",
    "whisply.connector.gmail",
    "whisply.connector.github",
    "whisply.tasks",
];

/// Model-visible function names reserved for server-owned capabilities.
pub const SERVER_MODEL_TOOL_NAMES: &[&str] = &[
    "memory",
    "web_search",
    "connector_gmail",
    "connector_github",
    "tasks",
];

/// Whether the static registry id belongs to the closed server-owned set.
pub fn is_server_model_tool_id(tool_id: &str) -> bool {
    SERVER_MODEL_TOOL_IDS.contains(&tool_id)
}

/// Returns the closed model-visible function name for a server descriptor.
///
/// Names are derived from the registry `icon_id` by replacing `.` with `_`.
pub(crate) fn server_model_tool_name(tool_id: &str) -> Option<String> {
    if !is_server_model_tool_id(tool_id) {
        return None;
    }
    let registry = first_party_tool_registry();
    let descriptor = registry.get(tool_id)?;
    if descriptor.owner != ToolOwner::Server {
        return None;
    }
    let name = descriptor.icon_id.replace('.', "_");
    if name.is_empty() {
        return None;
    }
    Some(name)
}

/// Reasons a proposed server tool execution was rejected before dispatch.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum ServerToolExecutionError {
    /// The proposed ID is not in Whisply's fixed first-party registry.
    #[error("unknown first-party tool")]
    UnknownTool,
    /// The proposed descriptor is not server-owned.
    #[error("first-party tool is not server-owned")]
    NonServerTool,
    /// The model call's registry version differs from its static descriptor.
    #[error("first-party tool schema version does not match")]
    SchemaVersionMismatch,
    /// The runtime correlation identifier was empty or exceeded its bound.
    #[error("invalid first-party tool runtime correlation")]
    InvalidRuntimeCorrelation,
}

/// A bounded request passed from Core to the server-tool dispatcher.
///
/// It intentionally contains no admission, account identity, bearer, target,
/// or lease. The app-server mints a placeholder envelope and the Mac client
/// re-mints trusted identity before execution.
#[derive(Clone, Debug, PartialEq)]
pub struct ServerToolExecution {
    call: WhisplyToolModelCall,
    thread_id: String,
    turn_id: String,
}

impl ServerToolExecution {
    /// Builds a dispatcher request after checking the static server descriptor.
    pub fn new(
        call: WhisplyToolModelCall,
        thread_id: String,
        turn_id: String,
    ) -> Result<Self, ServerToolExecutionError> {
        let registry = first_party_tool_registry();
        let descriptor = registry
            .get(&call.tool_id)
            .ok_or(ServerToolExecutionError::UnknownTool)?;
        if descriptor.owner != ToolOwner::Server {
            return Err(ServerToolExecutionError::NonServerTool);
        }
        if !is_server_model_tool_id(&call.tool_id)
            || server_model_tool_name(&call.tool_id).is_none()
        {
            return Err(ServerToolExecutionError::NonServerTool);
        }
        if descriptor.schema_version != call.schema_version {
            return Err(ServerToolExecutionError::SchemaVersionMismatch);
        }
        if !is_bounded_identifier(&thread_id, MAX_CORRELATION_ID_BYTES)
            || !is_bounded_identifier(&turn_id, MAX_CORRELATION_ID_BYTES)
            || !is_bounded_identifier(&call.execution_id, MAX_CORRELATION_ID_BYTES)
        {
            return Err(ServerToolExecutionError::InvalidRuntimeCorrelation);
        }

        Ok(Self {
            call,
            thread_id,
            turn_id,
        })
    }

    /// Returns the model-visible call after it passed static descriptor checks.
    pub fn call(&self) -> &WhisplyToolModelCall {
        &self.call
    }

    /// Returns the owning Core thread identifier.
    pub fn thread_id(&self) -> &str {
        &self.thread_id
    }

    /// Returns the exact Core turn identifier for this execution.
    pub fn turn_id(&self) -> &str {
        &self.turn_id
    }
}

/// Executes one server-owned first-party tool call through the Mac client.
pub trait ServerToolDispatcher: Send + Sync {
    /// Dispatches a server-owned call and returns a typed terminal result.
    fn execute(
        &self,
        execution: ServerToolExecution,
        cancellation: CancellationToken,
    ) -> BoxFuture<'static, Result<WhisplyToolResult, ServerToolDispatchError>>;
}

/// A non-sensitive terminal dispatch error exposed to the Core handler.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum ServerToolDispatchError {
    /// The Mac client is not reachable for this thread.
    #[error("server-owned tool is unavailable")]
    Unavailable,
    /// The active turn or server execution was cancelled.
    #[error("server-owned tool was cancelled")]
    Cancelled,
    /// The Mac client failed without a model-visible result.
    #[error("server-owned tool failed")]
    Failed,
}

/// Returns every static server-owned descriptor in the closed tool-id order.
pub(crate) fn server_descriptors() -> Vec<ToolDescriptor> {
    let registry = first_party_tool_registry();
    SERVER_MODEL_TOOL_IDS
        .iter()
        .filter_map(|tool_id| registry.get(tool_id).cloned())
        .collect()
}

fn is_bounded_identifier(value: &str, maximum_bytes: usize) -> bool {
    !value.trim().is_empty() && value.len() <= maximum_bytes
}

#[cfg(test)]
#[path = "server_tools_tests.rs"]
mod tests;

//! One-turn admission for owner-authenticated first-party Whisply tools.
//!
//! The static registry describes product capabilities, but it never grants a
//! model the authority to invoke a native owner. A UI owner must first create
//! one of these short-lived admissions after it has independently validated
//! the user, target, consent, and any required lease. Core only carries the
//! opaque admission and model-visible arguments to the owner-specific
//! dispatcher; it never manufactures an execution envelope from model output.

use std::collections::BTreeSet;
use std::sync::Mutex;

use codex_app_server_protocol::WhisplyToolModelCall;
use codex_app_server_protocol::WhisplyToolResult;
use codex_whisply::ToolDescriptor;
use codex_whisply::ToolOwner;
use codex_whisply::first_party_tool_registry;
use futures::future::BoxFuture;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

const MAX_ADMISSION_ID_BYTES: usize = 256;
const MAX_CLIENT_USER_MESSAGE_ID_BYTES: usize = 256;
/// The generic one-turn bridge carries only tools whose Mac owner admits the
/// call against its own permission, file grant, approved target, or
/// account-bound task before anything runs. Carrying a call is not granting
/// it: the owner still decides. Under `Auto`, Computer Use is reviewed first by
/// a fresh Luna packet in the handler; the native owner then honors that
/// verdict for routine app binding and still holds final-action confirmation.
const GENERIC_ADMITTED_NATIVE_TOOL_IDS: [&str; 5] = [
    "whisply.screen.context",
    "whisply.files",
    "whisply.computer_use",
    "whisply.browser",
    "whisply.chrome",
];
const MAX_ADMITTED_NATIVE_TOOLS: usize = GENERIC_ADMITTED_NATIVE_TOOL_IDS.len();

/// A validated, one-turn capability grant for a fixed set of native tools.
///
/// The ID is opaque to Core and the model. It is meaningful only to the
/// owner that registered it, which must bind it to an exact connection,
/// thread, user-message, target, consent, and lease before dispatching a
/// call. Server-owned and local-runtime descriptors are intentionally not
/// accepted here.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirstPartyToolAdmission {
    admission_id: String,
    client_user_message_id: String,
    allowed_tool_ids: BTreeSet<String>,
}

impl FirstPartyToolAdmission {
    /// Creates a bounded native-only admission from an owner-issued opaque ID.
    pub fn new(
        admission_id: String,
        client_user_message_id: String,
        tool_ids: impl IntoIterator<Item = String>,
    ) -> Result<Self, FirstPartyToolAdmissionError> {
        if !is_bounded_identifier(&admission_id, MAX_ADMISSION_ID_BYTES) {
            return Err(FirstPartyToolAdmissionError::InvalidAdmissionId);
        }
        if !is_bounded_identifier(&client_user_message_id, MAX_CLIENT_USER_MESSAGE_ID_BYTES) {
            return Err(FirstPartyToolAdmissionError::InvalidClientUserMessageId);
        }

        let tool_ids = tool_ids.into_iter().collect::<BTreeSet<_>>();
        if tool_ids.is_empty() || tool_ids.len() > MAX_ADMITTED_NATIVE_TOOLS {
            return Err(FirstPartyToolAdmissionError::InvalidToolSet);
        }

        let registry = first_party_tool_registry();
        for tool_id in &tool_ids {
            let Some(descriptor) = registry.get(tool_id) else {
                return Err(FirstPartyToolAdmissionError::UnknownTool);
            };
            if descriptor.owner != ToolOwner::Native {
                return Err(FirstPartyToolAdmissionError::NonNativeTool);
            }
            if !is_generic_admitted_native_tool_id(tool_id)
                || native_model_tool_name(tool_id).is_none()
            {
                return Err(FirstPartyToolAdmissionError::UnsupportedNativeTool);
            }
        }

        Ok(Self {
            admission_id,
            client_user_message_id,
            allowed_tool_ids: tool_ids,
        })
    }

    /// Returns the opaque admission identifier registered by the owner.
    pub fn admission_id(&self) -> &str {
        &self.admission_id
    }

    /// Returns the exact client user-message identifier this admission covers.
    pub fn client_user_message_id(&self) -> &str {
        &self.client_user_message_id
    }

    /// Returns whether a static native descriptor is admitted for this turn.
    pub fn allows_tool(&self, tool_id: &str) -> bool {
        self.allowed_tool_ids.contains(tool_id)
    }

    /// Returns the admitted static native descriptors in stable registry order.
    pub fn native_descriptors(&self) -> Vec<ToolDescriptor> {
        first_party_tool_registry()
            .descriptors()
            .iter()
            .filter(|descriptor| {
                descriptor.owner == ToolOwner::Native && self.allows_tool(&descriptor.id)
            })
            .cloned()
            .collect()
    }
}

/// The precise reason a proposed first-party admission was rejected.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum FirstPartyToolAdmissionError {
    /// The owner did not provide a bounded opaque admission ID.
    #[error("invalid first-party tool admission id")]
    InvalidAdmissionId,
    /// The owner did not bind the admission to one bounded client message ID.
    #[error("invalid first-party tool client message id")]
    InvalidClientUserMessageId,
    /// The proposed native tool set was empty or exceeded the fixed bound.
    #[error("invalid first-party tool set")]
    InvalidToolSet,
    /// The proposed ID is not in Whisply's fixed first-party registry.
    #[error("unknown first-party tool")]
    UnknownTool,
    /// The proposed descriptor is owned by a server or local runtime.
    #[error("first-party tool is not native-owned")]
    NonNativeTool,
    /// The static native descriptor does not have an approved model namespace.
    #[error("unsupported native first-party tool")]
    UnsupportedNativeTool,
    /// The model call did not match the admission's fixed descriptor set.
    #[error("first-party tool call is not admitted")]
    CallNotAdmitted,
    /// The model call's registry version differs from its static descriptor.
    #[error("first-party tool schema version does not match")]
    SchemaVersionMismatch,
    /// The runtime correlation identifier was empty or exceeded its bound.
    #[error("invalid first-party tool runtime correlation")]
    InvalidRuntimeCorrelation,
}

/// A bounded request passed from Core to the owner-specific dispatcher.
///
/// It intentionally contains no account ID, bearer, target, tab, file grant,
/// task lease, confirmation hash, or native permission. The owner uses the
/// opaque admission plus its own retained state to construct the existing
/// trusted execution envelope.
#[derive(Clone, Debug, PartialEq)]
pub struct FirstPartyToolExecution {
    admission: FirstPartyToolAdmission,
    call: WhisplyToolModelCall,
    thread_id: String,
    turn_id: String,
}

impl FirstPartyToolExecution {
    /// Builds a dispatcher request after checking the static descriptor and
    /// exact one-turn admission.
    pub fn new(
        admission: FirstPartyToolAdmission,
        call: WhisplyToolModelCall,
        thread_id: String,
        turn_id: String,
    ) -> Result<Self, FirstPartyToolAdmissionError> {
        if !admission.allows_tool(&call.tool_id) {
            return Err(FirstPartyToolAdmissionError::CallNotAdmitted);
        }
        let registry = first_party_tool_registry();
        let descriptor = registry
            .get(&call.tool_id)
            .ok_or(FirstPartyToolAdmissionError::UnknownTool)?;
        if descriptor.owner != ToolOwner::Native {
            return Err(FirstPartyToolAdmissionError::NonNativeTool);
        }
        if !is_generic_admitted_native_tool_id(&call.tool_id)
            || native_model_tool_name(&call.tool_id).is_none()
        {
            return Err(FirstPartyToolAdmissionError::UnsupportedNativeTool);
        }
        if descriptor.schema_version != call.schema_version {
            return Err(FirstPartyToolAdmissionError::SchemaVersionMismatch);
        }
        if !is_bounded_identifier(&thread_id, MAX_ADMISSION_ID_BYTES)
            || !is_bounded_identifier(&turn_id, MAX_ADMISSION_ID_BYTES)
            || !is_bounded_identifier(&call.execution_id, MAX_ADMISSION_ID_BYTES)
        {
            return Err(FirstPartyToolAdmissionError::InvalidRuntimeCorrelation);
        }

        Ok(Self {
            admission,
            call,
            thread_id,
            turn_id,
        })
    }

    /// Returns the opaque admission used to correlate this exact execution.
    pub fn admission(&self) -> &FirstPartyToolAdmission {
        &self.admission
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

/// Executes one already-admitted native first-party tool call.
///
/// Implementations must route only to the connection and owner that created
/// the opaque admission. They must build the trusted execution envelope from
/// retained owner state, reject correlation mismatches, and observe the
/// cancellation token without exposing credentials or authority in the model
/// call.
pub trait FirstPartyToolDispatcher: Send + Sync {
    /// Dispatches an admitted call and returns a typed terminal result.
    fn execute(
        &self,
        execution: FirstPartyToolExecution,
        cancellation: CancellationToken,
    ) -> BoxFuture<'static, Result<WhisplyToolResult, FirstPartyToolDispatchError>>;
}

/// A non-sensitive terminal dispatch error exposed to the Core handler.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum FirstPartyToolDispatchError {
    /// The owner is not reachable or no longer recognizes the admission.
    #[error("first-party tool is unavailable")]
    Unavailable,
    /// The active turn or native execution was cancelled.
    #[error("first-party tool was cancelled")]
    Cancelled,
    /// The owner failed without a model-visible result.
    #[error("first-party tool failed")]
    Failed,
}

/// Pending admissions keyed by the Core submission ID until a turn accepts
/// the user message. A steered message consumes and discards the admission,
/// because it cannot safely add native tools to an already-running turn.
#[derive(Default)]
pub(crate) struct PendingFirstPartyToolAdmissions {
    pending: Mutex<std::collections::HashMap<String, FirstPartyToolAdmission>>,
}

impl PendingFirstPartyToolAdmissions {
    pub(crate) fn insert(&self, submission_id: String, admission: FirstPartyToolAdmission) {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(submission_id, admission);
    }

    pub(crate) fn take(&self, submission_id: &str) -> Option<FirstPartyToolAdmission> {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(submission_id)
    }

    pub(crate) fn remove(&self, submission_id: &str) {
        let _ = self.take(submission_id);
    }
}

/// The model-visible names Whisply's first-party capabilities own.
///
/// These tools are registered only on a turn that carries a matching admission,
/// so on any turn without one the name is simply free. Without a standing
/// reservation an external MCP or plugin tool could take `screen_context` or
/// `browser` and present itself to the model as the first-party capability;
/// the model has no way to tell the difference, because the name is the whole
/// identity it sees. Reserving them unconditionally keeps the name meaningful
/// whether or not the real tool is admitted this turn.
pub const FIRST_PARTY_MODEL_TOOL_NAMES: &[&str] = &[
    "screen_context",
    "files",
    "computer_use",
    "browser",
    "chrome",
];

/// Whether the default-namespace name belongs to the product rather than to
/// anything a user can configure.
///
/// The registry refuses an external tool that takes one of these names, so
/// every surface that tells someone a tool is available has to answer the
/// question the same way the registry does.
pub fn is_reserved_model_tool_name(name: &str) -> bool {
    name == "shell_command"
        || FIRST_PARTY_MODEL_TOOL_NAMES.contains(&name)
        || crate::server_tools::SERVER_MODEL_TOOL_NAMES.contains(&name)
}

/// Whether the namespace belongs to the product rather than to anything a user
/// can configure.
///
/// A configured MCP server keeps its tools in a namespace named after the
/// server, so a server named `whisply` running without the `mcp__` prefix
/// lands in the one the product's capabilities answer in. The registry refuses
/// those tools; this is here so the surfaces that list a server's tools can
/// give the same answer instead of showing them as available.
pub fn is_reserved_model_tool_namespace(namespace: &str) -> bool {
    whisply_protocol::is_whisply_function_namespace(namespace)
}

/// Returns the closed model-visible function name for a native descriptor.
pub(crate) fn native_model_tool_name(tool_id: &str) -> Option<&'static str> {
    match tool_id {
        "whisply.screen.context" => Some("screen_context"),
        "whisply.files" => Some("files"),
        "whisply.computer_use" => Some("computer_use"),
        "whisply.browser" => Some("browser"),
        "whisply.chrome" => Some("chrome"),
        _ => None,
    }
}

/// Returns whether a registry descriptor is supported by the shared generic
/// one-turn admission bridge. This intentionally does not describe all native
/// tools: server-owned and local-runtime descriptors keep their own routes and
/// cannot be widened by this generic path.
pub(crate) fn is_generic_admitted_native_tool_id(tool_id: &str) -> bool {
    GENERIC_ADMITTED_NATIVE_TOOL_IDS.contains(&tool_id)
}

/// Computer Use is the native owner that currently skips its person-owned
/// prompt under `Auto`, claiming the runtime already reviewed the action.
/// That review has to happen here, as a fresh Luna packet, before dispatch.
pub(crate) fn first_party_tool_requires_auto_review(tool_id: &str) -> bool {
    tool_id == "whisply.computer_use"
}

fn is_bounded_identifier(value: &str, maximum_bytes: usize) -> bool {
    !value.trim().is_empty() && value.len() <= maximum_bytes
}

#[cfg(test)]
#[path = "first_party_tools_tests.rs"]
mod tests;

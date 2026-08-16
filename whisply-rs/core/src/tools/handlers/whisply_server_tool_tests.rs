use super::*;

use std::sync::Arc;

use codex_app_server_protocol::WhisplyToolResult;
use codex_whisply::first_party_tool_registry;
use futures::future::BoxFuture;
use tokio_util::sync::CancellationToken;

struct NeverDispatches;

impl ServerToolDispatcher for NeverDispatches {
    fn execute(
        &self,
        _execution: ServerToolExecution,
        _cancellation: CancellationToken,
    ) -> BoxFuture<'static, Result<WhisplyToolResult, ServerToolDispatchError>> {
        Box::pin(async { Err(ServerToolDispatchError::Unavailable) })
    }
}

#[test]
fn all_registers_only_server_owned_descriptors() {
    let handlers = ServerToolHandler::all(Arc::new(NeverDispatches));
    let reached: Vec<String> = handlers
        .iter()
        .map(|handler| handler.descriptor.id.clone())
        .collect();
    assert_eq!(reached, crate::server_tools::SERVER_MODEL_TOOL_IDS.to_vec());
}

#[test]
fn handler_refuses_native_owned_descriptor() {
    let registry = first_party_tool_registry();
    let native = registry
        .get("whisply.screen.context")
        .expect("native descriptor");
    assert!(ServerToolHandler::new(Arc::new(NeverDispatches), native.clone()).is_none());
}

#[test]
fn handler_refuses_local_runtime_descriptor() {
    let registry = first_party_tool_registry();
    let local = registry
        .get("whisply.skills")
        .expect("local runtime descriptor");
    assert!(ServerToolHandler::new(Arc::new(NeverDispatches), local.clone()).is_none());
}

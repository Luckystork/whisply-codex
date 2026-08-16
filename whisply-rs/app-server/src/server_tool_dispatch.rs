//! App-server routing for model-callable server-owned Whisply tools.
//!
//! Core hands off thread/turn correlation and model-visible arguments. This
//! module mints a placeholder trusted envelope, sends `whisply/tool/execute` to
//! the Mac client subscribed to the thread, and waits for the terminal result.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Weak;
use std::time::Duration;
use std::time::Instant;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use codex_app_server_protocol::ServerRequestPayload;
use codex_app_server_protocol::WhisplyExecutionEnvelope;
use codex_app_server_protocol::WhisplyToolAppServerCancelAcknowledgement;
use codex_app_server_protocol::WhisplyToolAppServerCancelRequest;
use codex_app_server_protocol::WhisplyToolAppServerExecuteRequest;
use codex_app_server_protocol::WhisplyToolPolicyInput;
use codex_app_server_protocol::WhisplyToolProgressReportParams;
use codex_app_server_protocol::WhisplyToolResult;
use futures::future::BoxFuture;
use tokio::sync::Mutex;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use whisply_core::server_tools::ServerToolDispatchError;
use whisply_core::server_tools::ServerToolDispatcher;
use whisply_core::server_tools::ServerToolExecution;
use whisply_protocol::ThreadId;

use crate::outgoing_message::ConnectionId;
use crate::outgoing_message::OutgoingMessageSender;
use crate::thread_state::ThreadStateManager;

const EXECUTION_TIMEOUT: Duration = Duration::from_secs(/*secs*/ 120);
const MAX_EXECUTION_LIFETIME: Duration = Duration::from_secs(/*secs*/ 1_800);
const CANCELLATION_TIMEOUT: Duration = Duration::from_secs(/*secs*/ 5);
const MAX_CORRELATION_ID_BYTES: usize = 256;
const MAX_PROGRESS_LABEL_BYTES: usize = 240;
const PLACEHOLDER_UUID: &str = "00000000-0000-0000-0000-000000000000";
const PLACEHOLDER_NONCE: &str = "placeholder";
const SERVER_TOOL_ADMISSION_PREFIX: &str = "server-tool:";

/// Synthetic admission id the Mac uses for server-owned tool progress on a turn.
pub(crate) fn server_tool_admission_id(thread_id: &str, turn_id: &str) -> String {
    format!("{SERVER_TOOL_ADMISSION_PREFIX}{thread_id}:{turn_id}")
}

/// Tracks in-flight server-tool calls so long waits can mirror native liveness.
#[derive(Default)]
pub(crate) struct ServerToolExecutionStore {
    state: Mutex<ExecutionState>,
}

#[derive(Default)]
struct ExecutionState {
    in_flight: HashMap<ExecutionKey, ExecutionRecord>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ExecutionKey {
    thread_id: String,
    turn_id: String,
    execution_id: String,
}

struct ExecutionRecord {
    connection_id: ConnectionId,
    liveness: ExecutionLiveness,
}

struct ExecutionLiveness {
    started_at: Instant,
    deadline: Instant,
}

impl ExecutionLiveness {
    fn new(now: Instant) -> Self {
        Self {
            started_at: now,
            deadline: now + EXECUTION_TIMEOUT,
        }
    }

    fn extend(&mut self, now: Instant) {
        self.deadline = (now + EXECUTION_TIMEOUT).min(self.started_at + MAX_EXECUTION_LIFETIME);
    }
}

impl ServerToolExecutionStore {
    async fn begin_execution(
        &self,
        connection_id: ConnectionId,
        execution: &ServerToolExecution,
    ) -> Option<()> {
        let key = execution_key(execution);
        let mut state = self.state.lock().await;
        if state.in_flight.contains_key(&key) {
            return None;
        }
        state.in_flight.insert(
            key,
            ExecutionRecord {
                connection_id,
                liveness: ExecutionLiveness::new(Instant::now()),
            },
        );
        Some(())
    }

    async fn execution_deadline(&self, execution: &ServerToolExecution) -> Option<Instant> {
        let state = self.state.lock().await;
        state
            .in_flight
            .get(&execution_key(execution))
            .map(|record| record.liveness.deadline)
    }

    async fn finish_execution(&self, execution: &ServerToolExecution) {
        let mut state = self.state.lock().await;
        state.in_flight.remove(&execution_key(execution));
    }

    /// Accepts progress only for a server-owned call in flight on this connection.
    pub(crate) async fn report_progress(
        &self,
        connection_id: ConnectionId,
        params: &WhisplyToolProgressReportParams,
    ) -> bool {
        if params.admission_id != server_tool_admission_id(&params.thread_id, &params.turn_id)
            || !is_bounded_identifier(&params.thread_id, MAX_CORRELATION_ID_BYTES)
            || !is_bounded_identifier(&params.turn_id, MAX_CORRELATION_ID_BYTES)
            || !is_bounded_identifier(&params.execution_id, MAX_CORRELATION_ID_BYTES)
            || !is_bounded_identifier(&params.label, MAX_PROGRESS_LABEL_BYTES)
            || params
                .fraction
                .is_some_and(|fraction| !(0.0..=1.0).contains(&fraction))
            || params
                .icon_id
                .as_deref()
                .is_some_and(|icon| !is_bounded_identifier(icon, MAX_CORRELATION_ID_BYTES))
        {
            return false;
        }
        let key = ExecutionKey {
            thread_id: params.thread_id.clone(),
            turn_id: params.turn_id.clone(),
            execution_id: params.execution_id.clone(),
        };
        let now = Instant::now();
        let mut state = self.state.lock().await;
        let Some(record) = state.in_flight.get_mut(&key) else {
            return false;
        };
        if record.connection_id != connection_id {
            return false;
        }
        record.liveness.extend(now);
        true
    }
}

fn is_bounded_identifier(value: &str, max_bytes: usize) -> bool {
    !value.is_empty() && value.len() <= max_bytes && !value.bytes().any(|byte| byte == b'\0')
}

fn execution_key(execution: &ServerToolExecution) -> ExecutionKey {
    ExecutionKey {
        thread_id: execution.thread_id().to_string(),
        turn_id: execution.turn_id().to_string(),
        execution_id: execution.call().execution_id.clone(),
    }
}

/// Creates the Core dispatcher that routes server-owned model calls to the Mac.
pub(crate) fn app_server_server_tool_dispatcher(
    outgoing: Arc<OutgoingMessageSender>,
    thread_state_manager: ThreadStateManager,
    installation_id: String,
    executions: Arc<ServerToolExecutionStore>,
) -> Arc<dyn ServerToolDispatcher> {
    Arc::new(AppServerServerToolDispatcher {
        outgoing: Arc::downgrade(&outgoing),
        thread_state_manager,
        installation_id,
        executions,
    })
}

struct AppServerServerToolDispatcher {
    outgoing: Weak<OutgoingMessageSender>,
    thread_state_manager: ThreadStateManager,
    installation_id: String,
    executions: Arc<ServerToolExecutionStore>,
}

impl ServerToolDispatcher for AppServerServerToolDispatcher {
    fn execute(
        &self,
        execution: ServerToolExecution,
        cancellation: CancellationToken,
    ) -> BoxFuture<'static, Result<WhisplyToolResult, ServerToolDispatchError>> {
        let outgoing = self.outgoing.clone();
        let thread_state_manager = self.thread_state_manager.clone();
        let installation_id = self.installation_id.clone();
        let executions = Arc::clone(&self.executions);
        Box::pin(async move {
            let Some(outgoing) = outgoing.upgrade() else {
                return Err(ServerToolDispatchError::Unavailable);
            };
            let Some(connection_id) =
                connection_for_thread(&thread_state_manager, execution.thread_id()).await
            else {
                return Err(ServerToolDispatchError::Unavailable);
            };
            if executions
                .begin_execution(connection_id, &execution)
                .await
                .is_none()
            {
                return Err(ServerToolDispatchError::Unavailable);
            }
            let result = dispatch_execution(
                &outgoing,
                connection_id,
                &execution,
                &installation_id,
                cancellation,
                &executions,
            )
            .await;
            executions.finish_execution(&execution).await;
            result
        })
    }
}

async fn connection_for_thread(
    thread_state_manager: &ThreadStateManager,
    thread_id: &str,
) -> Option<ConnectionId> {
    let thread_id = ThreadId::from_string(thread_id).ok()?;
    let connection_ids = thread_state_manager
        .subscribed_connection_ids(thread_id)
        .await;
    connection_ids
        .into_iter()
        .min_by_key(|connection_id| connection_id.0)
}

async fn dispatch_execution(
    outgoing: &Arc<OutgoingMessageSender>,
    connection_id: ConnectionId,
    execution: &ServerToolExecution,
    installation_id: &str,
    cancellation: CancellationToken,
    executions: &ServerToolExecutionStore,
) -> Result<WhisplyToolResult, ServerToolDispatchError> {
    let thread_id = ThreadId::from_string(execution.thread_id()).ok();
    let connection_ids = [connection_id];
    let (request_id, mut response) = outgoing
        .send_request_to_connections(
            Some(&connection_ids),
            ServerRequestPayload::WhisplyToolExecute(WhisplyToolAppServerExecuteRequest {
                call: execution.call().clone(),
                envelope: placeholder_envelope(execution, installation_id),
                policy_inputs: WhisplyToolPolicyInput::default(),
            }),
            thread_id,
        )
        .await;

    loop {
        let wait = executions
            .execution_deadline(execution)
            .await
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
            .unwrap_or(EXECUTION_TIMEOUT);
        tokio::select! {
            _ = cancellation.cancelled() => {
                let _canceled = outgoing.cancel_request(&request_id).await;
                send_cancellation(outgoing, connection_id, execution, installation_id).await;
                return Err(ServerToolDispatchError::Cancelled);
            }
            settled = timeout(wait, &mut response) => match settled {
                Ok(Ok(Ok(value))) => {
                    let result = serde_json::from_value::<WhisplyToolResult>(value)
                        .map_err(|_| ServerToolDispatchError::Failed)?;
                    return (result.execution_id == execution.call().execution_id)
                        .then_some(result)
                        .ok_or(ServerToolDispatchError::Failed);
                }
                Ok(Ok(Err(_))) => return Err(ServerToolDispatchError::Failed),
                Ok(Err(_)) => return Err(ServerToolDispatchError::Unavailable),
                Err(_) => {
                    let still_live = executions
                        .execution_deadline(execution)
                        .await
                        .is_some_and(|deadline| deadline > Instant::now());
                    if still_live {
                        continue;
                    }
                    let _canceled = outgoing.cancel_request(&request_id).await;
                    send_cancellation(outgoing, connection_id, execution, installation_id).await;
                    return Err(ServerToolDispatchError::Unavailable);
                }
            },
        }
    }
}

async fn send_cancellation(
    outgoing: &Arc<OutgoingMessageSender>,
    connection_id: ConnectionId,
    execution: &ServerToolExecution,
    installation_id: &str,
) {
    let connection_ids = [connection_id];
    let (request_id, response) = outgoing
        .send_request_to_connections(
            Some(&connection_ids),
            ServerRequestPayload::WhisplyToolCancel(WhisplyToolAppServerCancelRequest {
                execution_id: execution.call().execution_id.clone(),
                envelope: placeholder_envelope(execution, installation_id),
            }),
            ThreadId::from_string(execution.thread_id()).ok(),
        )
        .await;
    let acknowledged = match timeout(CANCELLATION_TIMEOUT, response).await {
        Ok(Ok(Ok(value))) => serde_json::from_value::<WhisplyToolAppServerCancelAcknowledgement>(
            value,
        )
        .is_ok_and(|acknowledgement| {
            acknowledgement.execution_id == execution.call().execution_id
                && acknowledgement.accepted
        }),
        Ok(Ok(Err(_))) | Ok(Err(_)) | Err(_) => false,
    };
    if !acknowledged {
        let _canceled = outgoing.cancel_request(&request_id).await;
    }
}

fn placeholder_envelope(
    execution: &ServerToolExecution,
    installation_id: &str,
) -> WhisplyExecutionEnvelope {
    let now_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0);
    let expires_at_ms = now_unix_ms.saturating_add(EXECUTION_TIMEOUT.as_millis() as i64);
    WhisplyExecutionEnvelope {
        account_epoch: PLACEHOLDER_UUID.to_string(),
        server_intent_grant_id: None,
        task_id: None,
        task_version: None,
        lease_token: None,
        lease_phase: None,
        device_id: PLACEHOLDER_UUID.to_string(),
        installation_id: installation_id.to_string(),
        thread_id: execution.thread_id().to_string(),
        turn_id: execution.turn_id().to_string(),
        activity_id: PLACEHOLDER_UUID.to_string(),
        confirmation_hash: None,
        origin_digest: None,
        content_digest: None,
        skill_run_id: None,
        expires_at_ms,
        nonce: PLACEHOLDER_NONCE.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn placeholder_envelope_uses_zero_identity_and_thread_correlation() {
        let execution = ServerToolExecution::new(
            codex_app_server_protocol::WhisplyToolModelCall {
                execution_id: "execution-12345678".to_string(),
                tool_id: "whisply.memory".to_string(),
                schema_version: 1,
                arguments: serde_json::json!({"operation": "list"}),
            },
            "thread-12345678".to_string(),
            "turn-12345678".to_string(),
        )
        .expect("execution");

        let envelope = placeholder_envelope(&execution, "install-12345678");
        assert_eq!(envelope.account_epoch, PLACEHOLDER_UUID);
        assert_eq!(envelope.device_id, PLACEHOLDER_UUID);
        assert_eq!(envelope.installation_id, "install-12345678");
        assert_eq!(envelope.thread_id, "thread-12345678");
        assert_eq!(envelope.turn_id, "turn-12345678");
        assert!(envelope.expires_at_ms > 0);
    }

    #[test]
    fn server_tool_admission_id_is_stable_for_a_turn() {
        assert_eq!(
            server_tool_admission_id("thread-1", "turn-1"),
            "server-tool:thread-1:turn-1"
        );
    }

    #[tokio::test]
    async fn progress_extends_an_in_flight_server_execution() {
        let store = ServerToolExecutionStore::default();
        let execution = ServerToolExecution::new(
            codex_app_server_protocol::WhisplyToolModelCall {
                execution_id: "execution-12345678".to_string(),
                tool_id: "whisply.memory".to_string(),
                schema_version: 1,
                arguments: serde_json::json!({"operation": "recall"}),
            },
            "thread-12345678".to_string(),
            "turn-12345678".to_string(),
        )
        .expect("execution");
        let connection_id = ConnectionId(7);
        assert!(
            store
                .begin_execution(connection_id, &execution)
                .await
                .is_some()
        );
        let before = store
            .execution_deadline(&execution)
            .await
            .expect("deadline");
        tokio::time::sleep(Duration::from_millis(5)).await;
        let published = store
            .report_progress(
                connection_id,
                &WhisplyToolProgressReportParams {
                    admission_id: server_tool_admission_id("thread-12345678", "turn-12345678"),
                    thread_id: "thread-12345678".to_string(),
                    turn_id: "turn-12345678".to_string(),
                    execution_id: "execution-12345678".to_string(),
                    label: "Reading saved memory".to_string(),
                    fraction: None,
                    icon_id: Some("memory".to_string()),
                },
            )
            .await;
        assert!(published);
        let after = store
            .execution_deadline(&execution)
            .await
            .expect("deadline");
        assert!(after >= before);
    }
}

//! App-server ownership and routing for one-turn native Whisply tool grants.
//!
//! A native UI owner registers an opaque, already-approved capability set for
//! a particular connection, thread, and user-message ID. Core receives only
//! the validated admission when that exact user message starts; model calls
//! are then sent back exclusively to the registering connection. This module
//! never stores an envelope, account identifier, target, browser state, file
//! grant, confirmation, or lease.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Weak;
use std::time::Duration;
use std::time::Instant;

use codex_app_server_protocol::ServerRequestPayload;
use codex_app_server_protocol::WhisplyToolAdmissionFinishParams;
use codex_app_server_protocol::WhisplyToolAdmissionFinishResponse;
use codex_app_server_protocol::WhisplyToolAdmissionRegisterParams;
use codex_app_server_protocol::WhisplyToolAdmissionRegisterResponse;
use codex_app_server_protocol::WhisplyToolAdmittedCancelAcknowledgement;
use codex_app_server_protocol::WhisplyToolAdmittedCancelRequest;
use codex_app_server_protocol::WhisplyToolAdmittedExecuteRequest;
use codex_app_server_protocol::WhisplyToolProgressReportParams;
use codex_app_server_protocol::WhisplyToolResult;
use futures::future::BoxFuture;
use thiserror::Error;
use tokio::sync::Mutex;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use whisply_core::first_party_tools::FirstPartyToolAdmission;
use whisply_core::first_party_tools::FirstPartyToolAdmissionError;
use whisply_core::first_party_tools::FirstPartyToolDispatchError;
use whisply_core::first_party_tools::FirstPartyToolDispatcher;
use whisply_core::first_party_tools::FirstPartyToolExecution;

use crate::outgoing_message::ConnectionId;
use crate::outgoing_message::OutgoingMessageSender;

/// Keep the server fallback lifetime aligned with the Mac owner's local
/// admission lifetime. Normal terminal cleanup uses the explicit finish RPC.
const ADMISSION_TTL: Duration = Duration::from_secs(/*secs*/ 120);
const EXECUTION_TIMEOUT: Duration = Duration::from_secs(/*secs*/ 120);
/// The longest an owner call may stay in flight even while reporting progress.
const MAX_EXECUTION_LIFETIME: Duration = Duration::from_secs(/*secs*/ 1_800);
const CANCELLATION_TIMEOUT: Duration = Duration::from_secs(/*secs*/ 5);
const MAX_ACTIVE_ADMISSIONS: usize = 128;
const MAX_ACTIVE_ADMISSIONS_PER_CONNECTION: usize = 16;
const MAX_CORRELATION_ID_BYTES: usize = 256;
const MAX_PROGRESS_LABEL_BYTES: usize = 240;

/// Stores opaque owner-issued grants until the corresponding turn finishes.
#[derive(Default)]
pub(crate) struct FirstPartyToolAdmissionStore {
    state: Mutex<AdmissionState>,
}

#[derive(Default)]
struct AdmissionState {
    by_id: HashMap<String, AdmissionRecord>,
}

struct AdmissionRecord {
    connection_id: ConnectionId,
    thread_id: String,
    admission: FirstPartyToolAdmission,
    expires_at: Instant,
    phase: AdmissionPhase,
    /// A terminal client release arrived while Core was still unwinding an
    /// owner call. Keep the ID reserved until that call finishes, then drop
    /// the record without requiring the client to race a second finish RPC.
    terminal_finish_requested: bool,
}

enum AdmissionPhase {
    Pending,
    Active {
        turn_id: Option<String>,
        in_flight: HashMap<String, ExecutionLiveness>,
    },
}

/// How long an in-flight owner call may still be waited on.
///
/// A native call is not always short. A browser task runs for as long as the
/// work takes, so a single fixed wait would abandon the call while the owner is
/// still doing exactly what it was asked to do — and the model would be told it
/// failed while the browser kept going. The owner keeps the wait alive by
/// reporting progress, which is a liveness signal rather than decoration: an
/// owner that has stopped saying anything is indistinguishable from one that
/// has died, and both should end the same way. The absolute lifetime is what
/// stops a talkative but stuck owner from pinning a turn open forever.
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
        // The absolute lifetime is a ceiling, not a suggestion: it has to be
        // able to pull the deadline back in, or an owner that reported once
        // near the end would keep the turn open past the limit.
        self.deadline = (now + EXECUTION_TIMEOUT).min(self.started_at + MAX_EXECUTION_LIFETIME);
    }
}

/// Reasons an app-server admission registration is rejected.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub(crate) enum FirstPartyToolAdmissionStoreError {
    /// The provided ID/tool set did not match the static native registry.
    #[error("invalid native first-party tool admission")]
    InvalidAdmission,
    /// The request did not name a bounded app-server thread.
    #[error("invalid native first-party tool thread id")]
    InvalidThreadId,
    /// This opaque admission ID is already active on this app-server.
    #[error("native first-party tool admission is already registered")]
    DuplicateAdmission,
    /// A connection can only hold one pending capability set per user message.
    #[error("native first-party tool admission already exists for this user message")]
    DuplicateUserMessage,
    /// The app-server refuses unbounded owner capability state.
    #[error("too many native first-party tool admissions are active")]
    CapacityExceeded,
}

impl FirstPartyToolAdmissionStore {
    /// Registers a native-owner approval before the corresponding turn starts.
    pub(crate) async fn register(
        &self,
        connection_id: ConnectionId,
        params: WhisplyToolAdmissionRegisterParams,
    ) -> Result<WhisplyToolAdmissionRegisterResponse, FirstPartyToolAdmissionStoreError> {
        if !is_bounded_identifier(&params.thread_id, MAX_CORRELATION_ID_BYTES) {
            return Err(FirstPartyToolAdmissionStoreError::InvalidThreadId);
        }
        let admission = FirstPartyToolAdmission::new(
            params.admission_id,
            params.client_user_message_id,
            params.tool_ids,
        )
        .map_err(map_admission_error)?;

        let now = Instant::now();
        let mut state = self.state.lock().await;
        state.purge_expired(now);
        if state.by_id.contains_key(admission.admission_id()) {
            return Err(FirstPartyToolAdmissionStoreError::DuplicateAdmission);
        }
        if state.by_id.len() >= MAX_ACTIVE_ADMISSIONS
            || state.connection_admission_count(connection_id)
                >= MAX_ACTIVE_ADMISSIONS_PER_CONNECTION
        {
            return Err(FirstPartyToolAdmissionStoreError::CapacityExceeded);
        }
        if state.by_id.values().any(|record| {
            record.connection_id == connection_id
                && record.thread_id == params.thread_id
                && record.admission.client_user_message_id() == admission.client_user_message_id()
        }) {
            return Err(FirstPartyToolAdmissionStoreError::DuplicateUserMessage);
        }

        state.by_id.insert(
            admission.admission_id().to_string(),
            AdmissionRecord {
                connection_id,
                thread_id: params.thread_id,
                admission,
                expires_at: now + ADMISSION_TTL,
                phase: AdmissionPhase::Pending,
                terminal_finish_requested: false,
            },
        );
        Ok(WhisplyToolAdmissionRegisterResponse { accepted: true })
    }

    /// Activates only the pending grant that exactly matches a new user turn.
    pub(crate) async fn take_for_turn(
        &self,
        connection_id: ConnectionId,
        thread_id: &str,
        client_user_message_id: Option<&str>,
    ) -> Option<FirstPartyToolAdmission> {
        let client_user_message_id = client_user_message_id?;
        let mut state = self.state.lock().await;
        state.purge_expired(Instant::now());
        let record = state.by_id.values_mut().find(|record| {
            record.connection_id == connection_id
                && record.thread_id == thread_id
                && record.admission.client_user_message_id() == client_user_message_id
                && matches!(&record.phase, AdmissionPhase::Pending)
        })?;
        record.phase = AdmissionPhase::Active {
            turn_id: None,
            in_flight: HashMap::new(),
        };
        Some(record.admission.clone())
    }

    /// Binds a model call to the same connection, user message, and turn.
    async fn begin_execution(&self, execution: &FirstPartyToolExecution) -> Option<ConnectionId> {
        let mut state = self.state.lock().await;
        state.purge_expired(Instant::now());
        let record = state.by_id.get_mut(execution.admission().admission_id())?;
        if record.thread_id != execution.thread_id()
            || &record.admission != execution.admission()
            || record.admission.client_user_message_id()
                != execution.admission().client_user_message_id()
        {
            return None;
        }
        let AdmissionPhase::Active { turn_id, in_flight } = &mut record.phase else {
            return None;
        };
        if turn_id
            .as_deref()
            .is_some_and(|active_turn_id| active_turn_id != execution.turn_id())
        {
            return None;
        }
        *turn_id = Some(execution.turn_id().to_string());
        let execution_id = execution.call().execution_id.clone();
        if in_flight.contains_key(&execution_id) {
            return None;
        }
        in_flight.insert(execution_id, ExecutionLiveness::new(Instant::now()));
        Some(record.connection_id)
    }

    /// Returns how long this exact in-flight call may still be waited on.
    ///
    /// The dispatcher asks again each time its current wait elapses, so a
    /// progress report that arrived in the meantime extends the wait instead of
    /// the call being abandoned mid-work.
    async fn execution_deadline(&self, execution: &FirstPartyToolExecution) -> Option<Instant> {
        let state = self.state.lock().await;
        let record = state.by_id.get(execution.admission().admission_id())?;
        let AdmissionPhase::Active { in_flight, .. } = &record.phase else {
            return None;
        };
        in_flight
            .get(&execution.call().execution_id)
            .map(|liveness| liveness.deadline)
    }

    /// Accepts a progress report only for a call that is running right now
    /// under this exact connection, thread, turn, and admission.
    ///
    /// The report cannot start a call, revive a finished one, or reach another
    /// connection's work. Its only effects are to keep the call's wait alive and
    /// to let the app-server publish one attributable line of activity.
    pub(crate) async fn report_progress(
        &self,
        connection_id: ConnectionId,
        params: &WhisplyToolProgressReportParams,
    ) -> bool {
        if !is_bounded_identifier(&params.admission_id, MAX_CORRELATION_ID_BYTES)
            || !is_bounded_identifier(&params.thread_id, MAX_CORRELATION_ID_BYTES)
            || !is_bounded_identifier(&params.turn_id, MAX_CORRELATION_ID_BYTES)
            || !is_bounded_identifier(&params.execution_id, MAX_CORRELATION_ID_BYTES)
            || !is_bounded_identifier(&params.label, MAX_PROGRESS_LABEL_BYTES)
            || params
                .fraction
                .is_some_and(|fraction| !(0.0..=1.0).contains(&fraction))
            // An icon travels to a surface that will draw it, so it is bounded
            // like every other identifier rather than passed through on trust.
            || params
                .icon_id
                .as_deref()
                .is_some_and(|icon| !is_bounded_identifier(icon, MAX_CORRELATION_ID_BYTES))
        {
            return false;
        }
        let now = Instant::now();
        let mut state = self.state.lock().await;
        let Some(record) = state.by_id.get_mut(&params.admission_id) else {
            return false;
        };
        if record.connection_id != connection_id || record.thread_id != params.thread_id {
            return false;
        }
        let AdmissionPhase::Active {
            turn_id: Some(active_turn_id),
            in_flight,
        } = &mut record.phase
        else {
            return false;
        };
        if active_turn_id != &params.turn_id {
            return false;
        }
        let Some(liveness) = in_flight.get_mut(&params.execution_id) else {
            return false;
        };
        liveness.extend(now);
        let execution_deadline = liveness.deadline;
        // The grant itself must outlive the call it is still carrying,
        // otherwise the periodic purge would drop the record mid-execution and
        // the terminal release would have nothing left to match.
        record.expires_at = record.expires_at.max(execution_deadline + ADMISSION_TTL);
        true
    }

    async fn finish_execution(&self, execution: &FirstPartyToolExecution) {
        let mut state = self.state.lock().await;
        let admission_id = execution.admission().admission_id().to_string();
        let Some(record) = state.by_id.get_mut(&admission_id) else {
            return;
        };
        let AdmissionPhase::Active { in_flight, .. } = &mut record.phase else {
            return;
        };
        in_flight.remove(&execution.call().execution_id);
        let release_after_execution = record.terminal_finish_requested && in_flight.is_empty();
        if release_after_execution {
            state.by_id.remove(&admission_id);
        }
    }

    /// Drops an activated admission when Core steers its user message into an
    /// already-running turn or declines to start that turn.
    pub(crate) async fn discard(&self, admission_id: &str) {
        self.state.lock().await.by_id.remove(admission_id);
    }

    /// Accepts terminal cleanup only after its exact owning connection,
    /// thread, turn, and client message all match. If Core is still unwinding
    /// an owner call, its admission ID stays reserved until that call exits;
    /// the accepted cleanup then releases it without a client retry.
    pub(crate) async fn finish_turn(
        &self,
        connection_id: ConnectionId,
        params: WhisplyToolAdmissionFinishParams,
    ) -> WhisplyToolAdmissionFinishResponse {
        if !is_bounded_identifier(&params.admission_id, MAX_CORRELATION_ID_BYTES)
            || !is_bounded_identifier(&params.thread_id, MAX_CORRELATION_ID_BYTES)
            || !is_bounded_identifier(&params.turn_id, MAX_CORRELATION_ID_BYTES)
            || !is_bounded_identifier(&params.client_user_message_id, MAX_CORRELATION_ID_BYTES)
        {
            return WhisplyToolAdmissionFinishResponse { released: false };
        }

        let mut state = self.state.lock().await;
        state.purge_expired(Instant::now());
        let mut remove_now = false;
        let released = state
            .by_id
            .get_mut(&params.admission_id)
            .is_some_and(|record| {
                let (active_turn_id, has_in_flight_execution) = match &record.phase {
                    AdmissionPhase::Active {
                        turn_id: Some(active_turn_id),
                        in_flight,
                    } => (active_turn_id, !in_flight.is_empty()),
                    AdmissionPhase::Pending | AdmissionPhase::Active { .. } => return false,
                };
                let exact_active_turn = record.connection_id == connection_id
                    && record.thread_id == params.thread_id
                    && record.admission.client_user_message_id() == params.client_user_message_id
                    && active_turn_id == &params.turn_id;
                if !exact_active_turn {
                    return false;
                }
                if has_in_flight_execution {
                    record.terminal_finish_requested = true;
                } else {
                    remove_now = true;
                }
                true
            });
        if remove_now {
            state.by_id.remove(&params.admission_id);
        }
        WhisplyToolAdmissionFinishResponse { released }
    }

    /// Revokes all pending and active opaque grants when their owner disconnects.
    pub(crate) async fn connection_closed(&self, connection_id: ConnectionId) {
        self.state
            .lock()
            .await
            .by_id
            .retain(|_, record| record.connection_id != connection_id);
    }
}

impl AdmissionState {
    fn purge_expired(&mut self, now: Instant) {
        self.by_id.retain(|_, record| record.expires_at > now);
    }

    fn connection_admission_count(&self, connection_id: ConnectionId) -> usize {
        self.by_id
            .values()
            .filter(|record| record.connection_id == connection_id)
            .count()
    }
}

fn map_admission_error(_: FirstPartyToolAdmissionError) -> FirstPartyToolAdmissionStoreError {
    FirstPartyToolAdmissionStoreError::InvalidAdmission
}

fn is_bounded_identifier(value: &str, maximum_bytes: usize) -> bool {
    !value.trim().is_empty() && value.len() <= maximum_bytes
}

/// Creates the Core dispatcher that can route an admitted model call only to
/// its original native-owner connection.
pub(crate) fn app_server_first_party_tool_dispatcher(
    outgoing: Arc<OutgoingMessageSender>,
    admissions: Arc<FirstPartyToolAdmissionStore>,
) -> Arc<dyn FirstPartyToolDispatcher> {
    Arc::new(AppServerFirstPartyToolDispatcher {
        outgoing: Arc::downgrade(&outgoing),
        admissions,
    })
}

struct AppServerFirstPartyToolDispatcher {
    outgoing: Weak<OutgoingMessageSender>,
    admissions: Arc<FirstPartyToolAdmissionStore>,
}

impl FirstPartyToolDispatcher for AppServerFirstPartyToolDispatcher {
    fn execute(
        &self,
        execution: FirstPartyToolExecution,
        cancellation: CancellationToken,
    ) -> BoxFuture<'static, Result<WhisplyToolResult, FirstPartyToolDispatchError>> {
        let outgoing = self.outgoing.clone();
        let admissions = Arc::clone(&self.admissions);
        Box::pin(async move {
            let Some(outgoing) = outgoing.upgrade() else {
                return Err(FirstPartyToolDispatchError::Unavailable);
            };
            let Some(connection_id) = admissions.begin_execution(&execution).await else {
                return Err(FirstPartyToolDispatchError::Unavailable);
            };
            let result = dispatch_execution(
                &outgoing,
                connection_id,
                &execution,
                cancellation,
                &admissions,
            )
            .await;
            admissions.finish_execution(&execution).await;
            result
        })
    }
}

async fn dispatch_execution(
    outgoing: &Arc<OutgoingMessageSender>,
    connection_id: ConnectionId,
    execution: &FirstPartyToolExecution,
    cancellation: CancellationToken,
    admissions: &FirstPartyToolAdmissionStore,
) -> Result<WhisplyToolResult, FirstPartyToolDispatchError> {
    let connection_ids = [connection_id];
    let (request_id, mut response) = outgoing
        .send_request_to_connections(
            Some(&connection_ids),
            ServerRequestPayload::WhisplyToolAdmittedExecute(WhisplyToolAdmittedExecuteRequest {
                admission_id: execution.admission().admission_id().to_string(),
                call: execution.call().clone(),
                thread_id: execution.thread_id().to_string(),
                turn_id: execution.turn_id().to_string(),
                client_user_message_id: execution.admission().client_user_message_id().to_string(),
            }),
            /*thread_id*/ None,
        )
        .await;

    loop {
        let wait = admissions
            .execution_deadline(execution)
            .await
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
            .unwrap_or(EXECUTION_TIMEOUT);
        tokio::select! {
            _ = cancellation.cancelled() => {
                let _canceled = outgoing.cancel_request(&request_id).await;
                send_cancellation(outgoing, connection_id, execution).await;
                return Err(FirstPartyToolDispatchError::Cancelled);
            }
            settled = timeout(wait, &mut response) => match settled {
                Ok(Ok(Ok(value))) => {
                    let result = serde_json::from_value::<WhisplyToolResult>(value)
                        .map_err(|_| FirstPartyToolDispatchError::Failed)?;
                    return (result.execution_id == execution.call().execution_id)
                        .then_some(result)
                        .ok_or(FirstPartyToolDispatchError::Failed);
                }
                Ok(Ok(Err(_))) => return Err(FirstPartyToolDispatchError::Failed),
                Ok(Err(_)) => return Err(FirstPartyToolDispatchError::Unavailable),
                Err(_) => {
                    // The wait elapsed. A progress report may have extended the
                    // call's deadline while this one was pending, in which case
                    // the owner is still working and gets the remaining time.
                    let still_live = admissions
                        .execution_deadline(execution)
                        .await
                        .is_some_and(|deadline| deadline > Instant::now());
                    if still_live {
                        continue;
                    }
                    let _canceled = outgoing.cancel_request(&request_id).await;
                    send_cancellation(outgoing, connection_id, execution).await;
                    return Err(FirstPartyToolDispatchError::Unavailable);
                }
            },
        }
    }
}

async fn send_cancellation(
    outgoing: &Arc<OutgoingMessageSender>,
    connection_id: ConnectionId,
    execution: &FirstPartyToolExecution,
) {
    let connection_ids = [connection_id];
    let (request_id, response) = outgoing
        .send_request_to_connections(
            Some(&connection_ids),
            ServerRequestPayload::WhisplyToolAdmittedCancel(WhisplyToolAdmittedCancelRequest {
                execution_id: execution.call().execution_id.clone(),
                admission_id: execution.admission().admission_id().to_string(),
                thread_id: execution.thread_id().to_string(),
                turn_id: execution.turn_id().to_string(),
                client_user_message_id: execution.admission().client_user_message_id().to_string(),
            }),
            /*thread_id*/ None,
        )
        .await;
    let acknowledged = match timeout(CANCELLATION_TIMEOUT, response).await {
        Ok(Ok(Ok(value))) => serde_json::from_value::<WhisplyToolAdmittedCancelAcknowledgement>(
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

#[cfg(test)]
#[path = "first_party_tool_admission_tests.rs"]
mod tests;

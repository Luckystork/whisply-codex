//! Redacted, zero-authority event streams for deterministic fixture threads.
//!
//! This module projects an already validated immutable fixture timeline into
//! bounded metadata. It does not launch a fixture host, access a live account,
//! open a connection, reserve Usage, execute a tool, or emit a persistent
//! event. Every adapter in the stream is explicitly fixture-only and a no-op.

use serde::Deserialize;
use serde::Serialize;
use thiserror::Error;

use crate::DiagnosticFixtureThreadLifecycle;
use crate::DiagnosticFixtureThreadPlanError;
use crate::DiagnosticFixtureThreadTimeline;
use crate::DiagnosticLane;
use crate::DiagnosticScenarioAuthority;
use crate::DiagnosticScenarioRegistry;
use crate::build_diagnostic_fixture_thread_timeline;

/// Schema revision for catalog-derived redacted fixture event streams.
pub const DIAGNOSTIC_FIXTURE_EVENT_SCHEMA_VERSION: u16 = 1;
/// Stable protocol for the redacted fixture-event envelope.
pub const DIAGNOSTIC_FIXTURE_EVENT_PROTOCOL: &str = "whisply.diagnostics.fixture-event.v1";
/// A stream contains the initial pending state plus at most one event per
/// catalog checkpoint.
pub const MAX_DIAGNOSTIC_FIXTURE_EVENTS: usize = 129;

/// Failure to derive or validate a redacted fixture event stream.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum DiagnosticFixtureEventError {
    #[error(transparent)]
    FixtureThread(#[from] DiagnosticFixtureThreadPlanError),
    #[error("The deterministic fixture event stream is invalid.")]
    InvalidStream,
}

/// The bounded categories whose fixture-only adapters are described by every
/// envelope. They are metadata categories, not provider or tool handles.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticFixtureAdapterKind {
    Network,
    Provider,
    Tool,
    Usage,
    Receipt,
    Policy,
    Compaction,
}

/// Adapter mode permitted inside deterministic presentation fixtures.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticFixtureAdapterMode {
    FixtureOnlyNoop,
}

/// One explicit no-op adapter contract. Every potentially authoritative
/// operation is false by construction and can be validated from the stream
/// without consulting a host, account, or provider.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiagnosticFixtureAdapterContract {
    pub kind: DiagnosticFixtureAdapterKind,
    pub mode: DiagnosticFixtureAdapterMode,
    pub allows_network: bool,
    pub allows_provider_request: bool,
    pub allows_tool_execution: bool,
    pub allows_usage_reservation: bool,
    pub allows_receipt_issuance: bool,
    pub allows_policy_override: bool,
    pub allows_persistence_mutation: bool,
}

/// The redacted event state, derived only from an immutable virtual timeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticFixtureEventKind {
    Pending,
    Checkpoint,
    Completed,
}

/// A bounded catalog checkpoint reference. It intentionally excludes visible
/// copy, screenshot bytes or paths, URLs, credentials, account IDs, and raw
/// event text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiagnosticFixtureEventCheckpoint {
    pub outcome_id: String,
    pub sequence: u16,
    pub step_id: String,
}

/// One versioned redacted event envelope. It is inspectable metadata only;
/// serializing it cannot perform or authorize product work.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiagnosticFixtureEventEnvelope {
    pub schema_version: u16,
    pub protocol: String,
    pub event_id: String,
    pub fixture_id: String,
    pub scenario_id: String,
    pub sequence: u16,
    pub at_monotonic_ms: u64,
    pub kind: DiagnosticFixtureEventKind,
    pub lifecycle: DiagnosticFixtureThreadLifecycle,
    pub checkpoint: Option<DiagnosticFixtureEventCheckpoint>,
    pub lane: DiagnosticLane,
    pub simulated: bool,
    pub billable: bool,
    pub authority: DiagnosticScenarioAuthority,
    pub adapters: Vec<DiagnosticFixtureAdapterContract>,
}

/// All redacted envelopes for one immutable fixture scenario.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiagnosticFixtureEventStream {
    pub schema_version: u16,
    pub protocol: String,
    pub fixture_id: String,
    pub scenario_id: String,
    pub events: Vec<DiagnosticFixtureEventEnvelope>,
}

impl DiagnosticFixtureEventStream {
    /// Rebuilds the exact catalog-derived stream and rejects every caller or
    /// post-construction change before a future fixture host can consume it.
    pub fn validate(
        &self,
        registry: &DiagnosticScenarioRegistry,
    ) -> Result<(), DiagnosticFixtureEventError> {
        let timeline = build_diagnostic_fixture_thread_timeline(registry, &self.scenario_id)?;
        if self != &Self::from_timeline(&timeline) {
            return Err(DiagnosticFixtureEventError::InvalidStream);
        }
        Ok(())
    }

    fn from_timeline(timeline: &DiagnosticFixtureThreadTimeline) -> Self {
        let events = timeline
            .projections
            .iter()
            .enumerate()
            .map(|(index, projection)| {
                let sequence = u16::try_from(index)
                    .expect("validated fixture timelines contain a bounded event count");
                let checkpoint = projection.current_checkpoint.as_ref().map(|outcome| {
                    DiagnosticFixtureEventCheckpoint {
                        outcome_id: outcome.outcome_id.clone(),
                        sequence: outcome.sequence,
                        step_id: outcome.step_id.clone(),
                    }
                });
                let kind = match projection.lifecycle {
                    DiagnosticFixtureThreadLifecycle::Pending => {
                        DiagnosticFixtureEventKind::Pending
                    }
                    DiagnosticFixtureThreadLifecycle::AtCheckpoint => {
                        DiagnosticFixtureEventKind::Checkpoint
                    }
                    DiagnosticFixtureThreadLifecycle::Completed => {
                        DiagnosticFixtureEventKind::Completed
                    }
                };

                DiagnosticFixtureEventEnvelope {
                    schema_version: DIAGNOSTIC_FIXTURE_EVENT_SCHEMA_VERSION,
                    protocol: DIAGNOSTIC_FIXTURE_EVENT_PROTOCOL.to_string(),
                    event_id: format!("{}-event-{sequence}", timeline.fixture_id),
                    fixture_id: timeline.fixture_id.clone(),
                    scenario_id: timeline.scenario_id.clone(),
                    sequence,
                    at_monotonic_ms: projection.at_monotonic_ms,
                    kind,
                    lifecycle: projection.lifecycle,
                    checkpoint,
                    lane: timeline.lane,
                    simulated: timeline.simulated,
                    billable: timeline.billable,
                    authority: timeline.authority.clone(),
                    adapters: fixture_only_adapter_contracts(),
                }
            })
            .collect();

        Self {
            schema_version: DIAGNOSTIC_FIXTURE_EVENT_SCHEMA_VERSION,
            protocol: DIAGNOSTIC_FIXTURE_EVENT_PROTOCOL.to_string(),
            fixture_id: timeline.fixture_id.clone(),
            scenario_id: timeline.scenario_id.clone(),
            events,
        }
    }
}

/// Builds the entire redacted event stream for one exact catalog scenario.
/// There is no API for arbitrary event input, endpoint, executable, account,
/// credential, file path, or action.
pub fn build_diagnostic_fixture_event_stream(
    registry: &DiagnosticScenarioRegistry,
    scenario_id: &str,
) -> Result<DiagnosticFixtureEventStream, DiagnosticFixtureEventError> {
    let timeline = build_diagnostic_fixture_thread_timeline(registry, scenario_id)?;
    if timeline.projections.len() > MAX_DIAGNOSTIC_FIXTURE_EVENTS {
        return Err(DiagnosticFixtureEventError::InvalidStream);
    }
    let stream = DiagnosticFixtureEventStream::from_timeline(&timeline);
    stream.validate(registry)?;
    Ok(stream)
}

fn fixture_only_adapter_contracts() -> Vec<DiagnosticFixtureAdapterContract> {
    [
        DiagnosticFixtureAdapterKind::Network,
        DiagnosticFixtureAdapterKind::Provider,
        DiagnosticFixtureAdapterKind::Tool,
        DiagnosticFixtureAdapterKind::Usage,
        DiagnosticFixtureAdapterKind::Receipt,
        DiagnosticFixtureAdapterKind::Policy,
        DiagnosticFixtureAdapterKind::Compaction,
    ]
    .into_iter()
    .map(|kind| DiagnosticFixtureAdapterContract {
        kind,
        mode: DiagnosticFixtureAdapterMode::FixtureOnlyNoop,
        allows_network: false,
        allows_provider_request: false,
        allows_tool_execution: false,
        allows_usage_reservation: false,
        allows_receipt_issuance: false,
        allows_policy_override: false,
        allows_persistence_mutation: false,
    })
    .collect()
}

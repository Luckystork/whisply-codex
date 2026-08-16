//! Catalog-attested execution input for deterministic presentation fixtures.
//!
//! This is the narrow handoff between the immutable Rust catalog and a native
//! fixture-only host. It contains the plan, virtual timeline, redacted event
//! stream, and synthetic selectors together so a host can reject a partial or
//! cross-scenario projection before it renders. It remains non-executable on
//! its own: no process, account, endpoint, path, credential, or adapter handle
//! can be supplied here.

use serde::Deserialize;
use serde::Serialize;
use thiserror::Error;

use crate::DiagnosticFixtureEventError;
use crate::DiagnosticFixtureEventStream;
use crate::DiagnosticFixtureSelectorError;
use crate::DiagnosticFixtureSelectors;
use crate::DiagnosticFixtureThreadPlan;
use crate::DiagnosticFixtureThreadPlanError;
use crate::DiagnosticFixtureThreadTimeline;
use crate::DiagnosticScenarioRegistry;
use crate::build_diagnostic_fixture_event_stream;
use crate::build_diagnostic_fixture_selectors;
use crate::build_diagnostic_fixture_thread_plan;
use crate::build_diagnostic_fixture_thread_timeline;

/// Schema revision for the typed catalog-to-host fixture handoff.
pub const DIAGNOSTIC_FIXTURE_EXECUTION_SCHEMA_VERSION: u16 = 1;
/// Stable protocol identifier for the typed catalog-to-host fixture handoff.
pub const DIAGNOSTIC_FIXTURE_EXECUTION_PROTOCOL: &str = "whisply.diagnostics.fixture-execution.v1";

/// Failure to derive or validate one catalog-attested fixture execution.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum DiagnosticFixtureExecutionError {
    #[error(transparent)]
    FixtureThread(#[from] DiagnosticFixtureThreadPlanError),
    #[error(transparent)]
    FixtureEvents(#[from] DiagnosticFixtureEventError),
    #[error(transparent)]
    FixtureSelectors(#[from] DiagnosticFixtureSelectorError),
    #[error("The deterministic fixture execution handoff is invalid.")]
    InvalidExecution,
}

/// The only native-host input for a registered deterministic fixture thread.
/// Every field is regenerated from one immutable catalog scenario before it
/// can cross the fixture-only IPC boundary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiagnosticFixtureExecution {
    pub schema_version: u16,
    pub protocol: String,
    pub plan: DiagnosticFixtureThreadPlan,
    pub timeline: DiagnosticFixtureThreadTimeline,
    pub events: DiagnosticFixtureEventStream,
    pub selectors: DiagnosticFixtureSelectors,
}

impl DiagnosticFixtureExecution {
    /// Regenerates every member from the immutable catalog and rejects all
    /// cross-scenario, scheduling, event, or selector drift before a native
    /// fixture host could consume the handoff.
    pub fn validate(
        &self,
        registry: &DiagnosticScenarioRegistry,
    ) -> Result<(), DiagnosticFixtureExecutionError> {
        let expected = Self::from_catalog(registry, &self.plan.scenario_id)?;
        if self != &expected {
            return Err(DiagnosticFixtureExecutionError::InvalidExecution);
        }
        Ok(())
    }

    /// Whether one redacted event is a catalog-attested capture point. The
    /// pending projection is not a named event; every later event is fixed by
    /// the immutable plan and may be selected without introducing a normal-app
    /// state or executable action.
    pub fn supports_named_capture_event(&self, event_id: &str) -> bool {
        self.events
            .events
            .iter()
            .skip(1)
            .any(|event| event.event_id == event_id)
    }

    fn from_catalog(
        registry: &DiagnosticScenarioRegistry,
        scenario_id: &str,
    ) -> Result<Self, DiagnosticFixtureExecutionError> {
        Ok(Self {
            schema_version: DIAGNOSTIC_FIXTURE_EXECUTION_SCHEMA_VERSION,
            protocol: DIAGNOSTIC_FIXTURE_EXECUTION_PROTOCOL.to_string(),
            plan: build_diagnostic_fixture_thread_plan(registry, scenario_id)?,
            timeline: build_diagnostic_fixture_thread_timeline(registry, scenario_id)?,
            events: build_diagnostic_fixture_event_stream(registry, scenario_id)?,
            selectors: build_diagnostic_fixture_selectors(registry, scenario_id)?,
        })
    }
}

/// Builds the sole valid native-host handoff for one exact catalog scenario.
/// It accepts no presentation mutation, account, model, tool, endpoint, path,
/// executable, credential, or side-effecting adapter input.
pub fn build_diagnostic_fixture_execution(
    registry: &DiagnosticScenarioRegistry,
    scenario_id: &str,
) -> Result<DiagnosticFixtureExecution, DiagnosticFixtureExecutionError> {
    let execution = DiagnosticFixtureExecution::from_catalog(registry, scenario_id)?;
    execution.validate(registry)?;
    Ok(execution)
}

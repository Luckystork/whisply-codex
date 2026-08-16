//! Pure virtual-time reduction for deterministic presentation-fixture plans.
//!
//! A timeline is an inspectable projection of an already validated fixture
//! plan. It neither launches the Swift fixture host nor opens a connection,
//! reads a real account, or performs an action. Its only clock is the fixed
//! virtual monotonic clock embedded in the catalog-derived plan.

use serde::Deserialize;
use serde::Serialize;

use crate::DiagnosticLane;
use crate::DiagnosticScenarioAuthority;
use crate::DiagnosticScenarioRegistry;
use crate::DiagnosticVirtualClock;
use crate::diagnostic_fixture_thread::DiagnosticFixtureThreadPlan;
use crate::diagnostic_fixture_thread::DiagnosticFixtureThreadPlanError;
use crate::diagnostic_fixture_thread::DiagnosticScheduledOutcome;
use crate::diagnostic_fixture_thread::build_diagnostic_fixture_thread_plan;

/// Schema revision for pure virtual fixture-thread timelines.
pub const DIAGNOSTIC_FIXTURE_THREAD_TIMELINE_SCHEMA_VERSION: u16 = 1;
/// Stable protocol for virtual fixture-thread timeline projections.
pub const DIAGNOSTIC_FIXTURE_THREAD_TIMELINE_PROTOCOL: &str =
    "whisply.diagnostics.fixture-timeline.v1";

/// A virtual fixture-thread lifecycle state. These states describe only the
/// catalog schedule; they do not reveal an operating-system process state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticFixtureThreadLifecycle {
    Pending,
    AtCheckpoint,
    Completed,
}

/// A deterministic view of a fixture thread at one virtual monotonic instant.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiagnosticFixtureThreadProjection {
    /// Milliseconds since the fixed virtual monotonic origin.
    pub at_monotonic_ms: u64,
    pub lifecycle: DiagnosticFixtureThreadLifecycle,
    pub completed_checkpoint_count: u16,
    pub current_checkpoint: Option<DiagnosticScheduledOutcome>,
    pub next_checkpoint: Option<DiagnosticScheduledOutcome>,
}

/// All stable virtual checkpoints for one catalog-derived fixture plan.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiagnosticFixtureThreadTimeline {
    pub schema_version: u16,
    pub protocol: String,
    pub fixture_id: String,
    pub scenario_id: String,
    pub clock: DiagnosticVirtualClock,
    pub lane: DiagnosticLane,
    pub simulated: bool,
    pub billable: bool,
    pub authority: DiagnosticScenarioAuthority,
    /// One pending projection plus one projection at every registered
    /// checkpoint. This is metadata only, not an emitted event transcript.
    pub projections: Vec<DiagnosticFixtureThreadProjection>,
}

/// Reduces a validated fixture plan to one virtual-time projection. The
/// provided time cannot change the plan, grant authority, or run a fixture.
pub fn project_diagnostic_fixture_thread_plan(
    registry: &DiagnosticScenarioRegistry,
    plan: &DiagnosticFixtureThreadPlan,
    at_monotonic_ms: u64,
) -> Result<DiagnosticFixtureThreadProjection, DiagnosticFixtureThreadPlanError> {
    plan.validate(registry)?;
    Ok(project_validated_fixture_thread_plan(plan, at_monotonic_ms))
}

/// Builds every stable virtual-time projection for one exact catalog entry.
/// It accepts only the registered scenario id and returns no executable or
/// external target data.
pub fn build_diagnostic_fixture_thread_timeline(
    registry: &DiagnosticScenarioRegistry,
    scenario_id: &str,
) -> Result<DiagnosticFixtureThreadTimeline, DiagnosticFixtureThreadPlanError> {
    let plan = build_diagnostic_fixture_thread_plan(registry, scenario_id)?;
    let mut projections = Vec::with_capacity(plan.scheduled_outcomes.len() + 1);
    projections.push(project_validated_fixture_thread_plan(&plan, 0));
    projections.extend(
        plan.scheduled_outcomes
            .iter()
            .map(|outcome| project_validated_fixture_thread_plan(&plan, outcome.at_monotonic_ms)),
    );

    Ok(DiagnosticFixtureThreadTimeline {
        schema_version: DIAGNOSTIC_FIXTURE_THREAD_TIMELINE_SCHEMA_VERSION,
        protocol: DIAGNOSTIC_FIXTURE_THREAD_TIMELINE_PROTOCOL.to_string(),
        fixture_id: plan.fixture_id,
        scenario_id: plan.scenario_id,
        clock: plan.clock,
        lane: plan.lane,
        simulated: plan.simulated,
        billable: plan.billable,
        authority: plan.authority,
        projections,
    })
}

fn project_validated_fixture_thread_plan(
    plan: &DiagnosticFixtureThreadPlan,
    at_monotonic_ms: u64,
) -> DiagnosticFixtureThreadProjection {
    let completed_checkpoint_count = plan
        .scheduled_outcomes
        .iter()
        .take_while(|outcome| outcome.at_monotonic_ms <= at_monotonic_ms)
        .count();
    let completed_checkpoint_count = u16::try_from(completed_checkpoint_count)
        .expect("validated fixture plans contain a bounded checkpoint count");
    let current_checkpoint = completed_checkpoint_count
        .checked_sub(1)
        .and_then(|index| plan.scheduled_outcomes.get(usize::from(index)))
        .cloned();
    let next_checkpoint = plan
        .scheduled_outcomes
        .get(usize::from(completed_checkpoint_count))
        .cloned();
    let lifecycle = if completed_checkpoint_count == 0 {
        DiagnosticFixtureThreadLifecycle::Pending
    } else if next_checkpoint.is_some() {
        DiagnosticFixtureThreadLifecycle::AtCheckpoint
    } else {
        DiagnosticFixtureThreadLifecycle::Completed
    };

    DiagnosticFixtureThreadProjection {
        at_monotonic_ms,
        lifecycle,
        completed_checkpoint_count,
        current_checkpoint,
        next_checkpoint,
    }
}

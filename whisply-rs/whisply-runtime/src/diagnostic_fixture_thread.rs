//! Deterministic, non-executable plans for presentation-fixture threads.
//!
//! A fixture-thread plan is catalog-derived metadata, not a launch request.
//! It has no process path, endpoint, account selector, credential, storage
//! root, or side-effecting operation. A future fixture host must validate this
//! plan again before rendering any actual Swift surface.

use serde::Deserialize;
use serde::Serialize;
use thiserror::Error;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::DiagnosticLane;
use crate::DiagnosticScenario;
use crate::DiagnosticScenarioAuthority;
use crate::DiagnosticScenarioFixture;
use crate::DiagnosticScenarioRegistry;

/// Schema revision for catalog-derived fixture-thread plans.
pub const DIAGNOSTIC_FIXTURE_THREAD_SCHEMA_VERSION: u16 = 1;
/// Stable protocol for an immutable fixture-thread plan.
pub const DIAGNOSTIC_FIXTURE_THREAD_PROTOCOL: &str = "whisply.diagnostics.fixture-thread.v1";
/// Fixed virtual monotonic interval between named scenario checkpoints.
pub const DIAGNOSTIC_FIXTURE_THREAD_STEP_INTERVAL_MS: u64 = 1_000;
/// The registry permits at most this many planned fixture checkpoints.
pub const MAX_DIAGNOSTIC_FIXTURE_THREAD_OUTCOMES: usize = 128;

/// Failure to derive or validate a deterministic fixture-thread plan.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum DiagnosticFixtureThreadPlanError {
    #[error("The immutable diagnostic scenario registry is invalid.")]
    InvalidRegistry,
    #[error("Unknown registered diagnostic scenario.")]
    UnknownScenario,
    #[error("The immutable diagnostic fixture-thread plan is invalid.")]
    InvalidPlan,
}

/// Fixed wall/monotonic clock origin for a deterministic fixture thread.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiagnosticVirtualClock {
    /// The catalogued RFC3339 wall-clock origin. It is never read from the
    /// host clock at execution time.
    pub wall_clock: String,
    /// A virtual monotonic origin. It is fixed at zero for every fixture plan.
    pub monotonic_origin_ms: u64,
}

/// The only currently-supported scheduled fixture outcome: a checkpoint that
/// a future fixture-only reducer may present. It is not an action, result, or
/// authority decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticScheduledOutcomeKind {
    FixtureCheckpoint,
}

/// One catalog-derived scheduled checkpoint on a virtual monotonic clock.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiagnosticScheduledOutcome {
    /// One-based, contiguous order within the immutable scenario plan.
    pub sequence: u16,
    /// Deterministic identity derived only from scenario id, version, seed,
    /// and sequence; it is not an account, task, or process identifier.
    pub outcome_id: String,
    /// Milliseconds from the fixed virtual monotonic origin.
    pub at_monotonic_ms: u64,
    /// The registered scenario step this checkpoint represents.
    pub step_id: String,
    pub kind: DiagnosticScheduledOutcomeKind,
}

/// Immutable, zero-authority schedule for a deterministic presentation
/// fixture thread. Constructing or printing it never launches a fixture.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiagnosticFixtureThreadPlan {
    pub schema_version: u16,
    pub protocol: String,
    pub runner_version: String,
    pub fixture_id: String,
    pub scenario_id: String,
    pub scenario_version: u32,
    pub seed: u32,
    pub clock: DiagnosticVirtualClock,
    pub lane: DiagnosticLane,
    pub simulated: bool,
    pub billable: bool,
    pub authority: DiagnosticScenarioAuthority,
    pub fixture: DiagnosticScenarioFixture,
    pub scheduled_outcomes: Vec<DiagnosticScheduledOutcome>,
}

impl DiagnosticFixtureThreadPlan {
    /// Validates this plan against the immutable catalog. The plan must be an
    /// exact deterministic projection of a registered zero-authority scenario,
    /// rather than caller-provided executable fixture instructions.
    pub fn validate(
        &self,
        registry: &DiagnosticScenarioRegistry,
    ) -> Result<(), DiagnosticFixtureThreadPlanError> {
        registry
            .validate()
            .map_err(|_| DiagnosticFixtureThreadPlanError::InvalidRegistry)?;
        let scenario = registry
            .scenarios
            .iter()
            .find(|scenario| scenario.id == self.scenario_id)
            .ok_or(DiagnosticFixtureThreadPlanError::UnknownScenario)?;

        if self != &Self::from_registered_scenario(registry, scenario) {
            return Err(DiagnosticFixtureThreadPlanError::InvalidPlan);
        }
        Ok(())
    }

    fn from_registered_scenario(
        registry: &DiagnosticScenarioRegistry,
        scenario: &DiagnosticScenario,
    ) -> Self {
        let fixture_id = fixture_id_for(scenario);
        let scheduled_outcomes = scenario
            .steps
            .iter()
            .enumerate()
            .map(|(index, step_id)| {
                let sequence = u16::try_from(index + 1)
                    .expect("validated registry limits fixture checkpoints to u16");
                DiagnosticScheduledOutcome {
                    sequence,
                    outcome_id: format!("{fixture_id}-checkpoint-{sequence}"),
                    at_monotonic_ms: u64::from(sequence)
                        .saturating_mul(DIAGNOSTIC_FIXTURE_THREAD_STEP_INTERVAL_MS),
                    step_id: step_id.clone(),
                    kind: DiagnosticScheduledOutcomeKind::FixtureCheckpoint,
                }
            })
            .collect();

        Self {
            schema_version: DIAGNOSTIC_FIXTURE_THREAD_SCHEMA_VERSION,
            protocol: DIAGNOSTIC_FIXTURE_THREAD_PROTOCOL.to_string(),
            runner_version: registry.runner_version.clone(),
            fixture_id,
            scenario_id: scenario.id.clone(),
            scenario_version: scenario.version,
            seed: scenario.defaults.seed,
            clock: DiagnosticVirtualClock {
                wall_clock: scenario.defaults.clock.clone(),
                monotonic_origin_ms: 0,
            },
            lane: DiagnosticLane::PresentationFixture,
            simulated: true,
            billable: false,
            authority: scenario.authority.clone(),
            fixture: scenario.fixture.clone(),
            scheduled_outcomes,
        }
    }
}

/// Builds an immutable deterministic plan for one registered fixture scenario.
/// The only caller input is an exact catalog id. It cannot choose a path,
/// executable, account, endpoint, or any real authority.
pub fn build_diagnostic_fixture_thread_plan(
    registry: &DiagnosticScenarioRegistry,
    scenario_id: &str,
) -> Result<DiagnosticFixtureThreadPlan, DiagnosticFixtureThreadPlanError> {
    registry
        .validate()
        .map_err(|_| DiagnosticFixtureThreadPlanError::InvalidRegistry)?;
    let scenario = registry
        .scenarios
        .iter()
        .find(|scenario| scenario.id == scenario_id)
        .ok_or(DiagnosticFixtureThreadPlanError::UnknownScenario)?;
    if scenario.steps.len() > MAX_DIAGNOSTIC_FIXTURE_THREAD_OUTCOMES
        || OffsetDateTime::parse(&scenario.defaults.clock, &Rfc3339).is_err()
    {
        return Err(DiagnosticFixtureThreadPlanError::InvalidPlan);
    }

    let plan = DiagnosticFixtureThreadPlan::from_registered_scenario(registry, scenario);
    plan.validate(registry)?;
    Ok(plan)
}

fn fixture_id_for(scenario: &DiagnosticScenario) -> String {
    format!(
        "fixture-{}-v{}-seed{}",
        scenario.id, scenario.version, scenario.defaults.seed
    )
}

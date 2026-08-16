//! Bounded replay reduction for catalog-attested presentation fixtures.
//!
//! Replay controls reduce only the already validated virtual fixture timeline.
//! They cannot change a scenario, inject an event, name a host, open a
//! connection, or reach any product authority. The resulting state is a
//! deterministic capture projection that the isolated fixture surface may
//! render after independently validating it against the same execution.

use serde::Deserialize;
use serde::Serialize;
use thiserror::Error;

use crate::DiagnosticFixtureExecution;
use crate::DiagnosticFixtureExecutionError;
use crate::DiagnosticFixtureThreadProjection;
use crate::DiagnosticScenarioRegistry;
use crate::build_diagnostic_fixture_execution;

/// Schema revision for catalog-attested fixture replay states.
pub const DIAGNOSTIC_FIXTURE_REPLAY_SCHEMA_VERSION: u16 = 1;
/// Stable protocol for deterministic fixture replay reduction.
pub const DIAGNOSTIC_FIXTURE_REPLAY_PROTOCOL: &str = "whisply.diagnostics.fixture-replay.v1";
/// The reducer accepts a small, explicit control sequence only.
pub const MAX_DIAGNOSTIC_FIXTURE_REPLAY_CONTROLS: usize = 32;

/// The closed set of virtual playback rates. Rates affect only fixture replay
/// presentation; they never alter the catalog schedule or host clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticFixtureReplaySpeed {
    Half,
    Normal,
    Double,
    Quadruple,
}

impl Default for DiagnosticFixtureReplaySpeed {
    fn default() -> Self {
        Self::Normal
    }
}

/// One closed deterministic replay operation. `Step` deliberately requires a
/// paused replay so it cannot accidentally stand in for an ordinary running
/// application action.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DiagnosticFixtureReplayControl {
    Pause,
    Resume,
    Step,
    Seek { checkpoint_index: u16 },
    Speed { speed: DiagnosticFixtureReplaySpeed },
}

/// A fully derived replay state for one immutable fixture execution. The
/// projection is copied from the already generated virtual timeline rather
/// than being caller-provided render data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiagnosticFixtureReplayState {
    pub schema_version: u16,
    pub protocol: String,
    pub fixture_id: String,
    pub scenario_id: String,
    /// Zero means the pending projection. Each later value is the matching
    /// completed-checkpoint count in the immutable timeline.
    pub checkpoint_index: u16,
    pub paused: bool,
    pub speed: DiagnosticFixtureReplaySpeed,
    pub projection: DiagnosticFixtureThreadProjection,
}

impl DiagnosticFixtureReplayState {
    /// Checks the replay state against the exact execution already accepted by
    /// the fixture lane. This is intentionally independent of normal product
    /// state, clocks, accounts, and authority handles.
    pub fn validate_for_execution(
        &self,
        execution: &DiagnosticFixtureExecution,
    ) -> Result<(), DiagnosticFixtureReplayError> {
        let projection = execution
            .timeline
            .projections
            .get(usize::from(self.checkpoint_index))
            .ok_or(DiagnosticFixtureReplayError::InvalidReplay)?;
        if self.schema_version != DIAGNOSTIC_FIXTURE_REPLAY_SCHEMA_VERSION
            || self.protocol != DIAGNOSTIC_FIXTURE_REPLAY_PROTOCOL
            || self.fixture_id != execution.plan.fixture_id
            || self.scenario_id != execution.plan.scenario_id
            || self.projection != *projection
        {
            return Err(DiagnosticFixtureReplayError::InvalidReplay);
        }
        Ok(())
    }

    /// Returns whether the state is suitable for the already validated
    /// catalog-attested execution. This small convenience keeps callers from
    /// treating a malformed replay payload as a presentation choice.
    pub fn is_valid_for_execution(&self, execution: &DiagnosticFixtureExecution) -> bool {
        self.validate_for_execution(execution).is_ok()
    }
}

/// Failure to construct or reduce a deterministic fixture replay state.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum DiagnosticFixtureReplayError {
    #[error(transparent)]
    FixtureExecution(#[from] DiagnosticFixtureExecutionError),
    #[error("The deterministic fixture replay control sequence is too large.")]
    TooManyControls,
    #[error("Fixture replay step requires the replay to be paused.")]
    StepRequiresPause,
    #[error("Fixture replay seek is outside the immutable fixture timeline.")]
    InvalidSeek,
    #[error("The deterministic fixture replay state is invalid.")]
    InvalidReplay,
}

/// Builds the initial paused-or-playing state for an already validated
/// execution. The initial replay is playing at the catalog's pending view.
pub fn initial_diagnostic_fixture_replay(
    execution: &DiagnosticFixtureExecution,
) -> Result<DiagnosticFixtureReplayState, DiagnosticFixtureReplayError> {
    let projection = execution
        .timeline
        .projections
        .first()
        .cloned()
        .ok_or(DiagnosticFixtureReplayError::InvalidReplay)?;
    let state = DiagnosticFixtureReplayState {
        schema_version: DIAGNOSTIC_FIXTURE_REPLAY_SCHEMA_VERSION,
        protocol: DIAGNOSTIC_FIXTURE_REPLAY_PROTOCOL.to_string(),
        fixture_id: execution.plan.fixture_id.clone(),
        scenario_id: execution.plan.scenario_id.clone(),
        checkpoint_index: 0,
        paused: false,
        speed: DiagnosticFixtureReplaySpeed::Normal,
        projection,
    };
    state.validate_for_execution(execution)?;
    Ok(state)
}

/// Reduces a bounded ordered control list through the exact virtual timeline.
/// The execution is regenerated/validated against the immutable registry
/// before controls are applied, so the reducer cannot become a generic replay
/// source for arbitrary events or a mutation route into the normal app.
pub fn reduce_diagnostic_fixture_replay(
    registry: &DiagnosticScenarioRegistry,
    execution: &DiagnosticFixtureExecution,
    controls: &[DiagnosticFixtureReplayControl],
) -> Result<DiagnosticFixtureReplayState, DiagnosticFixtureReplayError> {
    if controls.len() > MAX_DIAGNOSTIC_FIXTURE_REPLAY_CONTROLS {
        return Err(DiagnosticFixtureReplayError::TooManyControls);
    }
    execution.validate(registry)?;
    let mut state = initial_diagnostic_fixture_replay(execution)?;
    let maximum_checkpoint_index = u16::try_from(
        execution
            .timeline
            .projections
            .len()
            .checked_sub(1)
            .ok_or(DiagnosticFixtureReplayError::InvalidReplay)?,
    )
    .map_err(|_| DiagnosticFixtureReplayError::InvalidReplay)?;

    for control in controls {
        match control {
            DiagnosticFixtureReplayControl::Pause => state.paused = true,
            DiagnosticFixtureReplayControl::Resume => state.paused = false,
            DiagnosticFixtureReplayControl::Step => {
                if !state.paused {
                    return Err(DiagnosticFixtureReplayError::StepRequiresPause);
                }
                state.checkpoint_index = state
                    .checkpoint_index
                    .saturating_add(1)
                    .min(maximum_checkpoint_index);
            }
            DiagnosticFixtureReplayControl::Seek { checkpoint_index } => {
                if *checkpoint_index > maximum_checkpoint_index {
                    return Err(DiagnosticFixtureReplayError::InvalidSeek);
                }
                state.checkpoint_index = *checkpoint_index;
            }
            DiagnosticFixtureReplayControl::Speed { speed } => state.speed = *speed,
        }
        state.projection =
            execution.timeline.projections[usize::from(state.checkpoint_index)].clone();
    }

    state.validate_for_execution(execution)?;
    Ok(state)
}

/// Builds the sole deterministic replay state for one registered fixture
/// scenario. It accepts no caller-provided plan, event stream, target, path,
/// account, provider, or authority input.
pub fn build_diagnostic_fixture_replay(
    registry: &DiagnosticScenarioRegistry,
    scenario_id: &str,
    controls: &[DiagnosticFixtureReplayControl],
) -> Result<DiagnosticFixtureReplayState, DiagnosticFixtureReplayError> {
    let execution = build_diagnostic_fixture_execution(registry, scenario_id)?;
    reduce_diagnostic_fixture_replay(registry, &execution, controls)
}

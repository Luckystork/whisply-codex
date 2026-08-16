use crate::DiagnosticFixtureReplayControl;
use crate::DiagnosticFixtureReplayError;
use crate::DiagnosticFixtureReplaySpeed;
use crate::build_diagnostic_fixture_execution;
use crate::build_diagnostic_fixture_replay;
use crate::diagnostic_fixture_thread_tests::SCENARIO_REGISTRY;
use crate::parse_diagnostic_scenario_registry;
use crate::reduce_diagnostic_fixture_replay;

fn registry() -> crate::DiagnosticScenarioRegistry {
    parse_diagnostic_scenario_registry(SCENARIO_REGISTRY)
        .expect("canonical scenario registry must validate")
}

#[test]
fn fixture_replay_reduces_pause_step_seek_and_speed_against_the_exact_timeline() {
    let registry = registry();
    let execution = build_diagnostic_fixture_execution(&registry, "browser-work-success")
        .expect("catalog-attested fixture execution");
    let controls = [
        DiagnosticFixtureReplayControl::Pause,
        DiagnosticFixtureReplayControl::Step,
        DiagnosticFixtureReplayControl::Seek {
            checkpoint_index: 2,
        },
        DiagnosticFixtureReplayControl::Speed {
            speed: DiagnosticFixtureReplaySpeed::Quadruple,
        },
    ];

    let replay = reduce_diagnostic_fixture_replay(&registry, &execution, &controls)
        .expect("bounded deterministic fixture replay");

    assert!(replay.paused);
    assert_eq!(replay.speed, DiagnosticFixtureReplaySpeed::Quadruple);
    assert_eq!(replay.checkpoint_index, 2);
    assert_eq!(
        replay.projection,
        execution.timeline.projections[usize::from(replay.checkpoint_index)]
    );
    assert!(replay.is_valid_for_execution(&execution));
}

#[test]
fn fixture_replay_rejects_running_step_and_out_of_range_seek() {
    let registry = registry();
    let execution = build_diagnostic_fixture_execution(&registry, "browser-work-success")
        .expect("catalog-attested fixture execution");

    assert_eq!(
        reduce_diagnostic_fixture_replay(
            &registry,
            &execution,
            &[DiagnosticFixtureReplayControl::Step]
        ),
        Err(DiagnosticFixtureReplayError::StepRequiresPause)
    );
    assert_eq!(
        reduce_diagnostic_fixture_replay(
            &registry,
            &execution,
            &[DiagnosticFixtureReplayControl::Seek {
                checkpoint_index: 99,
            }]
        ),
        Err(DiagnosticFixtureReplayError::InvalidSeek)
    );
}

#[test]
fn fixture_replay_rejects_tampered_state_and_oversized_control_sequences() {
    let registry = registry();
    let execution = build_diagnostic_fixture_execution(&registry, "browser-work-success")
        .expect("catalog-attested fixture execution");
    let mut replay = build_diagnostic_fixture_replay(&registry, "browser-work-success", &[])
        .expect("initial deterministic fixture replay");
    replay.fixture_id = "other-fixture".to_string();
    assert!(!replay.is_valid_for_execution(&execution));

    let controls = vec![DiagnosticFixtureReplayControl::Pause; 33];
    assert_eq!(
        reduce_diagnostic_fixture_replay(&registry, &execution, &controls),
        Err(DiagnosticFixtureReplayError::TooManyControls)
    );
}

use crate::DIAGNOSTIC_FIXTURE_THREAD_PROTOCOL;
use crate::DIAGNOSTIC_FIXTURE_THREAD_SCHEMA_VERSION;
use crate::DIAGNOSTIC_FIXTURE_THREAD_STEP_INTERVAL_MS;
use crate::DiagnosticFixtureThreadPlanError;
use crate::DiagnosticLane;
use crate::DiagnosticScheduledOutcomeKind;
use crate::build_diagnostic_fixture_thread_plan;
use crate::parse_diagnostic_scenario_registry;

pub(crate) const SCENARIO_REGISTRY: &str =
    include_str!("../../../../../docs/contextual-action-layer/diagnostic-scenarios.json");

#[test]
fn fixture_thread_plan_derives_fixed_virtual_time_and_zero_authority_from_the_catalog() {
    let registry = parse_diagnostic_scenario_registry(SCENARIO_REGISTRY)
        .expect("canonical scenario registry must validate");
    let scenario = registry
        .scenarios
        .iter()
        .find(|scenario| scenario.id == "browser-work-success")
        .expect("registered scenario");

    let plan = build_diagnostic_fixture_thread_plan(&registry, &scenario.id)
        .expect("catalog-derived fixture plan");

    assert_eq!(
        plan.schema_version,
        DIAGNOSTIC_FIXTURE_THREAD_SCHEMA_VERSION
    );
    assert_eq!(plan.protocol, DIAGNOSTIC_FIXTURE_THREAD_PROTOCOL);
    assert_eq!(plan.fixture_id, "fixture-browser-work-success-v1-seed4101");
    assert_eq!(plan.lane, DiagnosticLane::PresentationFixture);
    assert!(plan.simulated);
    assert!(!plan.billable);
    assert_eq!(plan.authority.commercial_authority, "none");
    assert_eq!(plan.authority.network, "forbidden");
    assert!(!plan.fixture.can_mint_authority);
    assert!(!plan.fixture.can_reserve_usage);
    assert!(!plan.fixture.can_reach_provider);
    assert_eq!(plan.clock.wall_clock, scenario.defaults.clock);
    assert_eq!(plan.clock.monotonic_origin_ms, 0);
    assert_eq!(plan.scheduled_outcomes.len(), scenario.steps.len());
    assert!(
        plan.scheduled_outcomes
            .iter()
            .enumerate()
            .all(|(index, outcome)| {
                outcome.sequence == u16::try_from(index + 1).expect("small fixture schedule")
                    && outcome.at_monotonic_ms
                        == u64::try_from(index + 1)
                            .expect("small fixture schedule")
                            .saturating_mul(DIAGNOSTIC_FIXTURE_THREAD_STEP_INTERVAL_MS)
                    && outcome.step_id == scenario.steps[index]
                    && outcome.kind == DiagnosticScheduledOutcomeKind::FixtureCheckpoint
            })
    );
    plan.validate(&registry)
        .expect("derived plan must remain catalog-bound");
}

#[test]
fn fixture_thread_plan_rejects_schedule_drift_before_a_fixture_can_run() {
    let registry = parse_diagnostic_scenario_registry(SCENARIO_REGISTRY)
        .expect("canonical scenario registry must validate");
    let mut plan = build_diagnostic_fixture_thread_plan(&registry, "browser-work-success")
        .expect("catalog-derived fixture plan");
    plan.scheduled_outcomes[0].at_monotonic_ms = 0;

    assert_eq!(
        plan.validate(&registry),
        Err(DiagnosticFixtureThreadPlanError::InvalidPlan)
    );
}

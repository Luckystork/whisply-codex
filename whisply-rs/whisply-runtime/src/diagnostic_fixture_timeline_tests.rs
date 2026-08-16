use pretty_assertions::assert_eq;

use crate::DIAGNOSTIC_FIXTURE_THREAD_TIMELINE_PROTOCOL;
use crate::DIAGNOSTIC_FIXTURE_THREAD_TIMELINE_SCHEMA_VERSION;
use crate::DiagnosticFixtureThreadLifecycle;
use crate::DiagnosticFixtureThreadPlanError;
use crate::DiagnosticScheduledOutcomeKind;
use crate::build_diagnostic_fixture_thread_plan;
use crate::build_diagnostic_fixture_thread_timeline;
use crate::diagnostic_fixture_thread_tests::SCENARIO_REGISTRY;
use crate::parse_diagnostic_scenario_registry;
use crate::project_diagnostic_fixture_thread_plan;

#[test]
fn fixture_thread_timeline_reduces_each_catalog_checkpoint_on_virtual_time() {
    let registry = parse_diagnostic_scenario_registry(SCENARIO_REGISTRY)
        .expect("canonical scenario registry must validate");
    let timeline = build_diagnostic_fixture_thread_timeline(&registry, "browser-work-success")
        .expect("catalog-derived fixture timeline");

    assert_eq!(
        timeline.schema_version,
        DIAGNOSTIC_FIXTURE_THREAD_TIMELINE_SCHEMA_VERSION
    );
    assert_eq!(
        timeline.protocol,
        DIAGNOSTIC_FIXTURE_THREAD_TIMELINE_PROTOCOL
    );
    assert_eq!(
        timeline.fixture_id,
        "fixture-browser-work-success-v1-seed4101"
    );
    assert!(timeline.simulated);
    assert!(!timeline.billable);
    assert_eq!(timeline.authority.commercial_authority, "none");
    assert_eq!(timeline.authority.network, "forbidden");
    assert_eq!(timeline.projections.len(), 4);

    let initial = &timeline.projections[0];
    assert_eq!(initial.at_monotonic_ms, 0);
    assert_eq!(initial.lifecycle, DiagnosticFixtureThreadLifecycle::Pending);
    assert_eq!(initial.completed_checkpoint_count, 0);
    assert_eq!(initial.current_checkpoint, None);
    assert_eq!(
        initial
            .next_checkpoint
            .as_ref()
            .map(|outcome| outcome.step_id.as_str()),
        Some("working")
    );

    let second_checkpoint = &timeline.projections[2];
    assert_eq!(second_checkpoint.at_monotonic_ms, 2_000);
    assert_eq!(
        second_checkpoint.lifecycle,
        DiagnosticFixtureThreadLifecycle::AtCheckpoint
    );
    assert_eq!(second_checkpoint.completed_checkpoint_count, 2);
    assert_eq!(
        second_checkpoint
            .current_checkpoint
            .as_ref()
            .map(|outcome| (outcome.step_id.as_str(), outcome.kind)),
        Some((
            "action-succeeded",
            DiagnosticScheduledOutcomeKind::FixtureCheckpoint
        ))
    );
    assert_eq!(
        second_checkpoint
            .next_checkpoint
            .as_ref()
            .map(|outcome| outcome.step_id.as_str()),
        Some("worked")
    );

    let completed = timeline.projections.last().expect("terminal projection");
    assert_eq!(completed.at_monotonic_ms, 3_000);
    assert_eq!(
        completed.lifecycle,
        DiagnosticFixtureThreadLifecycle::Completed
    );
    assert_eq!(completed.completed_checkpoint_count, 3);
    assert_eq!(
        completed
            .current_checkpoint
            .as_ref()
            .map(|outcome| outcome.step_id.as_str()),
        Some("worked")
    );
    assert_eq!(completed.next_checkpoint, None);
}

#[test]
fn fixture_thread_projection_refuses_a_plan_that_drifted_after_construction() {
    let registry = parse_diagnostic_scenario_registry(SCENARIO_REGISTRY)
        .expect("canonical scenario registry must validate");
    let mut plan = build_diagnostic_fixture_thread_plan(&registry, "browser-work-success")
        .expect("catalog-derived fixture plan");
    plan.scheduled_outcomes[1].step_id = "other-step".to_string();

    assert_eq!(
        project_diagnostic_fixture_thread_plan(&registry, &plan, 2_000),
        Err(DiagnosticFixtureThreadPlanError::InvalidPlan)
    );
}

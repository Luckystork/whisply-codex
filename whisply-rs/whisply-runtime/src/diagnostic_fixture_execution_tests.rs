use crate::DiagnosticFixtureExecutionError;
use crate::build_diagnostic_fixture_execution;
use crate::diagnostic_fixture_thread_tests::SCENARIO_REGISTRY;
use crate::parse_diagnostic_scenario_registry;

#[test]
fn fixture_execution_binds_every_catalog_projection_without_authority() {
    let registry = parse_diagnostic_scenario_registry(SCENARIO_REGISTRY)
        .expect("canonical scenario registry must validate");
    let execution = build_diagnostic_fixture_execution(&registry, "browser-work-success")
        .expect("catalog-attested fixture execution");

    assert_eq!(execution.plan.fixture_id, execution.timeline.fixture_id);
    assert_eq!(execution.plan.fixture_id, execution.events.fixture_id);
    assert_eq!(execution.plan.fixture_id, execution.selectors.fixture_id);
    assert_eq!(execution.plan.scenario_id, execution.timeline.scenario_id);
    assert_eq!(execution.plan.scenario_id, execution.events.scenario_id);
    assert_eq!(execution.plan.scenario_id, execution.selectors.scenario_id);
    assert!(execution.events.events.iter().all(|event| {
        event.lane.network_is_denied()
            && event.simulated
            && !event.billable
            && event.authority.commercial_authority == "none"
            && event.adapters.iter().all(|adapter| {
                !adapter.allows_network
                    && !adapter.allows_provider_request
                    && !adapter.allows_tool_execution
                    && !adapter.allows_usage_reservation
                    && !adapter.allows_receipt_issuance
                    && !adapter.allows_policy_override
                    && !adapter.allows_persistence_mutation
            })
    }));
    assert!(
        execution
            .selectors
            .tools
            .iter()
            .all(|tool| !tool.can_execute && !tool.can_resolve_real_authority)
    );
    assert!(execution.supports_named_capture_event(&execution.events.events[1].event_id));
    assert!(!execution.supports_named_capture_event("unregistered-fixture-event"));
    assert!(execution.validate(&registry).is_ok());
}

#[test]
fn fixture_execution_rejects_cross_projection_drift() {
    let registry = parse_diagnostic_scenario_registry(SCENARIO_REGISTRY)
        .expect("canonical scenario registry must validate");
    let mut execution = build_diagnostic_fixture_execution(&registry, "browser-work-success")
        .expect("catalog-attested fixture execution");
    execution.events.events[1]
        .checkpoint
        .as_mut()
        .expect("checkpoint")
        .step_id = "unexpected-step".to_string();

    assert_eq!(
        execution.validate(&registry),
        Err(DiagnosticFixtureExecutionError::InvalidExecution)
    );
}

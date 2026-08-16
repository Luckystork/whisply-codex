use crate::DIAGNOSTIC_SCENARIO_REGISTRY_PROTOCOL;
use crate::DIAGNOSTIC_SCENARIO_REGISTRY_SCHEMA_VERSION;
use crate::DIAGNOSTIC_SCENARIO_RUNNER_VERSION;
use crate::parse_diagnostic_scenario_registry;

const SCENARIO_REGISTRY: &str =
    include_str!("../../../../../docs/contextual-action-layer/diagnostic-scenarios.json");

#[test]
fn embedded_scenario_registry_preserves_the_fixed_zero_authority_contract() {
    let registry = parse_diagnostic_scenario_registry(SCENARIO_REGISTRY)
        .expect("canonical scenario registry must validate");

    assert_eq!(
        registry.schema_version,
        DIAGNOSTIC_SCENARIO_REGISTRY_SCHEMA_VERSION
    );
    assert_eq!(registry.protocol, DIAGNOSTIC_SCENARIO_REGISTRY_PROTOCOL);
    assert_eq!(registry.runner_version, DIAGNOSTIC_SCENARIO_RUNNER_VERSION);
    assert_eq!(registry.scenarios.len(), 25);
    assert!(registry.scenarios.iter().all(|scenario| {
        scenario.authority.commercial_authority == "none"
            && scenario.authority.network == "forbidden"
            && !scenario.fixture.can_mint_authority
            && !scenario.fixture.can_reserve_usage
            && !scenario.fixture.can_reach_provider
    }));
    assert!(registry.scenarios.iter().any(|scenario| {
        scenario.id == "exam-mode-protected-states"
            && scenario.fixture.subscription_presentation == "isolated-protected-mode-states"
    }));
    assert!(registry.scenarios.iter().any(|scenario| {
        scenario.id == "undetected-mode-protected-states"
            && scenario.fixture.subscription_presentation == "isolated-protected-mode-states"
    }));
}

#[test]
fn scenario_registry_rejects_authority_drift_before_any_fixture_can_be_enumerated() {
    let mut registry: serde_json::Value =
        serde_json::from_str(SCENARIO_REGISTRY).expect("canonical registry JSON");
    registry["scenarios"][0]["authority"]["network"] = serde_json::json!("allowed");

    assert!(parse_diagnostic_scenario_registry(&registry.to_string()).is_err());
}

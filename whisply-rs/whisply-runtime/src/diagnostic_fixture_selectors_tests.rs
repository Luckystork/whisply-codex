use crate::DIAGNOSTIC_FIXTURE_SELECTOR_PROTOCOL;
use crate::DIAGNOSTIC_FIXTURE_SELECTOR_SCHEMA_VERSION;
use crate::DiagnosticFixtureAccountSelector;
use crate::DiagnosticFixtureAdapterKind;
use crate::DiagnosticFixtureAdapterMode;
use crate::DiagnosticFixtureSelectorError;
use crate::build_diagnostic_fixture_selectors;
use crate::diagnostic_fixture_thread_tests::SCENARIO_REGISTRY;
use crate::parse_diagnostic_scenario_registry;

#[test]
fn fixture_selectors_are_catalog_bound_synthetic_and_non_executable() {
    let registry = parse_diagnostic_scenario_registry(SCENARIO_REGISTRY)
        .expect("canonical scenario registry must validate");
    let selectors = build_diagnostic_fixture_selectors(&registry, "account-subscription-states")
        .expect("catalog-derived selectors");

    assert_eq!(
        selectors.schema_version,
        DIAGNOSTIC_FIXTURE_SELECTOR_SCHEMA_VERSION
    );
    assert_eq!(selectors.protocol, DIAGNOSTIC_FIXTURE_SELECTOR_PROTOCOL);
    assert_eq!(
        selectors.fixture_id,
        "fixture-account-subscription-states-v1-seed4208"
    );
    assert_eq!(
        selectors.account,
        DiagnosticFixtureAccountSelector::SyntheticLocalAccount
    );
    assert_eq!(selectors.subscription_presentation, "pro-max");
    assert_eq!(selectors.tools.len(), 7);
    assert_eq!(
        selectors.tools[0].adapter,
        DiagnosticFixtureAdapterKind::Network
    );
    assert_eq!(
        selectors.tools[6].adapter,
        DiagnosticFixtureAdapterKind::Compaction
    );
    assert!(selectors.tools.iter().all(|tool| {
        tool.mode == DiagnosticFixtureAdapterMode::FixtureOnlyNoop
            && !tool.can_execute
            && !tool.can_resolve_real_authority
    }));

    let encoded = serde_json::to_string(&selectors).expect("selectors serialize");
    for forbidden_key in [
        "accountId",
        "credential",
        "endpoint",
        "executable",
        "providerRoute",
    ] {
        assert!(
            !encoded.contains(forbidden_key),
            "selectors must not serialize {forbidden_key}"
        );
    }
    selectors
        .validate(&registry)
        .expect("derived selectors must remain catalog-bound");
}

#[test]
fn fixture_selectors_reject_real_authority_or_catalog_drift() {
    let registry = parse_diagnostic_scenario_registry(SCENARIO_REGISTRY)
        .expect("canonical scenario registry must validate");
    let mut selectors = build_diagnostic_fixture_selectors(&registry, "browser-work-success")
        .expect("catalog-derived selectors");
    selectors.tools[0].can_resolve_real_authority = true;

    assert_eq!(
        selectors.validate(&registry),
        Err(DiagnosticFixtureSelectorError::InvalidSelectors)
    );

    let mut selectors = build_diagnostic_fixture_selectors(&registry, "browser-work-success")
        .expect("catalog-derived selectors");
    selectors.subscription_presentation = "pro-max".to_string();

    assert_eq!(
        selectors.validate(&registry),
        Err(DiagnosticFixtureSelectorError::InvalidSelectors)
    );
}

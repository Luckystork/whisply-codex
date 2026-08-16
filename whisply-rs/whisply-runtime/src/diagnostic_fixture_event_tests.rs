use crate::DIAGNOSTIC_FIXTURE_EVENT_PROTOCOL;
use crate::DIAGNOSTIC_FIXTURE_EVENT_SCHEMA_VERSION;
use crate::DiagnosticFixtureAdapterKind;
use crate::DiagnosticFixtureAdapterMode;
use crate::DiagnosticFixtureEventError;
use crate::DiagnosticFixtureEventKind;
use crate::build_diagnostic_fixture_event_stream;
use crate::diagnostic_fixture_thread_tests::SCENARIO_REGISTRY;
use crate::parse_diagnostic_scenario_registry;

#[test]
fn fixture_event_stream_is_catalog_bound_redacted_and_zero_authority() {
    let registry = parse_diagnostic_scenario_registry(SCENARIO_REGISTRY)
        .expect("canonical scenario registry must validate");
    let stream = build_diagnostic_fixture_event_stream(&registry, "browser-work-success")
        .expect("catalog-derived fixture event stream");

    assert_eq!(
        stream.schema_version,
        DIAGNOSTIC_FIXTURE_EVENT_SCHEMA_VERSION
    );
    assert_eq!(stream.protocol, DIAGNOSTIC_FIXTURE_EVENT_PROTOCOL);
    assert_eq!(
        stream.fixture_id,
        "fixture-browser-work-success-v1-seed4101"
    );
    assert_eq!(stream.events.len(), 4);

    let pending = &stream.events[0];
    assert_eq!(pending.kind, DiagnosticFixtureEventKind::Pending);
    assert_eq!(pending.sequence, 0);
    assert_eq!(pending.at_monotonic_ms, 0);
    assert_eq!(pending.checkpoint, None);

    let checkpoint = &stream.events[1];
    assert_eq!(checkpoint.kind, DiagnosticFixtureEventKind::Checkpoint);
    assert_eq!(checkpoint.sequence, 1);
    assert_eq!(checkpoint.at_monotonic_ms, 1_000);
    assert_eq!(
        checkpoint
            .checkpoint
            .as_ref()
            .map(|value| value.step_id.as_str()),
        Some("working")
    );

    let completed = stream.events.last().expect("completed fixture event");
    assert_eq!(completed.kind, DiagnosticFixtureEventKind::Completed);
    assert_eq!(completed.sequence, 3);
    assert_eq!(completed.at_monotonic_ms, 3_000);
    assert_eq!(
        completed
            .checkpoint
            .as_ref()
            .map(|value| value.step_id.as_str()),
        Some("worked")
    );

    for event in &stream.events {
        assert!(event.simulated);
        assert!(!event.billable);
        assert_eq!(event.authority.commercial_authority, "none");
        assert_eq!(event.authority.network, "forbidden");
        assert_eq!(event.adapters.len(), 7);
        assert_eq!(
            event.adapters[0].kind,
            DiagnosticFixtureAdapterKind::Network
        );
        assert_eq!(
            event.adapters[6].kind,
            DiagnosticFixtureAdapterKind::Compaction
        );
        assert!(event.adapters.iter().all(|adapter| {
            adapter.mode == DiagnosticFixtureAdapterMode::FixtureOnlyNoop
                && !adapter.allows_network
                && !adapter.allows_provider_request
                && !adapter.allows_tool_execution
                && !adapter.allows_usage_reservation
                && !adapter.allows_receipt_issuance
                && !adapter.allows_policy_override
                && !adapter.allows_persistence_mutation
        }));
    }

    let encoded = serde_json::to_string(&stream).expect("event stream serializes");
    for forbidden_key in [
        "endpoint",
        "accountId",
        "credential",
        "rawText",
        "screenshotPath",
        "artifactPath",
        "url",
    ] {
        assert!(
            !encoded.contains(forbidden_key),
            "redacted event stream must not serialize {forbidden_key}"
        );
    }
    stream
        .validate(&registry)
        .expect("derived stream must remain catalog-bound");
}

#[test]
fn fixture_event_stream_rejects_authority_or_checkpoint_drift() {
    let registry = parse_diagnostic_scenario_registry(SCENARIO_REGISTRY)
        .expect("canonical scenario registry must validate");
    let mut stream = build_diagnostic_fixture_event_stream(&registry, "browser-work-success")
        .expect("catalog-derived fixture event stream");
    stream.events[1].adapters[0].allows_network = true;

    assert_eq!(
        stream.validate(&registry),
        Err(DiagnosticFixtureEventError::InvalidStream)
    );

    let mut stream = build_diagnostic_fixture_event_stream(&registry, "browser-work-success")
        .expect("catalog-derived fixture event stream");
    stream.events[2]
        .checkpoint
        .as_mut()
        .expect("checkpoint event")
        .step_id = "other-step".to_string();

    assert_eq!(
        stream.validate(&registry),
        Err(DiagnosticFixtureEventError::InvalidStream)
    );
}

use serde_json::Value;

use super::scenario_registry::DiagnosticScenarioAction;
use super::scenario_registry::DiagnosticScenarioCommand;
use super::scenario_registry::DiagnosticScenarioEventKindArg;
use super::scenario_registry::DiagnosticScenarioEventsArgs;
use super::scenario_registry::DiagnosticScenarioLookupArgs;
use super::scenario_registry::DiagnosticScenarioReplayArgs;
use super::scenario_registry::DiagnosticScenarioReplayControlArg;
use super::scenario_registry::catalog_fixture_execution_from_redacted_replay_bundle;
use super::scenario_registry::catalog_redacted_replay_bundle;
use super::scenario_registry::run;
use codex_whisply::DiagnosticFixtureReplayControl;
use codex_whisply::DiagnosticFixtureReplaySpeed;
use codex_whisply::MAX_DIAGNOSTIC_FIXTURE_EVENTS;

fn catalog_replay_bundle() -> Value {
    let inspection = run(DiagnosticScenarioCommand {
        action: DiagnosticScenarioAction::Inspect(DiagnosticScenarioLookupArgs {
            scenario: "browser-work-success".to_string(),
        }),
    })
    .expect("embedded scenario inspection");
    serde_json::json!({
        "schemaVersion": 1,
        "protocol": "whisply.diagnostics.catalog-replay.v1",
        "registrySchemaVersion": 1,
        "registryProtocol": "whisply.diagnostics.v1",
        "registryRunnerVersion": "1.0.0",
        "registrySha256": inspection["registrySha256"].clone(),
        "schemaSha256": inspection["schemaSha256"].clone(),
        "scenarioId": "browser-work-success",
        "scenarioVersion": inspection["scenario"]["version"].clone(),
        "controls": [
            {"kind": "pause"},
            {"kind": "step"},
            {"kind": "speed", "speed": "double"}
        ]
    })
}

#[test]
fn scenario_catalog_lists_the_embedded_zero_authority_fixture_metadata() {
    let report = run(DiagnosticScenarioCommand {
        action: DiagnosticScenarioAction::List,
    })
    .expect("embedded catalog");

    assert_eq!(report["ok"], Value::Bool(true));
    assert_eq!(report["protocol"], "whisply.diagnostics.v1");
    assert_eq!(report["scenarios"].as_array().map(Vec::len), Some(25));
    assert!(report["scenarios"].as_array().is_some_and(|scenarios| {
        scenarios.iter().all(|scenario| {
            scenario["authority"]["commercialAuthority"] == "none"
                && scenario["authority"]["network"] == "forbidden"
        })
    }));
}

#[test]
fn scenario_catalog_inspection_is_metadata_only_and_hash_bound() {
    let report = run(DiagnosticScenarioCommand {
        action: DiagnosticScenarioAction::Inspect(DiagnosticScenarioLookupArgs {
            scenario: "browser-work-success".to_string(),
        }),
    })
    .expect("embedded scenario inspection");

    assert_eq!(report["scenario"]["id"], "browser-work-success");
    assert_eq!(report["scenario"]["authority"]["host"], "xctest-only");
    assert_eq!(report["scenario"]["fixture"]["canReachProvider"], false);
    assert_eq!(
        report["commands"],
        serde_json::json!([
            "list",
            "describe",
            "inspect",
            "plan",
            "timeline",
            "events",
            "selectors",
            "replay",
            "execute",
            "schema",
            "validate"
        ])
    );
    assert!(
        report["registrySha256"]
            .as_str()
            .is_some_and(|digest| digest.len() == 64)
    );
    assert!(
        report["schemaSha256"]
            .as_str()
            .is_some_and(|digest| digest.len() == 64)
    );
}

#[test]
fn scenario_catalog_replay_is_bounded_and_reduces_only_catalog_projections() {
    let report = run(DiagnosticScenarioCommand {
        action: DiagnosticScenarioAction::Replay(DiagnosticScenarioReplayArgs {
            scenario: "browser-work-success".to_string(),
            controls: vec![
                DiagnosticScenarioReplayControlArg(DiagnosticFixtureReplayControl::Pause),
                DiagnosticScenarioReplayControlArg(DiagnosticFixtureReplayControl::Step),
                DiagnosticScenarioReplayControlArg(DiagnosticFixtureReplayControl::Seek {
                    checkpoint_index: 2,
                }),
                DiagnosticScenarioReplayControlArg(DiagnosticFixtureReplayControl::Speed {
                    speed: DiagnosticFixtureReplaySpeed::Quadruple,
                }),
            ],
        }),
    })
    .expect("catalog-attested fixture replay");

    assert_eq!(report["ok"], Value::Bool(true));
    assert_eq!(
        report["replay"]["protocol"],
        "whisply.diagnostics.fixture-replay.v1"
    );
    assert_eq!(report["replay"]["checkpointIndex"], 2);
    assert_eq!(report["replay"]["paused"], true);
    assert_eq!(report["replay"]["speed"], "quadruple");
    assert_eq!(
        report["replay"]["projection"]["completedCheckpointCount"],
        2
    );
    assert!(report["replay"].get("path").is_none());
    assert!(report["replay"].get("account").is_none());
    assert!(report["replay"].get("endpoint").is_none());
}

#[test]
fn production_replay_bundle_is_redacted_schema_hash_and_version_bound() {
    let bundle = catalog_replay_bundle();
    let bytes = serde_json::to_vec(&bundle).expect("redacted replay bundle bytes");
    let catalog = catalog_fixture_execution_from_redacted_replay_bundle(&bytes)
        .expect("exact catalog-bound replay bundle");
    let replay = catalog
        .replay
        .as_ref()
        .expect("reducer-derived replay state");

    assert_eq!(catalog.execution.plan.scenario_id, "browser-work-success");
    assert_eq!(replay.checkpoint_index, 1);
    assert!(replay.paused);
    assert_eq!(replay.speed, DiagnosticFixtureReplaySpeed::Double);
    assert!(
        catalog
            .execution
            .selectors
            .tools
            .iter()
            .all(|tool| !tool.can_execute && !tool.can_resolve_real_authority)
    );

    let mut wrong_catalog = bundle.clone();
    wrong_catalog["registrySha256"] = Value::String("0".repeat(64));
    assert!(
        catalog_fixture_execution_from_redacted_replay_bundle(
            &serde_json::to_vec(&wrong_catalog).expect("wrong catalog bundle bytes"),
        )
        .is_err()
    );

    let mut wrong_version = bundle.clone();
    wrong_version["scenarioVersion"] = Value::from(999_u64);
    assert!(
        catalog_fixture_execution_from_redacted_replay_bundle(
            &serde_json::to_vec(&wrong_version).expect("wrong version bundle bytes"),
        )
        .is_err()
    );

    let mut caller_authored_state = bundle.clone();
    caller_authored_state["visibleCopy"] = Value::String("not accepted".to_string());
    assert!(
        catalog_fixture_execution_from_redacted_replay_bundle(
            &serde_json::to_vec(&caller_authored_state).expect("extra-state bundle bytes"),
        )
        .is_err()
    );

    let mut widened_control = bundle;
    widened_control["controls"] = serde_json::json!([
        {"kind": "pause", "endpoint": "not-a-fixture-control"}
    ]);
    assert!(
        catalog_fixture_execution_from_redacted_replay_bundle(
            &serde_json::to_vec(&widened_control).expect("widened-control bundle bytes"),
        )
        .is_err()
    );
}

#[test]
fn catalog_replay_bundle_export_is_deterministic_closed_and_consumer_valid() {
    let controls = vec![
        DiagnosticScenarioReplayControlArg(DiagnosticFixtureReplayControl::Pause),
        DiagnosticScenarioReplayControlArg(DiagnosticFixtureReplayControl::Step),
        DiagnosticScenarioReplayControlArg(DiagnosticFixtureReplayControl::Speed {
            speed: DiagnosticFixtureReplaySpeed::Double,
        }),
    ];
    let first = catalog_redacted_replay_bundle("browser-work-success", &controls)
        .expect("catalog replay bundle");
    let second = catalog_redacted_replay_bundle("browser-work-success", &controls)
        .expect("deterministic catalog replay bundle");
    assert_eq!(first, second);

    let bundle: Value = serde_json::from_slice(&first).expect("redacted replay bundle JSON");
    let keys = bundle
        .as_object()
        .expect("replay bundle object")
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        keys,
        vec![
            "schemaVersion",
            "protocol",
            "registrySchemaVersion",
            "registryProtocol",
            "registryRunnerVersion",
            "registrySha256",
            "schemaSha256",
            "scenarioId",
            "scenarioVersion",
            "controls",
        ]
    );
    assert!(bundle.get("visibleCopy").is_none());
    assert!(bundle.get("authority").is_none());
    assert!(bundle.get("account").is_none());
    assert!(bundle.get("endpoint").is_none());

    let catalog = catalog_fixture_execution_from_redacted_replay_bundle(&first)
        .expect("producer output accepted by installed consumer");
    let replay = catalog.replay.expect("reducer-derived replay");
    assert_eq!(catalog.execution.plan.scenario_id, "browser-work-success");
    assert!(replay.paused);
    assert_eq!(replay.checkpoint_index, 1);
    assert_eq!(replay.speed, DiagnosticFixtureReplaySpeed::Double);
    assert!(
        catalog
            .execution
            .selectors
            .tools
            .iter()
            .all(|tool| !tool.can_execute && !tool.can_resolve_real_authority)
    );
    assert!(catalog_redacted_replay_bundle("not-a-catalog-scenario", &controls).is_err());
}

#[test]
fn scenario_catalog_selectors_are_synthetic_and_cannot_resolve_authority() {
    let report = run(DiagnosticScenarioCommand {
        action: DiagnosticScenarioAction::Selectors(DiagnosticScenarioLookupArgs {
            scenario: "account-subscription-states".to_string(),
        }),
    })
    .expect("catalog-derived fixture selectors");

    assert_eq!(report["ok"], Value::Bool(true));
    assert_eq!(report["selectors"]["account"], "synthetic_local_account");
    assert_eq!(report["selectors"]["model"], "simulated_presentation");
    assert_eq!(report["selectors"]["profile"], "synthetic_presentation");
    assert_eq!(report["selectors"]["subscriptionPresentation"], "pro-max");
    assert_eq!(
        report["selectors"]["tools"].as_array().map(Vec::len),
        Some(7)
    );
    assert!(
        report["selectors"]["tools"]
            .as_array()
            .is_some_and(|tools| {
                tools.iter().all(|tool| {
                    tool["mode"] == "fixture_only_noop"
                        && tool["canExecute"] == false
                        && tool["canResolveRealAuthority"] == false
                })
            })
    );
    assert!(report["selectors"].get("accountId").is_none());
    assert!(report["selectors"].get("credential").is_none());
    assert!(report["selectors"].get("endpoint").is_none());
}

#[test]
fn scenario_catalog_plan_is_deterministic_and_non_executable() {
    let report = run(DiagnosticScenarioCommand {
        action: DiagnosticScenarioAction::Plan(DiagnosticScenarioLookupArgs {
            scenario: "browser-work-success".to_string(),
        }),
    })
    .expect("catalog-derived fixture plan");

    assert_eq!(report["ok"], Value::Bool(true));
    assert_eq!(report["plan"]["lane"], "presentation_fixture");
    assert_eq!(report["plan"]["simulated"], true);
    assert_eq!(report["plan"]["billable"], false);
    assert_eq!(report["plan"]["authority"]["commercialAuthority"], "none");
    assert_eq!(report["plan"]["authority"]["network"], "forbidden");
    assert_eq!(report["plan"]["clock"]["monotonicOriginMs"], 0);
    assert_eq!(
        report["plan"]["scheduledOutcomes"][0]["atMonotonicMs"],
        1_000
    );
    assert_eq!(
        report["plan"]["scheduledOutcomes"][0]["kind"],
        "fixture_checkpoint"
    );
    assert!(report["plan"].get("path").is_none());
    assert!(report["plan"].get("account").is_none());
    assert!(report["plan"].get("endpoint").is_none());
}

#[test]
fn scenario_catalog_timeline_is_virtual_and_zero_authority() {
    let report = run(DiagnosticScenarioCommand {
        action: DiagnosticScenarioAction::Timeline(DiagnosticScenarioLookupArgs {
            scenario: "browser-work-success".to_string(),
        }),
    })
    .expect("catalog-derived fixture timeline");

    assert_eq!(report["ok"], Value::Bool(true));
    assert_eq!(report["timeline"]["lane"], "presentation_fixture");
    assert_eq!(report["timeline"]["simulated"], true);
    assert_eq!(report["timeline"]["billable"], false);
    assert_eq!(
        report["timeline"]["authority"]["commercialAuthority"],
        "none"
    );
    assert_eq!(report["timeline"]["projections"][0]["lifecycle"], "pending");
    assert_eq!(
        report["timeline"]["projections"][3]["lifecycle"],
        "completed"
    );
    assert!(report["timeline"].get("path").is_none());
    assert!(report["timeline"].get("account").is_none());
    assert!(report["timeline"].get("endpoint").is_none());
}

#[test]
fn scenario_catalog_events_are_redacted_fixture_only_metadata() {
    let report = run(DiagnosticScenarioCommand {
        action: DiagnosticScenarioAction::Events(DiagnosticScenarioEventsArgs {
            scenario: "browser-work-success".to_string(),
            tail: None,
            show: None,
            kind: None,
        }),
    })
    .expect("catalog-derived fixture events");

    assert_eq!(report["ok"], Value::Bool(true));
    assert_eq!(report["events"]["events"].as_array().map(Vec::len), Some(4));
    assert_eq!(report["events"]["events"][0]["kind"], "pending");
    assert_eq!(report["events"]["events"][3]["kind"], "completed");
    assert_eq!(
        report["events"]["events"][1]["adapters"][0]["mode"],
        "fixture_only_noop"
    );
    assert_eq!(
        report["events"]["events"][1]["adapters"][0]["allowsNetwork"],
        false
    );
    assert_eq!(
        report["events"]["events"][1]["authority"]["commercialAuthority"],
        "none"
    );
    assert!(report["events"].get("path").is_none());
    assert!(report["events"].get("account").is_none());
    assert!(report["events"].get("endpoint").is_none());
    assert!(report["events"].get("credential").is_none());
}

#[test]
fn scenario_catalog_event_inspection_is_bounded_redacted_and_exact() {
    let complete = run(DiagnosticScenarioCommand {
        action: DiagnosticScenarioAction::Events(DiagnosticScenarioEventsArgs {
            scenario: "browser-work-success".to_string(),
            tail: None,
            show: None,
            kind: None,
        }),
    })
    .expect("complete catalog event stream");
    let event_id = complete["events"]["events"][1]["eventId"]
        .as_str()
        .expect("catalog event id")
        .to_string();

    let tail = run(DiagnosticScenarioCommand {
        action: DiagnosticScenarioAction::Events(DiagnosticScenarioEventsArgs {
            scenario: "browser-work-success".to_string(),
            tail: Some(2),
            show: None,
            kind: None,
        }),
    })
    .expect("bounded catalog event tail");
    assert_eq!(tail["ok"], Value::Bool(true));
    assert_eq!(tail["selection"], "tail");
    assert_eq!(tail["requestedTailCount"], 2);
    assert_eq!(tail["stream"]["totalEventCount"], 4);
    assert_eq!(tail["events"].as_array().map(Vec::len), Some(2));
    assert_eq!(tail["events"][0]["kind"], "checkpoint");
    assert_eq!(tail["events"][1]["kind"], "completed");
    assert!(tail.get("path").is_none());
    assert!(tail.get("account").is_none());
    assert!(tail.get("endpoint").is_none());
    assert!(tail.get("credential").is_none());
    assert!(tail["events"].as_array().is_some_and(|events| {
        events.iter().all(|event| {
            event.get("path").is_none()
                && event.get("account").is_none()
                && event.get("endpoint").is_none()
                && event.get("credential").is_none()
        })
    }));

    let show = run(DiagnosticScenarioCommand {
        action: DiagnosticScenarioAction::Events(DiagnosticScenarioEventsArgs {
            scenario: "browser-work-success".to_string(),
            tail: None,
            show: Some(event_id.clone()),
            kind: None,
        }),
    })
    .expect("exact catalog event");
    assert_eq!(show["selection"], "show");
    assert_eq!(show["requestedEventId"], event_id);
    assert_eq!(show["events"].as_array().map(Vec::len), Some(1));
    assert_eq!(show["events"][0]["eventId"], event_id);
    assert_eq!(show["events"][0]["kind"], "checkpoint");

    let checkpoint = run(DiagnosticScenarioCommand {
        action: DiagnosticScenarioAction::Events(DiagnosticScenarioEventsArgs {
            scenario: "browser-work-success".to_string(),
            tail: None,
            show: None,
            kind: Some(DiagnosticScenarioEventKindArg::Checkpoint),
        }),
    })
    .expect("catalog event kind filter");
    assert_eq!(checkpoint["selection"], "filter");
    assert_eq!(checkpoint["requestedKind"], "checkpoint");
    assert_eq!(checkpoint["events"].as_array().map(Vec::len), Some(2));
    assert!(
        checkpoint["events"]
            .as_array()
            .is_some_and(|events| events.iter().all(|event| event["kind"] == "checkpoint"))
    );

    let zero_tail = run(DiagnosticScenarioCommand {
        action: DiagnosticScenarioAction::Events(DiagnosticScenarioEventsArgs {
            scenario: "browser-work-success".to_string(),
            tail: Some(0),
            show: None,
            kind: None,
        }),
    })
    .expect_err("zero event tail must be rejected");
    assert!(zero_tail.to_string().contains("between 1"));

    let oversized_tail = run(DiagnosticScenarioCommand {
        action: DiagnosticScenarioAction::Events(DiagnosticScenarioEventsArgs {
            scenario: "browser-work-success".to_string(),
            tail: Some(MAX_DIAGNOSTIC_FIXTURE_EVENTS + 1),
            show: None,
            kind: None,
        }),
    })
    .expect_err("oversized event tail must be rejected");
    assert!(oversized_tail.to_string().contains("tail count"));

    let invalid_event_id = run(DiagnosticScenarioCommand {
        action: DiagnosticScenarioAction::Events(DiagnosticScenarioEventsArgs {
            scenario: "browser-work-success".to_string(),
            tail: None,
            show: Some("not-a-catalog-event/".to_string()),
            kind: None,
        }),
    })
    .expect_err("unsafe event id must be rejected");
    assert!(
        invalid_event_id
            .to_string()
            .contains("bounded lowercase fixture identifier")
    );

    let missing_event_id = run(DiagnosticScenarioCommand {
        action: DiagnosticScenarioAction::Events(DiagnosticScenarioEventsArgs {
            scenario: "browser-work-success".to_string(),
            tail: None,
            show: Some(format!("{event_id}-missing")),
            kind: None,
        }),
    })
    .expect_err("unregistered event id must be rejected");
    assert!(
        missing_event_id
            .to_string()
            .contains("Unknown catalog event for this scenario.")
    );

    let mixed_selectors = run(DiagnosticScenarioCommand {
        action: DiagnosticScenarioAction::Events(DiagnosticScenarioEventsArgs {
            scenario: "browser-work-success".to_string(),
            tail: Some(1),
            show: None,
            kind: Some(DiagnosticScenarioEventKindArg::Pending),
        }),
    })
    .expect_err("mixed event selectors must be rejected");
    assert!(
        mixed_selectors
            .to_string()
            .contains("Choose only one event selector")
    );
}

#[test]
fn scenario_catalog_rejects_unregistered_ids_without_execution_fallback() {
    let error = run(DiagnosticScenarioCommand {
        action: DiagnosticScenarioAction::Describe(DiagnosticScenarioLookupArgs {
            scenario: "unregistered-scenario".to_string(),
        }),
    })
    .expect_err("unknown scenarios must not receive a fallback route");

    assert!(
        error
            .to_string()
            .contains("Unknown registered diagnostic scenario.")
    );
}

#[test]
fn catalog_fixture_execution_is_exact_and_has_no_authority_input() {
    let execution = super::scenario_registry::catalog_fixture_execution("browser-work-success")
        .expect("catalog fixture execution");

    assert_eq!(execution.execution.plan.scenario_id, "browser-work-success");
    assert_eq!(
        execution.execution.plan.fixture_id,
        execution.execution.events.fixture_id
    );
    assert_eq!(execution.presentation.locale, "en_US");
    assert!(!execution.presentation.reduce_motion);
    assert!(!execution.presentation.increased_contrast);
    assert!(
        execution
            .execution
            .selectors
            .tools
            .iter()
            .all(|tool| !tool.can_execute && !tool.can_resolve_real_authority)
    );
}

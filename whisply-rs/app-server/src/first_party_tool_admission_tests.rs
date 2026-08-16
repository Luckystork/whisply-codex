use super::*;
use codex_app_server_protocol::WhisplyToolModelCall;
use pretty_assertions::assert_eq;
use serde_json::json;

fn registration(
    admission_id: &str,
    thread_id: &str,
    client_user_message_id: &str,
) -> WhisplyToolAdmissionRegisterParams {
    WhisplyToolAdmissionRegisterParams {
        admission_id: admission_id.to_string(),
        thread_id: thread_id.to_string(),
        client_user_message_id: client_user_message_id.to_string(),
        tool_ids: vec!["whisply.screen.context".to_string()],
    }
}

fn finish(
    admission_id: &str,
    thread_id: &str,
    turn_id: &str,
    client_user_message_id: &str,
) -> WhisplyToolAdmissionFinishParams {
    WhisplyToolAdmissionFinishParams {
        admission_id: admission_id.to_string(),
        thread_id: thread_id.to_string(),
        turn_id: turn_id.to_string(),
        client_user_message_id: client_user_message_id.to_string(),
    }
}

#[tokio::test]
async fn registration_activates_only_once_for_its_exact_turn_identity() {
    let store = FirstPartyToolAdmissionStore::default();
    let owner_connection = ConnectionId(1);
    let response = store
        .register(
            owner_connection,
            registration("admission-1", "thread-1", "message-1"),
        )
        .await
        .expect("registration should succeed");

    assert_eq!(response.accepted, true);
    assert!(
        store
            .take_for_turn(owner_connection, "thread-1", Some("message-1"))
            .await
            .is_some()
    );
    assert!(
        store
            .take_for_turn(owner_connection, "thread-1", Some("message-1"))
            .await
            .is_none()
    );
}

#[tokio::test]
async fn registration_cannot_cross_connection_thread_or_user_message_boundaries() {
    let store = FirstPartyToolAdmissionStore::default();
    let owner_connection = ConnectionId(1);
    store
        .register(
            owner_connection,
            registration("admission-1", "thread-1", "message-1"),
        )
        .await
        .expect("registration should succeed");

    assert!(
        store
            .take_for_turn(ConnectionId(2), "thread-1", Some("message-1"))
            .await
            .is_none()
    );
    assert!(
        store
            .take_for_turn(owner_connection, "thread-2", Some("message-1"))
            .await
            .is_none()
    );
    assert!(
        store
            .take_for_turn(owner_connection, "thread-1", Some("message-2"))
            .await
            .is_none()
    );
    assert!(
        store
            .take_for_turn(owner_connection, "thread-1", Some("message-1"))
            .await
            .is_some()
    );
}

#[tokio::test]
async fn connection_close_revokes_its_pending_native_admissions() {
    let store = FirstPartyToolAdmissionStore::default();
    let owner_connection = ConnectionId(1);
    store
        .register(
            owner_connection,
            registration("admission-1", "thread-1", "message-1"),
        )
        .await
        .expect("registration should succeed");

    store.connection_closed(owner_connection).await;

    assert!(
        store
            .take_for_turn(owner_connection, "thread-1", Some("message-1"))
            .await
            .is_none()
    );
}

#[test]
fn expiry_purges_a_native_admission_before_it_can_be_reused() {
    let now = std::time::Instant::now();
    let admission = FirstPartyToolAdmission::new(
        "admission-1".to_string(),
        "message-1".to_string(),
        vec!["whisply.screen.context".to_string()],
    )
    .expect("static screen admission should be valid");
    let mut state = AdmissionState {
        by_id: std::collections::HashMap::from([(
            "admission-1".to_string(),
            AdmissionRecord {
                connection_id: ConnectionId(1),
                thread_id: "thread-1".to_string(),
                admission,
                expires_at: now - std::time::Duration::from_millis(1),
                phase: AdmissionPhase::Pending,
                terminal_finish_requested: false,
            },
        )]),
    };

    state.purge_expired(now);

    assert!(state.by_id.is_empty());
}

#[tokio::test]
async fn finish_turn_requires_the_exact_active_admission_binding() {
    let store = FirstPartyToolAdmissionStore::default();
    let owner_connection = ConnectionId(1);
    store
        .register(
            owner_connection,
            registration("admission-1", "thread-1", "message-1"),
        )
        .await
        .expect("registration should succeed");
    let admission = store
        .take_for_turn(owner_connection, "thread-1", Some("message-1"))
        .await
        .expect("matching turn should activate admission");
    let execution = FirstPartyToolExecution::new(
        admission,
        WhisplyToolModelCall {
            execution_id: "native-call-1".to_string(),
            tool_id: "whisply.screen.context".to_string(),
            schema_version: 1,
            arguments: json!({"scope": "exact_window"}),
        },
        "thread-1".to_string(),
        "turn-1".to_string(),
    )
    .expect("execution should match admission");
    assert_eq!(
        store.begin_execution(&execution).await,
        Some(owner_connection)
    );
    store.finish_execution(&execution).await;

    assert!(
        !store
            .finish_turn(
                ConnectionId(2),
                finish("admission-1", "thread-1", "turn-1", "message-1"),
            )
            .await
            .released
    );
    assert!(
        !store
            .finish_turn(
                owner_connection,
                finish("admission-1", "thread-1", "turn-2", "message-1"),
            )
            .await
            .released
    );
    assert!(
        !store
            .finish_turn(
                owner_connection,
                finish("admission-1", "thread-1", "turn-1", "message-2"),
            )
            .await
            .released
    );

    assert!(
        store
            .finish_turn(
                owner_connection,
                finish("admission-1", "thread-1", "turn-1", "message-1"),
            )
            .await
            .released
    );
    assert!(
        !store
            .finish_turn(
                owner_connection,
                finish("admission-1", "thread-1", "turn-1", "message-1"),
            )
            .await
            .released,
        "terminal cleanup must be idempotent"
    );

    let response = store
        .register(
            owner_connection,
            registration("admission-1", "thread-1", "message-1"),
        )
        .await
        .expect("the finished grant must free its connection capacity");
    assert!(response.accepted);
}

#[tokio::test]
async fn terminal_finish_is_accepted_while_an_owner_call_is_still_unwinding() {
    let store = FirstPartyToolAdmissionStore::default();
    let owner_connection = ConnectionId(1);
    store
        .register(
            owner_connection,
            registration("admission-1", "thread-1", "message-1"),
        )
        .await
        .expect("registration should succeed");
    let admission = store
        .take_for_turn(owner_connection, "thread-1", Some("message-1"))
        .await
        .expect("matching turn should activate admission");
    let execution = FirstPartyToolExecution::new(
        admission,
        WhisplyToolModelCall {
            execution_id: "native-call-1".to_string(),
            tool_id: "whisply.screen.context".to_string(),
            schema_version: 1,
            arguments: json!({"scope": "exact_window"}),
        },
        "thread-1".to_string(),
        "turn-1".to_string(),
    )
    .expect("execution should match admission");
    assert_eq!(
        store.begin_execution(&execution).await,
        Some(owner_connection)
    );

    assert!(
        store
            .finish_turn(
                owner_connection,
                finish("admission-1", "thread-1", "turn-1", "message-1"),
            )
            .await
            .released,
        "the terminal client should not need to retry after Core's dispatch future unwinds"
    );
    assert!(matches!(
        store
            .register(
                owner_connection,
                registration("admission-1", "thread-1", "message-1"),
            )
            .await,
        Err(FirstPartyToolAdmissionStoreError::DuplicateAdmission),
    ));

    store.finish_execution(&execution).await;
    let response = store
        .register(
            owner_connection,
            registration("admission-1", "thread-1", "message-1"),
        )
        .await
        .expect("the reserved admission ID must be released after its owner call unwinds");
    assert!(response.accepted);
}

fn progress(
    admission_id: &str,
    thread_id: &str,
    turn_id: &str,
    execution_id: &str,
) -> WhisplyToolProgressReportParams {
    WhisplyToolProgressReportParams {
        admission_id: admission_id.to_string(),
        thread_id: thread_id.to_string(),
        turn_id: turn_id.to_string(),
        execution_id: execution_id.to_string(),
        label: "Opening the first result".to_string(),
        fraction: Some(0.25),
        icon_id: Some("browser.spawned".to_string()),
    }
}

/// Activates an admission and begins one owner call on it.
async fn running_call(store: &FirstPartyToolAdmissionStore) -> FirstPartyToolExecution {
    store
        .register(
            ConnectionId(1),
            registration("admission-1", "thread-1", "message-1"),
        )
        .await
        .expect("registration should succeed");
    let admission = store
        .take_for_turn(ConnectionId(1), "thread-1", Some("message-1"))
        .await
        .expect("matching turn should activate admission");
    let execution = FirstPartyToolExecution::new(
        admission,
        WhisplyToolModelCall {
            execution_id: "native-call-1".to_string(),
            tool_id: "whisply.screen.context".to_string(),
            schema_version: 1,
            arguments: json!({"scope": "exact_window"}),
        },
        "thread-1".to_string(),
        "turn-1".to_string(),
    )
    .expect("execution should match admission");
    assert_eq!(
        store.begin_execution(&execution).await,
        Some(ConnectionId(1))
    );
    execution
}

#[tokio::test]
async fn progress_is_accepted_only_for_a_call_that_is_running_now() {
    let store = FirstPartyToolAdmissionStore::default();
    let execution = running_call(&store).await;

    assert!(
        store
            .report_progress(
                ConnectionId(1),
                &progress("admission-1", "thread-1", "turn-1", "native-call-1",)
            )
            .await
    );

    store.finish_execution(&execution).await;
    assert!(
        !store
            .report_progress(
                ConnectionId(1),
                &progress("admission-1", "thread-1", "turn-1", "native-call-1",)
            )
            .await,
        "a call that already returned must not keep publishing activity"
    );
}

#[tokio::test]
async fn progress_cannot_borrow_another_connection_thread_turn_or_call() {
    let store = FirstPartyToolAdmissionStore::default();
    let _execution = running_call(&store).await;

    for (description, connection, params) in [
        (
            "another connection",
            ConnectionId(2),
            progress("admission-1", "thread-1", "turn-1", "native-call-1"),
        ),
        (
            "another thread",
            ConnectionId(1),
            progress("admission-1", "thread-2", "turn-1", "native-call-1"),
        ),
        (
            "another turn",
            ConnectionId(1),
            progress("admission-1", "thread-1", "turn-2", "native-call-1"),
        ),
        (
            "another call",
            ConnectionId(1),
            progress("admission-1", "thread-1", "turn-1", "native-call-2"),
        ),
        (
            "another admission",
            ConnectionId(1),
            progress("admission-2", "thread-1", "turn-1", "native-call-1"),
        ),
    ] {
        assert!(
            !store.report_progress(connection, &params).await,
            "progress reported from {description} must be refused"
        );
    }
}

#[tokio::test]
async fn progress_keeps_a_long_owner_call_alive_up_to_its_absolute_lifetime() {
    let store = FirstPartyToolAdmissionStore::default();
    let execution = running_call(&store).await;
    let first_deadline = store
        .execution_deadline(&execution)
        .await
        .expect("a running call has a deadline");

    // Rewind the call's start and deadline the way elapsed time would, then
    // report progress: an owner that is still working keeps its wait.
    {
        let mut state = store.state.lock().await;
        let record = state.by_id.get_mut("admission-1").expect("record");
        let AdmissionPhase::Active { in_flight, .. } = &mut record.phase else {
            panic!("call should be active");
        };
        let liveness = in_flight.get_mut("native-call-1").expect("liveness");
        liveness.started_at -= EXECUTION_TIMEOUT;
        liveness.deadline -= EXECUTION_TIMEOUT;
    }
    assert!(
        store
            .report_progress(
                ConnectionId(1),
                &progress("admission-1", "thread-1", "turn-1", "native-call-1",)
            )
            .await
    );
    let renewed = store
        .execution_deadline(&execution)
        .await
        .expect("a running call has a deadline");
    assert!(
        renewed >= first_deadline - EXECUTION_TIMEOUT,
        "progress must move the deadline forward from where it had fallen"
    );

    // An owner that keeps talking past the absolute lifetime still ends.
    {
        let mut state = store.state.lock().await;
        let record = state.by_id.get_mut("admission-1").expect("record");
        let AdmissionPhase::Active { in_flight, .. } = &mut record.phase else {
            panic!("call should be active");
        };
        let liveness = in_flight.get_mut("native-call-1").expect("liveness");
        liveness.started_at -= MAX_EXECUTION_LIFETIME;
    }
    assert!(
        store
            .report_progress(
                ConnectionId(1),
                &progress("admission-1", "thread-1", "turn-1", "native-call-1",)
            )
            .await
    );
    assert!(
        store
            .execution_deadline(&execution)
            .await
            .expect("deadline")
            <= Instant::now(),
        "the absolute lifetime must cap a talkative but stuck owner"
    );
}

#[tokio::test]
async fn a_progress_label_or_fraction_that_cannot_be_shown_is_refused() {
    let store = FirstPartyToolAdmissionStore::default();
    let _execution = running_call(&store).await;

    let mut blank_label = progress("admission-1", "thread-1", "turn-1", "native-call-1");
    blank_label.label = "   ".to_string();
    assert!(!store.report_progress(ConnectionId(1), &blank_label).await);

    let mut unbounded_label = progress("admission-1", "thread-1", "turn-1", "native-call-1");
    unbounded_label.label = "n".repeat(MAX_PROGRESS_LABEL_BYTES + 1);
    assert!(
        !store
            .report_progress(ConnectionId(1), &unbounded_label)
            .await
    );

    for fraction in [-0.1, 1.1, f64::NAN] {
        let mut out_of_range = progress("admission-1", "thread-1", "turn-1", "native-call-1");
        out_of_range.fraction = Some(fraction);
        assert!(
            !store.report_progress(ConnectionId(1), &out_of_range).await,
            "a fraction of {fraction} cannot be drawn as a proportion"
        );
    }
}

#[tokio::test]
async fn an_icon_a_surface_could_not_draw_is_refused_like_any_other_identifier() {
    let store = FirstPartyToolAdmissionStore::default();
    let _execution = running_call(&store).await;

    let named = progress("admission-1", "thread-1", "turn-1", "native-call-1");
    assert!(
        store.report_progress(ConnectionId(1), &named).await,
        "a report that names the capability at work is the ordinary case"
    );

    let mut unnamed = progress("admission-1", "thread-1", "turn-1", "native-call-1");
    unnamed.icon_id = None;
    assert!(
        store.report_progress(ConnectionId(1), &unnamed).await,
        "a step is still worth showing when no icon travels with it"
    );

    let mut unbounded_icon = progress("admission-1", "thread-1", "turn-1", "native-call-1");
    unbounded_icon.icon_id = Some("n".repeat(MAX_CORRELATION_ID_BYTES + 1));
    assert!(
        !store
            .report_progress(ConnectionId(1), &unbounded_icon)
            .await,
        "an unbounded icon reaches a surface that would try to draw it"
    );
}

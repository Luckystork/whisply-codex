//! Public JSON-RPC coverage for one-turn native Whisply tool admissions.
//!
//! The lower-level store tests exercise binding and revocation directly. These
//! checks deliberately use the normal client request route, so they prove an
//! admitted capability reaches only its matching user turn and that a model
//! call returns to the registering connection without any envelope crossing
//! the model boundary.

#![cfg(target_os = "macos")]

use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use app_test_support::ManagedWhisplyConfig;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::ServerRequest;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::TurnCompletedNotification;
use codex_app_server_protocol::TurnInterruptParams;
use codex_app_server_protocol::TurnInterruptResponse;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::TurnStatus;
use codex_app_server_protocol::UserInput as V2UserInput;
use codex_app_server_protocol::WhisplyToolAdmissionFinishParams;
use codex_app_server_protocol::WhisplyToolAdmissionFinishResponse;
use codex_app_server_protocol::WhisplyToolAdmissionRegisterParams;
use codex_app_server_protocol::WhisplyToolAdmissionRegisterResponse;
use codex_app_server_protocol::WhisplyToolAdmittedCancelAcknowledgement;
use codex_app_server_protocol::WhisplyToolResult;
use codex_app_server_protocol::WhisplyToolTerminalStatus;
use core_test_support::responses;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(10);
const ADMISSION_ID: &str = "native-admission-1";
const CLIENT_MESSAGE_ID: &str = "native-message-1";
const CALL_ID: &str = "native-screen-call-1";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admitted_native_tool_round_trips_only_for_its_registered_user_turn() -> Result<()> {
    let responses_server = responses::start_mock_server().await;
    let response_mock = responses::mount_sse_sequence(
        &responses_server,
        vec![
            responses::sse(vec![
                responses::ev_response_created("native-tool-call"),
                responses::ev_function_call_with_namespace(
                    CALL_ID,
                    "whisply",
                    "screen_context",
                    r#"{"scope":"exact_window"}"#,
                ),
                responses::ev_completed("native-tool-call"),
            ]),
            responses::sse(vec![
                responses::ev_response_created("native-tool-complete"),
                responses::ev_assistant_message("native-tool-message", "Screen context received."),
                responses::ev_completed("native-tool-complete"),
            ]),
        ],
    )
    .await;
    let codex_home = TempDir::new()?;
    ManagedWhisplyConfig::new().write(codex_home.path())?;

    let mut app_server =
        app_test_support::managed_whisply_app_server_builder!(&responses_server.uri())
            .with_codex_home(codex_home.path())
            .build_initialized()
            .await?;
    let ThreadStartResponse { thread, .. } = app_server
        .start_thread(ThreadStartParams::default())
        .await?;

    let registration: WhisplyToolAdmissionRegisterResponse = app_server
        .request(|request_id| ClientRequest::WhisplyToolAdmissionRegister {
            request_id,
            params: WhisplyToolAdmissionRegisterParams {
                admission_id: ADMISSION_ID.to_string(),
                thread_id: thread.id.clone(),
                client_user_message_id: CLIENT_MESSAGE_ID.to_string(),
                tool_ids: vec![
                    "whisply.screen.context".to_string(),
                    "whisply.files".to_string(),
                ],
            },
        })
        .await?;
    assert!(registration.accepted);

    let TurnStartResponse { turn } = app_server
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                thread_id: thread.id.clone(),
                client_user_message_id: Some(CLIENT_MESSAGE_ID.to_string()),
                input: vec![V2UserInput::Text {
                    text: "Inspect the selected window.".to_string(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            },
        })
        .await?;

    let request = timeout(
        DEFAULT_READ_TIMEOUT,
        app_server.read_stream_until_request_message(),
    )
    .await??;
    let ServerRequest::WhisplyToolAdmittedExecute { request_id, params } = request else {
        panic!("expected admitted native tool request, got {request:?}");
    };
    assert_eq!(params.admission_id, ADMISSION_ID);
    assert_eq!(params.thread_id, thread.id);
    assert_eq!(params.turn_id, turn.id);
    assert_eq!(params.client_user_message_id, CLIENT_MESSAGE_ID);
    assert_eq!(params.call.execution_id, CALL_ID);
    assert_eq!(params.call.tool_id, "whisply.screen.context");
    assert_eq!(params.call.arguments, json!({ "scope": "exact_window" }));

    app_server
        .send_response(
            request_id,
            serde_json::to_value(WhisplyToolResult {
                execution_id: CALL_ID.to_string(),
                status: WhisplyToolTerminalStatus::Succeeded,
                content: Some(json!({ "summary": "Selected window inspected." })),
                safe_summary: "Selected window inspected.".to_string(),
                setup_route: None,
                receipt_id: None,
            })?,
        )
        .await?;
    timeout(
        DEFAULT_READ_TIMEOUT,
        app_server.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let release_params = WhisplyToolAdmissionFinishParams {
        admission_id: ADMISSION_ID.to_string(),
        thread_id: thread.id.clone(),
        turn_id: turn.id.clone(),
        client_user_message_id: CLIENT_MESSAGE_ID.to_string(),
    };
    let released: WhisplyToolAdmissionFinishResponse = app_server
        .request(|request_id| ClientRequest::WhisplyToolAdmissionFinish {
            request_id,
            params: release_params.clone(),
        })
        .await?;
    assert!(released.released);
    let already_released: WhisplyToolAdmissionFinishResponse = app_server
        .request(|request_id| ClientRequest::WhisplyToolAdmissionFinish {
            request_id,
            params: release_params,
        })
        .await?;
    assert!(!already_released.released);

    let requests = response_mock.requests();
    assert_eq!(requests.len(), 2);
    let tool = requests[0]
        .tool_by_name("whisply", "screen_context")
        .context("the exact matching turn should expose its admitted screen tool")?;
    assert_eq!(tool["type"], "function");
    assert_eq!(tool["strict"], false);
    let files_tool = requests[0]
        .tool_by_name("whisply", "files")
        .context("the same admitted namespace should include Files")?;
    assert_eq!(files_tool["type"], "function");
    let function_output = requests[1].function_call_output(CALL_ID);
    assert_eq!(function_output["type"], "function_call_output");
    assert_eq!(function_output["call_id"], CALL_ID);
    assert!(
        function_output["output"]
            .to_string()
            .contains("Selected window inspected."),
        "only the approved result should reach the follow-up model request: {function_output:#?}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admitted_native_tool_is_absent_for_a_different_client_message() -> Result<()> {
    let responses_server = responses::start_mock_server().await;
    let response_mock = responses::mount_sse_sequence(
        &responses_server,
        vec![responses::sse(vec![
            responses::ev_response_created("unadmitted-turn"),
            responses::ev_assistant_message("unadmitted-message", "No native capability."),
            responses::ev_completed("unadmitted-turn"),
        ])],
    )
    .await;
    let codex_home = TempDir::new()?;
    ManagedWhisplyConfig::new().write(codex_home.path())?;

    let mut app_server =
        app_test_support::managed_whisply_app_server_builder!(&responses_server.uri())
            .with_codex_home(codex_home.path())
            .build_initialized()
            .await?;
    let ThreadStartResponse { thread, .. } = app_server
        .start_thread(ThreadStartParams::default())
        .await?;

    let registration: WhisplyToolAdmissionRegisterResponse = app_server
        .request(|request_id| ClientRequest::WhisplyToolAdmissionRegister {
            request_id,
            params: WhisplyToolAdmissionRegisterParams {
                admission_id: ADMISSION_ID.to_string(),
                thread_id: thread.id.clone(),
                client_user_message_id: CLIENT_MESSAGE_ID.to_string(),
                tool_ids: vec!["whisply.screen.context".to_string()],
            },
        })
        .await?;
    assert!(registration.accepted);

    let _: TurnStartResponse = app_server
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                thread_id: thread.id,
                client_user_message_id: Some("different-client-message".to_string()),
                input: vec![V2UserInput::Text {
                    text: "Do not use the unbound capability.".to_string(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            },
        })
        .await?;
    timeout(
        DEFAULT_READ_TIMEOUT,
        app_server.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let request = response_mock.single_request();
    assert!(
        request.tool_by_name("whisply", "screen_context").is_none(),
        "a pending admission must not cross a client-message boundary"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupting_an_admitted_native_tool_cancels_the_exact_execution_and_releases_the_turn()
-> Result<()> {
    let responses_server = responses::start_mock_server().await;
    let _response_mock = responses::mount_sse_sequence(
        &responses_server,
        vec![responses::sse(vec![
            responses::ev_response_created("native-tool-cancel"),
            responses::ev_function_call_with_namespace(
                CALL_ID,
                "whisply",
                "screen_context",
                r#"{"scope":"exact_window"}"#,
            ),
            responses::ev_completed("native-tool-cancel"),
        ])],
    )
    .await;
    let codex_home = TempDir::new()?;
    ManagedWhisplyConfig::new().write(codex_home.path())?;

    let mut app_server =
        app_test_support::managed_whisply_app_server_builder!(&responses_server.uri())
            .with_codex_home(codex_home.path())
            .build_initialized()
            .await?;
    let ThreadStartResponse { thread, .. } = app_server
        .start_thread(ThreadStartParams::default())
        .await?;

    let registration: WhisplyToolAdmissionRegisterResponse = app_server
        .request(|request_id| ClientRequest::WhisplyToolAdmissionRegister {
            request_id,
            params: WhisplyToolAdmissionRegisterParams {
                admission_id: ADMISSION_ID.to_string(),
                thread_id: thread.id.clone(),
                client_user_message_id: CLIENT_MESSAGE_ID.to_string(),
                tool_ids: vec!["whisply.screen.context".to_string()],
            },
        })
        .await?;
    assert!(registration.accepted);

    let TurnStartResponse { turn } = app_server
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                thread_id: thread.id.clone(),
                client_user_message_id: Some(CLIENT_MESSAGE_ID.to_string()),
                input: vec![V2UserInput::Text {
                    text: "Start a native screen inspection, then stop it.".to_string(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            },
        })
        .await?;

    let execute_request = timeout(
        DEFAULT_READ_TIMEOUT,
        app_server.read_stream_until_request_message(),
    )
    .await??;
    let ServerRequest::WhisplyToolAdmittedExecute { params, .. } = execute_request else {
        panic!("expected admitted native tool execution before interrupt");
    };
    assert_eq!(params.call.execution_id, CALL_ID);

    let interrupt_request_id = app_server
        .send_turn_interrupt_request(TurnInterruptParams {
            thread_id: thread.id.clone(),
            turn_id: turn.id.clone(),
        })
        .await?;
    let cancel_request = timeout(
        DEFAULT_READ_TIMEOUT,
        app_server.read_stream_until_request_message(),
    )
    .await??;
    let ServerRequest::WhisplyToolAdmittedCancel { request_id, params } = cancel_request else {
        panic!("interrupting an admitted native call must issue an owner cancellation");
    };
    assert_eq!(params.admission_id, ADMISSION_ID);
    assert_eq!(params.execution_id, CALL_ID);
    assert_eq!(params.thread_id, thread.id);
    assert_eq!(params.turn_id, turn.id);
    assert_eq!(params.client_user_message_id, CLIENT_MESSAGE_ID);
    app_server
        .send_response(
            request_id,
            serde_json::to_value(WhisplyToolAdmittedCancelAcknowledgement {
                execution_id: CALL_ID.to_string(),
                accepted: true,
            })?,
        )
        .await?;

    let _: TurnInterruptResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        app_server.read_response(interrupt_request_id),
    )
    .await??;
    let completed: TurnCompletedNotification = timeout(
        DEFAULT_READ_TIMEOUT,
        app_server.read_notification("turn/completed"),
    )
    .await??;
    assert_eq!(completed.thread_id, thread.id);
    assert_eq!(completed.turn.id, turn.id);
    assert_eq!(completed.turn.status, TurnStatus::Interrupted);

    let released: WhisplyToolAdmissionFinishResponse = app_server
        .request(|request_id| ClientRequest::WhisplyToolAdmissionFinish {
            request_id,
            params: WhisplyToolAdmissionFinishParams {
                admission_id: ADMISSION_ID.to_string(),
                thread_id: thread.id,
                turn_id: turn.id,
                client_user_message_id: CLIENT_MESSAGE_ID.to_string(),
            },
        })
        .await?;
    assert!(released.released);

    Ok(())
}

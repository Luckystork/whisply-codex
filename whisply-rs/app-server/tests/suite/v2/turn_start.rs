#![cfg(target_os = "macos")]

use anyhow::Context;
use anyhow::Result;
#[cfg(target_os = "macos")]
use app_test_support::ManagedWhisplyConfig;
#[cfg(target_os = "macos")]
use app_test_support::ManagedWhisplyGatewayFixture;
use app_test_support::TestAppServer;
use app_test_support::create_apply_patch_sse_response;
use app_test_support::create_exec_command_sse_response;
use app_test_support::create_final_assistant_message_sse_response;
use app_test_support::create_mock_responses_server_repeating_assistant;
use app_test_support::create_mock_responses_server_sequence;
use app_test_support::create_mock_responses_server_sequence_unchecked;
use app_test_support::create_request_user_input_sse_response;
use app_test_support::create_shell_command_sse_response;
use app_test_support::format_with_current_shell_display;
use app_test_support::write_models_cache;
use codex_app_server_protocol::AdditionalContextEntry;
use codex_app_server_protocol::AdditionalContextKind;
use codex_app_server_protocol::ByteRange;
use codex_app_server_protocol::ClientInfo;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::CommandExecutionApprovalDecision;
use codex_app_server_protocol::CommandExecutionRequestApprovalResponse;
use codex_app_server_protocol::CommandExecutionStatus;
use codex_app_server_protocol::FileChangeApprovalDecision;
use codex_app_server_protocol::FileChangePatchUpdatedNotification;
use codex_app_server_protocol::FileChangeRequestApprovalResponse;
use codex_app_server_protocol::ItemCompletedNotification;
use codex_app_server_protocol::ItemStartedNotification;
use codex_app_server_protocol::JSONRPCError;
use codex_app_server_protocol::JSONRPCMessage;
use codex_app_server_protocol::PatchApplyStatus;
use codex_app_server_protocol::PatchChangeKind;
use codex_app_server_protocol::RawResponseCompletedNotification;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ServerRequest;
use codex_app_server_protocol::ServerRequestResolvedNotification;
use codex_app_server_protocol::SubAgentActivityKind;
use codex_app_server_protocol::TextElement;
use codex_app_server_protocol::ThreadDeleteParams;
use codex_app_server_protocol::ThreadDeleteResponse;
use codex_app_server_protocol::ThreadDeletedNotification;
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::ThreadLoadedListParams;
use codex_app_server_protocol::ThreadLoadedListResponse;
use codex_app_server_protocol::ThreadSettingsUpdatedNotification;
use codex_app_server_protocol::ThreadSource;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::TokenUsageBreakdown;
use codex_app_server_protocol::TurnCompletedNotification;
use codex_app_server_protocol::TurnEnvironmentParams;
use codex_app_server_protocol::TurnItemsView;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::TurnStartedNotification;
use codex_app_server_protocol::TurnStatus;
use codex_app_server_protocol::TurnSteerParams;
use codex_app_server_protocol::UserInput as V2UserInput;
use codex_app_server_protocol::WarningNotification;
use core_test_support::responses;
use core_test_support::skip_if_no_network;
use core_test_support::skip_if_remote;
use core_test_support::skip_if_wine_exec;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::collections::HashMap;
use std::path::Path;
use tempfile::TempDir;
use tokio::time::timeout;
use whisply_app_server::INPUT_TOO_LARGE_ERROR_CODE;
use whisply_app_server::INVALID_PARAMS_ERROR_CODE;
use whisply_exec_server::LOCAL_ENVIRONMENT_ID;
use whisply_features::Feature;
use whisply_protocol::config_types::CollaborationMode;
use whisply_protocol::config_types::ModeKind;
use whisply_protocol::config_types::MultiAgentMode;
use whisply_protocol::config_types::Personality;
use whisply_protocol::config_types::ReasoningSummary;
use whisply_protocol::config_types::Settings;
use whisply_protocol::models::BUILT_IN_PERMISSION_PROFILE_DANGER_FULL_ACCESS;
use whisply_protocol::models::ImageDetail;
use whisply_protocol::openai_models::ReasoningEffort;
use whisply_protocol::protocol::MULTI_AGENT_MODE_OPEN_TAG;
use whisply_protocol::user_input::MAX_USER_INPUT_TEXT_CHARS;
use whisply_utils_absolute_path::test_support::PathExt;

#[cfg(windows)]
const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(25);
#[cfg(not(windows))]
const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const MULTI_AGENT_V2_NAMESPACE: &str = "collaboration";
const INVALID_REQUEST_ERROR_CODE: i64 = -32600;
const TINY_PNG_BYTES: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0,
    0, 0, 31, 21, 196, 137, 0, 0, 0, 11, 73, 68, 65, 84, 120, 156, 99, 96, 0, 2, 0, 0, 5, 0, 1,
    122, 94, 171, 63, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];
const TINY_PNG_DATA_URL: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==";

fn body_contains(req: &wiremock::Request, text: &str) -> bool {
    String::from_utf8(req.body.clone())
        .ok()
        .is_some_and(|body| body.contains(text))
}

async fn wait_for_v2_spawned_subagent(
    mcp: &mut TestAppServer,
    call_id: &str,
) -> Result<(String, String)> {
    timeout(DEFAULT_READ_TIMEOUT, async {
        loop {
            let completed: ItemCompletedNotification =
                mcp.read_notification("item/completed").await?;
            if let ThreadItem::SubAgentActivity {
                id,
                kind: SubAgentActivityKind::Started,
                agent_thread_id,
                agent_path,
            } = completed.item
                && id == call_id
            {
                return Ok::<(String, String), anyhow::Error>((agent_thread_id, agent_path));
            }
        }
    })
    .await?
}

async fn response_request_payload_for_child(
    server: &wiremock::MockServer,
    child_prompt: &str,
    spawn_call_id: &str,
) -> Result<Value> {
    timeout(DEFAULT_READ_TIMEOUT, async {
        loop {
            let requests = server
                .received_requests()
                .await
                .context("managed response fixture should retain recorded requests")?;
            if let Some(request) = requests.iter().find(|request| {
                request.url.path().ends_with("/responses")
                    && body_contains(request, child_prompt)
                    && !body_contains(request, spawn_call_id)
            }) {
                return serde_json::from_slice(&request.body)
                    .context("child response request should be valid JSON");
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await?
}

async fn run_local_image_turn(detail: Option<ImageDetail>) -> Result<Vec<Value>> {
    // Two Codex turns hit the mock model (session start + turn/start).
    let responses = vec![
        create_final_assistant_message_sse_response("Done")?,
        create_final_assistant_message_sse_response("Done")?,
    ];
    // Use the unchecked variant because the strict matcher does not currently
    // cover image-bearing request payloads.
    let server = create_mock_responses_server_sequence_unchecked(responses).await;

    let codex_home = TempDir::new()?;
    ManagedWhisplyConfig::new().write(codex_home.path())?;

    let mut mcp = app_test_support::managed_whisply_app_server_builder!(&server.uri())
        .with_codex_home(codex_home.path())
        .build_initialized()
        .await?;

    let ThreadStartResponse { thread, .. } = mcp
        .start_thread(ThreadStartParams {
            model: Some("mock-model".to_string()),
            ..Default::default()
        })
        .await?;

    let image_path = codex_home.path().join("image.png");
    std::fs::write(&image_path, TINY_PNG_BYTES)?;

    let TurnStartResponse { turn } = mcp
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                thread_id: thread.id.clone(),
                client_user_message_id: None,
                input: vec![V2UserInput::LocalImage {
                    path: image_path,
                    detail,
                }],
                ..Default::default()
            },
        })
        .await?;
    assert!(!turn.id.is_empty());

    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    received_response_input_images(&server).await
}

async fn received_response_input_images(server: &wiremock::MockServer) -> Result<Vec<Value>> {
    let requests = server
        .received_requests()
        .await
        .context("failed to fetch received requests")?;
    let mut input_images = Vec::new();

    for request in requests {
        if !request.url.path().ends_with("/responses") {
            continue;
        }
        let body = request
            .body_json::<Value>()
            .context("request body should be JSON")?;
        let Some(input) = body.get("input").and_then(Value::as_array) else {
            continue;
        };

        for item in input {
            if item.get("type").and_then(Value::as_str) != Some("message") {
                continue;
            }
            let Some(content) = item.get("content").and_then(Value::as_array) else {
                continue;
            };
            input_images.extend(
                content
                    .iter()
                    .filter(|span| span.get("type").and_then(Value::as_str) == Some("input_image"))
                    .cloned(),
            );
        }
    }

    Ok(input_images)
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn turn_start_with_empty_input_runs_model_request() -> Result<()> {
    let responses = vec![create_final_assistant_message_sse_response("Done")?];
    let server = create_mock_responses_server_sequence_unchecked(responses).await;

    let codex_home = TempDir::new()?;
    ManagedWhisplyConfig::new().write(codex_home.path())?;
    let managed_gateway = ManagedWhisplyGatewayFixture::new(&server.uri())?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .with_managed_whisply_gateway(managed_gateway)
        .build_initialized()
        .await?;

    let ThreadStartResponse { thread, .. } = mcp
        .start_thread(ThreadStartParams {
            model: Some("mock-model".to_string()),
            thread_source: Some(ThreadSource::User),
            ..Default::default()
        })
        .await?;

    let TurnStartResponse { turn } = mcp
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                thread_id: thread.id.clone(),
                client_user_message_id: None,
                input: Vec::new(),
                ..Default::default()
            },
        })
        .await?;
    assert!(!turn.id.is_empty());

    let started: TurnStartedNotification =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_notification("turn/started")).await??;
    assert_eq!(started.thread_id, thread.id);
    assert_eq!(started.turn.id, turn.id);
    assert_eq!(started.turn.status, TurnStatus::InProgress);

    let completed: TurnCompletedNotification = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_notification("turn/completed"),
    )
    .await??;
    assert_eq!(completed.thread_id, thread.id);
    assert_eq!(completed.turn.id, turn.id);
    assert_eq!(completed.turn.status, TurnStatus::Completed);
    assert_eq!(completed.turn.items_view, TurnItemsView::Summary);
    assert!(matches!(
        &completed.turn.items[..],
        [ThreadItem::AgentMessage { text, .. }] if text == "Done"
    ));

    let requests = server
        .received_requests()
        .await
        .context("failed to fetch received requests")?;
    let response_requests = requests
        .iter()
        .filter(|request| request.url.path().ends_with("/responses"))
        .collect::<Vec<_>>();
    assert_eq!(response_requests.len(), 1);
    let response_request = response_requests[0];
    assert_eq!(
        response_request
            .headers
            .get("authorization")
            .context("managed response request should include a broker bearer")?
            .to_str()
            .context("managed response authorization should be valid ASCII")?,
        "Bearer test-managed-broker-bearer",
        "the response request must use the refreshed broker descriptor rather than the initial launch descriptor"
    );
    assert_eq!(
        response_request
            .headers
            .get("x-whisply-runtime")
            .context("managed response request should include the direct runtime marker")?
            .to_str()
            .context("direct runtime marker should be valid ASCII")?,
        "direct"
    );
    assert_eq!(
        response_request
            .headers
            .get("x-whisply-protocol-version")
            .context("managed response request should include the direct protocol marker")?
            .to_str()
            .context("direct protocol marker should be valid ASCII")?,
        "1"
    );
    let body = response_request
        .body_json::<Value>()
        .context("request body should be JSON")?;
    let input = body
        .get("input")
        .and_then(Value::as_array)
        .context("request body should include input array")?;
    assert!(
        !input.iter().any(|item| {
            item.get("type").and_then(Value::as_str) == Some("message")
                && item.get("role").and_then(Value::as_str) == Some("user")
                && item
                    .get("content")
                    .and_then(Value::as_array)
                    .is_some_and(Vec::is_empty)
        }),
        "empty turn/start should not synthesize an empty user message: {input:?}"
    );
    mcp.assert_managed_whisply_gateway_healthy()?;
    let operations = mcp
        .managed_whisply_broker_operations()
        .context("managed Whisply broker fixture should be attached")?;
    assert!(
        operations
            .iter()
            .any(|operation| operation == "broker.hello"),
        "managed Whisply provider should initialize through broker.hello: {operations:?}"
    );
    assert!(
        operations
            .iter()
            .any(|operation| operation == "runtime.descriptors"),
        "model response I/O should refresh through runtime.descriptors: {operations:?}"
    );

    Ok(())
}

#[tokio::test]
async fn turn_start_additional_context_flows_to_model_input() -> Result<()> {
    let responses = vec![create_final_assistant_message_sse_response("Done")?];
    let server = create_mock_responses_server_sequence_unchecked(responses).await;

    let codex_home = TempDir::new()?;
    ManagedWhisplyConfig::new().write(codex_home.path())?;

    let mut mcp = app_test_support::managed_whisply_app_server_builder!(&server.uri())
        .with_codex_home(codex_home.path())
        .build_initialized()
        .await?;

    let ThreadStartResponse { thread, .. } = mcp
        .start_thread(ThreadStartParams {
            model: Some("mock-model".to_string()),
            ..Default::default()
        })
        .await?;

    let _: TurnStartResponse = mcp
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                thread_id: thread.id,
                client_user_message_id: None,
                input: vec![V2UserInput::Text {
                    text: "inspect tab".to_string(),
                    text_elements: Vec::new(),
                }],
                additional_context: Some(HashMap::from([(
                    "custom_source".to_string(),
                    AdditionalContextEntry {
                        value: "source value".to_string(),
                        kind: AdditionalContextKind::Untrusted,
                    },
                )])),
                ..Default::default()
            },
        })
        .await?;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let requests = server
        .received_requests()
        .await
        .context("failed to fetch received requests")?;
    let request = requests
        .iter()
        .find(|request| request.url.path().ends_with("/responses"))
        .context("expected model request")?;
    let body = request
        .body_json::<Value>()
        .context("request body should be JSON")?;
    assert!(
        body.to_string()
            .contains("<external_custom_source>source value</external_custom_source>")
    );

    Ok(())
}

/// Whisply hands a continued conversation to the runtime through this field,
/// and the Mac app is an ordinary non-experimental client. Gating it would
/// force the app to opt into every experimental method and notification just to
/// say what the conversation is being continued from, so the field is stable in
/// this fork.
#[tokio::test]
async fn turn_start_additional_context_does_not_require_experimental_api() -> Result<()> {
    let responses = vec![create_final_assistant_message_sse_response("Done")?];
    let server = create_mock_responses_server_sequence_unchecked(responses).await;

    let codex_home = TempDir::new()?;
    ManagedWhisplyConfig::new().write(codex_home.path())?;

    let mut mcp = app_test_support::managed_whisply_app_server_builder!(&server.uri())
        .with_codex_home(codex_home.path())
        .build()
        .await?;
    let initialized = mcp
        .initialize_with_capabilities(
            ClientInfo {
                name: app_test_support::DEFAULT_CLIENT_NAME.to_string(),
                title: None,
                version: "0.1.0".to_string(),
            },
            Some(codex_app_server_protocol::InitializeCapabilities {
                experimental_api: false,
                ..Default::default()
            }),
        )
        .await?;
    let JSONRPCMessage::Response(_) = initialized else {
        anyhow::bail!("expected initialize response, got {initialized:?}");
    };

    // Deliberately not `start_thread`: that helper pins a local environment,
    // which is itself experimental. The Mac app does not send one.
    let ThreadStartResponse { thread, .. } = mcp
        .request(|request_id| ClientRequest::ThreadStart {
            request_id,
            params: ThreadStartParams {
                model: Some("mock-model".to_string()),
                ..Default::default()
            },
        })
        .await?;

    let _: TurnStartResponse = mcp
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                thread_id: thread.id,
                client_user_message_id: None,
                input: vec![V2UserInput::Text {
                    text: "and the deposit?".to_string(),
                    text_elements: Vec::new(),
                }],
                additional_context: Some(HashMap::from([(
                    "whisply_earlier_conversation".to_string(),
                    AdditionalContextEntry {
                        value: "Person: compare the lease offers".to_string(),
                        kind: AdditionalContextKind::Application,
                    },
                )])),
                ..Default::default()
            },
        })
        .await?;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let requests = server
        .received_requests()
        .await
        .context("failed to fetch received requests")?;
    let request = requests
        .iter()
        .find(|request| request.url.path().ends_with("/responses"))
        .context("expected model request")?;
    let body = request
        .body_json::<Value>()
        .context("request body should be JSON")?;
    assert!(
        body.to_string().contains(
            "<whisply_earlier_conversation>Person: compare the lease offers\
             </whisply_earlier_conversation>"
        ),
        "the earlier conversation must reach the model: {body}"
    );

    Ok(())
}

#[tokio::test]
async fn turn_start_omits_client_originator_header_for_managed_gateway() -> Result<()> {
    let responses = vec![create_final_assistant_message_sse_response("Done")?];
    let server = create_mock_responses_server_sequence_unchecked(responses).await;

    let codex_home = TempDir::new()?;
    ManagedWhisplyConfig::new()
        .enable_feature(Feature::Personality)
        .write(codex_home.path())?;

    let mut mcp = app_test_support::managed_whisply_app_server_builder!(&server.uri())
        .with_codex_home(codex_home.path())
        .build()
        .await?;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.initialize_with_client_info(ClientInfo {
            name: "codex_vscode".to_string(),
            title: Some("Codex VS Code Extension".to_string()),
            version: "0.1.0".to_string(),
        }),
    )
    .await??;

    let ThreadStartResponse { thread, .. } = mcp
        .start_thread(ThreadStartParams {
            model: Some("mock-model".to_string()),
            thread_source: Some(ThreadSource::User),
            ..Default::default()
        })
        .await?;

    let _: TurnStartResponse = mcp
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                thread_id: thread.id.clone(),
                client_user_message_id: None,
                input: vec![V2UserInput::Text {
                    text: "Hello".to_string(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            },
        })
        .await?;

    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let requests = server
        .received_requests()
        .await
        .expect("failed to fetch received requests");
    assert!(!requests.is_empty());
    for request in requests {
        assert!(
            request.headers.get("originator").is_none(),
            "managed gateway requests must not forward the client originator"
        );
    }

    Ok(())
}

#[tokio::test]
async fn turn_start_emits_user_message_item_with_text_elements() -> Result<()> {
    let responses = vec![create_final_assistant_message_sse_response("Done")?];
    let server = create_mock_responses_server_sequence_unchecked(responses).await;

    let codex_home = TempDir::new()?;
    ManagedWhisplyConfig::new()
        .enable_feature(Feature::Personality)
        .write(codex_home.path())?;

    let mut mcp = app_test_support::managed_whisply_app_server_builder!(&server.uri())
        .with_codex_home(codex_home.path())
        .build_initialized()
        .await?;

    let ThreadStartResponse { thread, .. } = mcp
        .start_thread(ThreadStartParams {
            model: Some("mock-model".to_string()),
            thread_source: Some(ThreadSource::User),
            ..Default::default()
        })
        .await?;

    let text_elements = vec![TextElement::new(
        ByteRange { start: 0, end: 5 },
        Some("<note>".to_string()),
    )];
    let _: TurnStartResponse = mcp
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                thread_id: thread.id.clone(),
                client_user_message_id: Some("client-message-1".to_string()),
                input: vec![V2UserInput::Text {
                    text: "Hello".to_string(),
                    text_elements: text_elements.clone(),
                }],
                ..Default::default()
            },
        })
        .await?;

    let user_message_item = timeout(DEFAULT_READ_TIMEOUT, async {
        loop {
            let item_started: ItemStartedNotification =
                mcp.read_notification("item/started").await?;
            if let ThreadItem::UserMessage { .. } = item_started.item {
                return Ok::<ThreadItem, anyhow::Error>(item_started.item);
            }
        }
    })
    .await??;

    match user_message_item {
        ThreadItem::UserMessage {
            client_id, content, ..
        } => {
            assert_eq!(client_id, Some("client-message-1".to_string()));
            assert_eq!(
                content,
                vec![V2UserInput::Text {
                    text: "Hello".to_string(),
                    text_elements,
                }]
            );
        }
        other => panic!("expected user message item, got {other:?}"),
    }

    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    Ok(())
}

#[tokio::test]
async fn turn_start_emits_thread_scoped_warning_notification_for_trimmed_skills() -> Result<()> {
    let responses = vec![create_final_assistant_message_sse_response("Done")?];
    let server = create_mock_responses_server_sequence_unchecked(responses).await;

    let codex_home = TempDir::new()?;
    ManagedWhisplyConfig::new()
        .with_model("gpt-5.6-terra")
        .enable_feature(Feature::Personality)
        .with_additional_config("model_context_window = 100")
        .write(codex_home.path())?;
    write_test_skill(codex_home.path(), "alpha-skill")?;
    write_test_skill(codex_home.path(), "beta-skill")?;

    let isolated_home = codex_home.path().to_string_lossy();
    let mut mcp = app_test_support::managed_whisply_app_server_builder!(&server.uri())
        .with_codex_home(codex_home.path())
        .with_env_overrides(&[
            ("HOME", Some(isolated_home.as_ref())),
            ("USERPROFILE", Some(isolated_home.as_ref())),
        ])
        .build_initialized()
        .await?;

    let ThreadStartResponse { thread, .. } = mcp.start_thread(ThreadStartParams::default()).await?;

    let _: TurnStartResponse = mcp
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                thread_id: thread.id.clone(),
                client_user_message_id: None,
                input: vec![V2UserInput::Text {
                    text: "Hello".to_string(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            },
        })
        .await?;

    let warning: WarningNotification =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_notification("warning")).await??;
    assert_eq!(warning.thread_id.as_deref(), Some(thread.id.as_str()));
    assert_eq!(
        warning.message,
        "Exceeded skills context budget. All skill descriptions were removed and 7 additional skills were not included in the model-visible skills list."
    );

    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let requests = server
        .received_requests()
        .await
        .expect("failed to fetch received requests");
    let request = requests
        .last()
        .expect("expected at least one model request");
    assert!(
        body_contains(request, "## Skills"),
        "expected outgoing request to include the skills section"
    );
    assert!(
        !body_contains(request, "- alpha-skill:") && !body_contains(request, "- beta-skill:"),
        "expected trimmed skills to be omitted from the outgoing request body"
    );

    Ok(())
}

#[tokio::test]
async fn turn_start_omits_unadvertised_service_tier_from_managed_model_request() -> Result<()> {
    let server = responses::start_mock_server().await;
    let body = responses::sse(vec![
        responses::ev_response_created("resp-1"),
        responses::ev_assistant_message("msg-1", "Done"),
        responses::ev_completed("resp-1"),
    ]);
    let response_mock = responses::mount_sse_once(&server, body).await;

    let codex_home = TempDir::new()?;
    const MODEL_ID: &str = "gpt-5.6-terra";
    const UNADVERTISED_SERVICE_TIER: &str = "priority";
    ManagedWhisplyConfig::new().write(codex_home.path())?;

    let mut mcp = app_test_support::managed_whisply_app_server_builder!(&server.uri())
        .with_codex_home(codex_home.path())
        .build_initialized()
        .await?;

    let ThreadStartResponse { thread, .. } = mcp
        .start_thread(ThreadStartParams {
            model: Some(MODEL_ID.to_string()),
            ..Default::default()
        })
        .await?;

    let _: TurnStartResponse = mcp
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                thread_id: thread.id,
                service_tier: Some(Some(UNADVERTISED_SERVICE_TIER.to_string())),
                input: vec![V2UserInput::Text {
                    text: "Hello".to_string(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            },
        })
        .await?;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let request = response_mock.single_request().body_json();
    assert_eq!(request["model"], json!(MODEL_ID));
    assert!(
        request.get("service_tier").is_none(),
        "unadvertised service tier must not reach the managed gateway: {request:#?}"
    );

    Ok(())
}

#[tokio::test]
async fn turn_start_emits_raw_response_completed_with_upstream_usage() -> Result<()> {
    let server = responses::start_mock_server().await;
    let body = responses::sse(vec![
        responses::ev_response_created("resp-1"),
        responses::ev_assistant_message("msg-1", "Done"),
        json!({
            "type": "response.completed",
            "response": {
                "id": "resp-1",
                "usage": {
                    "input_tokens": 30,
                    "input_tokens_details": { "cached_tokens": 11 },
                    "output_tokens": 7,
                    "output_tokens_details": { "reasoning_tokens": 3 },
                    "total_tokens": 37
                }
            }
        }),
    ]);
    responses::mount_sse_once(&server, body).await;

    let codex_home = TempDir::new()?;
    ManagedWhisplyConfig::new().write(codex_home.path())?;
    write_models_cache(codex_home.path())?;

    let mut mcp = app_test_support::managed_whisply_app_server_builder!(&server.uri())
        .with_codex_home(codex_home.path())
        .build_initialized()
        .await?;

    let ThreadStartResponse { thread, .. } = mcp
        .start_thread(ThreadStartParams {
            experimental_raw_events: true,
            ..Default::default()
        })
        .await?;

    let TurnStartResponse { turn } = mcp
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                thread_id: thread.id.clone(),
                input: vec![V2UserInput::Text {
                    text: "Hello".to_string(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            },
        })
        .await?;

    let notification = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("rawResponse/completed"),
    )
    .await??;
    let notification: codex_app_server_protocol::ServerNotification = notification.try_into()?;
    let codex_app_server_protocol::ServerNotification::RawResponseCompleted(notification) =
        notification
    else {
        anyhow::bail!("expected rawResponse/completed notification");
    };

    assert_eq!(
        notification,
        RawResponseCompletedNotification {
            thread_id: thread.id,
            turn_id: turn.id,
            response_id: "resp-1".to_string(),
            usage: Some(TokenUsageBreakdown {
                total_tokens: 37,
                input_tokens: 30,
                cached_input_tokens: 11,
                cache_write_input_tokens: 0,
                output_tokens: 7,
                reasoning_output_tokens: 3,
            }),
        }
    );

    Ok(())
}

#[tokio::test]
async fn thread_start_omits_empty_instruction_overrides_from_model_request() -> Result<()> {
    let server = responses::start_mock_server().await;
    let body = responses::sse(vec![
        responses::ev_response_created("resp-1"),
        responses::ev_assistant_message("msg-1", "Done"),
        responses::ev_completed("resp-1"),
    ]);
    let response_mock = responses::mount_sse_once(&server, body).await;

    let codex_home = TempDir::new()?;
    ManagedWhisplyConfig::new().write(codex_home.path())?;

    let mut mcp = app_test_support::managed_whisply_app_server_builder!(&server.uri())
        .with_codex_home(codex_home.path())
        .build_initialized()
        .await?;

    let ThreadStartResponse { thread, .. } = mcp
        .start_thread(ThreadStartParams {
            // TODO(aibrahim): Replace empty string instruction overrides with explicit tri-state
            // app-server semantics: omitted, explicitly none, or explicit value.
            config: Some(HashMap::from([(
                "include_permissions_instructions".to_string(),
                json!(false),
            )])),
            base_instructions: Some(String::new()),
            developer_instructions: Some(String::new()),
            ..Default::default()
        })
        .await?;

    let _: TurnStartResponse = mcp
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                thread_id: thread.id,
                client_user_message_id: None,
                input: vec![V2UserInput::Text {
                    text: "Hello".to_string(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            },
        })
        .await?;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let request_body = response_mock.single_request().body_json();
    let empty_developer_input_texts = request_body["input"]
        .as_array()
        .expect("input array")
        .iter()
        .filter(|item| item.get("role").and_then(serde_json::Value::as_str) == Some("developer"))
        .filter_map(|item| item.get("content").and_then(serde_json::Value::as_array))
        .flatten()
        .filter(|content| {
            content.get("type").and_then(serde_json::Value::as_str) == Some("input_text")
        })
        .filter_map(|content| content.get("text").and_then(serde_json::Value::as_str))
        .filter(|text| text.is_empty())
        .collect::<Vec<_>>();
    assert_eq!(
        json!({
            "hasInstructions": request_body.get("instructions").is_some(),
            "emptyDeveloperInputTexts": empty_developer_input_texts,
        }),
        json!({
            "hasInstructions": false,
            "emptyDeveloperInputTexts": [],
        })
    );

    Ok(())
}

#[tokio::test]
async fn turn_start_accepts_text_at_limit_with_mention_item() -> Result<()> {
    let responses = vec![create_final_assistant_message_sse_response("Done")?];
    let server = create_mock_responses_server_sequence_unchecked(responses).await;

    let codex_home = TempDir::new()?;
    ManagedWhisplyConfig::new()
        .enable_feature(Feature::Personality)
        .write(codex_home.path())?;

    let mut mcp = app_test_support::managed_whisply_app_server_builder!(&server.uri())
        .with_codex_home(codex_home.path())
        .build_initialized()
        .await?;

    let ThreadStartResponse { thread, .. } = mcp
        .start_thread(ThreadStartParams {
            model: Some("mock-model".to_string()),
            ..Default::default()
        })
        .await?;

    let TurnStartResponse { turn } = mcp
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                thread_id: thread.id,
                client_user_message_id: None,
                input: vec![
                    V2UserInput::Text {
                        text: "x".repeat(MAX_USER_INPUT_TEXT_CHARS),
                        text_elements: Vec::new(),
                    },
                    V2UserInput::Mention {
                        name: "Demo App".to_string(),
                        path: "app://demo-app".to_string(),
                    },
                ],
                ..Default::default()
            },
        })
        .await?;
    assert_eq!(turn.status, TurnStatus::InProgress);

    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    Ok(())
}

#[tokio::test]
async fn turn_start_rejects_combined_oversized_text_input() -> Result<()> {
    let codex_home = TempDir::new()?;
    ManagedWhisplyConfig::new()
        .enable_feature(Feature::Personality)
        .write(codex_home.path())?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build_initialized()
        .await?;

    let ThreadStartResponse { thread, .. } = mcp
        .start_thread(ThreadStartParams {
            model: Some("mock-model".to_string()),
            ..Default::default()
        })
        .await?;

    let first = "x".repeat(MAX_USER_INPUT_TEXT_CHARS / 2);
    let second = "y".repeat(MAX_USER_INPUT_TEXT_CHARS / 2 + 1);
    let actual_chars = first.chars().count() + second.chars().count();

    let turn_req = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id,
            client_user_message_id: None,
            input: vec![
                V2UserInput::Text {
                    text: first,
                    text_elements: Vec::new(),
                },
                V2UserInput::Text {
                    text: second,
                    text_elements: Vec::new(),
                },
            ],
            ..Default::default()
        })
        .await?;
    let err: JSONRPCError = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(turn_req)),
    )
    .await??;

    assert_eq!(err.error.code, INVALID_PARAMS_ERROR_CODE);
    assert_eq!(
        err.error.message,
        format!("Input exceeds the maximum length of {MAX_USER_INPUT_TEXT_CHARS} characters.")
    );
    let data = err.error.data.expect("expected structured error data");
    assert_eq!(data["input_error_code"], INPUT_TOO_LARGE_ERROR_CODE);
    assert_eq!(data["max_chars"], MAX_USER_INPUT_TEXT_CHARS);
    assert_eq!(data["actual_chars"], actual_chars);

    let turn_started = tokio::time::timeout(
        std::time::Duration::from_millis(250),
        mcp.read_stream_until_notification_message("turn/started"),
    )
    .await;
    assert!(
        turn_started.is_err(),
        "did not expect a turn/started notification for rejected input"
    );

    Ok(())
}

#[tokio::test]
async fn turn_start_rejects_invalid_permission_selection_before_starting_turn() -> Result<()> {
    let codex_home = TempDir::new()?;
    ManagedWhisplyConfig::new()
        .enable_feature(Feature::Personality)
        .write(codex_home.path())?;
    std::fs::write(
        codex_home.path().join("managed_config.toml"),
        "sandbox_mode = \"read-only\"\n",
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build_initialized()
        .await?;

    let ThreadStartResponse { thread, .. } = mcp
        .start_thread(ThreadStartParams {
            model: Some("mock-model".to_string()),
            ..Default::default()
        })
        .await?;
    let turn_req = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id,
            client_user_message_id: None,
            input: vec![V2UserInput::Text {
                text: "Hello".to_string(),
                text_elements: Vec::new(),
            }],
            permissions: Some(BUILT_IN_PERMISSION_PROFILE_DANGER_FULL_ACCESS.to_string()),
            ..Default::default()
        })
        .await?;
    let err: JSONRPCError = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(turn_req)),
    )
    .await??;

    assert_eq!(err.error.code, INVALID_REQUEST_ERROR_CODE);
    assert!(
        err.error
            .message
            .contains("`approval_policy = \"never\"` cannot be used"),
        "unexpected error message: {}",
        err.error.message
    );
    assert!(
        err.error
            .message
            .contains("requirements do not allow `sandbox_mode = \"danger-full-access\"`"),
        "unexpected error message: {}",
        err.error.message
    );
    let turn_started = tokio::time::timeout(
        std::time::Duration::from_millis(250),
        mcp.read_stream_until_notification_message("turn/started"),
    )
    .await;
    assert!(
        turn_started.is_err(),
        "did not expect a turn/started notification after rejected permissions selection"
    );

    Ok(())
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn turn_start_accepts_managed_network_profile_from_requirements() -> Result<()> {
    let responses = vec![create_final_assistant_message_sse_response("Done")?];
    let server = create_mock_responses_server_sequence_unchecked(responses).await;

    let codex_home = TempDir::new()?;
    ManagedWhisplyConfig::new()
        .enable_feature(Feature::NetworkProxy)
        .write(codex_home.path())?;
    std::fs::write(
        codex_home.path().join("requirements.toml"),
        r#"
default_permissions = "managed-network"

[allowed_permission_profiles]
managed-network = true
":read-only" = true

[permissions.managed-network]
extends = ":read-only"

[permissions.managed-network.network]
enabled = true
allow_local_binding = false

[permissions.managed-network.network.domains]
"packages.example" = "allow"
"#,
    )?;

    let mut app_server = app_test_support::managed_whisply_app_server_builder!(&server.uri())
        .with_codex_home(codex_home.path())
        .build_initialized()
        .await?;

    let ThreadStartResponse {
        thread,
        active_permission_profile,
        ..
    } = app_server
        .start_thread(ThreadStartParams {
            model: Some("mock-model".to_string()),
            ..Default::default()
        })
        .await?;
    let active_permission_profile =
        active_permission_profile.context("expected active permission profile")?;
    assert_eq!(active_permission_profile.id, "managed-network");

    let TurnStartResponse { turn } = app_server
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                thread_id: thread.id,
                client_user_message_id: None,
                input: vec![V2UserInput::Text {
                    text: "Use the managed network profile".to_string(),
                    text_elements: Vec::new(),
                }],
                permissions: Some("managed-network".to_string()),
                ..Default::default()
            },
        })
        .await?;
    assert!(
        !turn.id.is_empty(),
        "turn/start should resolve the managed profile's network configuration"
    );
    timeout(
        DEFAULT_READ_TIMEOUT,
        app_server.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    Ok(())
}

#[tokio::test]
async fn turn_start_rejects_unknown_environment_before_starting_turn() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let codex_home = TempDir::new()?;
    ManagedWhisplyConfig::new().write(codex_home.path())?;

    let mut mcp = app_test_support::managed_whisply_app_server_builder!(&server.uri())
        .with_codex_home(codex_home.path())
        .build_initialized()
        .await?;

    let ThreadStartResponse { thread, .. } = mcp
        .start_thread(ThreadStartParams {
            model: Some("mock-model".to_string()),
            ..Default::default()
        })
        .await?;

    let turn_req = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id,
            client_user_message_id: None,
            input: vec![V2UserInput::Text {
                text: "Hello".to_string(),
                text_elements: Vec::new(),
            }],
            environments: Some(vec![TurnEnvironmentParams {
                environment_id: "missing".to_string(),
                cwd: whisply_utils_absolute_path::AbsolutePathBuf::try_from(
                    codex_home.path().to_path_buf(),
                )?
                .into(),
                runtime_workspace_roots: None,
            }]),
            ..Default::default()
        })
        .await?;
    let err: JSONRPCError = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(turn_req)),
    )
    .await??;

    assert_eq!(err.id, RequestId::Integer(turn_req));
    assert_eq!(err.error.code, INVALID_REQUEST_ERROR_CODE);
    assert_eq!(err.error.message, "unknown turn environment id `missing`");
    let turn_started = tokio::time::timeout(
        std::time::Duration::from_millis(250),
        mcp.read_stream_until_notification_message("turn/started"),
    )
    .await;
    assert!(
        turn_started.is_err(),
        "did not expect a turn/started notification after rejected environments"
    );

    Ok(())
}

#[tokio::test]
async fn turn_start_emits_notifications_and_accepts_model_override() -> Result<()> {
    // Provide a mock server and config so model wiring is valid.
    // Three Codex turns hit the mock model (session start + two turn/start calls).
    let responses = vec![
        create_final_assistant_message_sse_response("Done")?,
        create_final_assistant_message_sse_response("Done")?,
        create_final_assistant_message_sse_response("Done")?,
    ];
    let server = create_mock_responses_server_sequence_unchecked(responses).await;

    let codex_home = TempDir::new()?;
    ManagedWhisplyConfig::new()
        .enable_feature(Feature::Personality)
        .write(codex_home.path())?;

    let mut mcp = app_test_support::managed_whisply_app_server_builder!(&server.uri())
        .with_codex_home(codex_home.path())
        .build_initialized()
        .await?;

    // Start a thread (v2) and capture its id.
    let ThreadStartResponse { thread, .. } = mcp
        .start_thread(ThreadStartParams {
            model: Some("mock-model".to_string()),
            ..Default::default()
        })
        .await?;

    // Start a turn with only input and thread_id set (no overrides).
    let TurnStartResponse { turn } = mcp
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                thread_id: thread.id.clone(),
                client_user_message_id: None,
                input: vec![V2UserInput::Text {
                    text: "Hello".to_string(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            },
        })
        .await?;
    assert!(!turn.id.is_empty());

    // Expect a turn/started notification.
    let started: TurnStartedNotification =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_notification("turn/started")).await??;
    assert_eq!(started.thread_id, thread.id);
    assert_eq!(
        started.turn.status,
        codex_app_server_protocol::TurnStatus::InProgress
    );
    assert_eq!(started.turn.id, turn.id);
    assert_eq!(started.turn.items_view, TurnItemsView::NotLoaded);
    assert!(started.turn.items.is_empty());

    let completed: TurnCompletedNotification = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_notification("turn/completed"),
    )
    .await??;
    assert_eq!(completed.thread_id, thread.id);
    assert_eq!(completed.turn.id, turn.id);
    assert_eq!(completed.turn.status, TurnStatus::Completed);

    // Send a second turn that exercises the overrides path: change the model.
    let TurnStartResponse { turn: turn2 } = mcp
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                thread_id: thread.id.clone(),
                client_user_message_id: None,
                input: vec![V2UserInput::Text {
                    text: "Second".to_string(),
                    text_elements: Vec::new(),
                }],
                model: Some("mock-model-override".to_string()),
                ..Default::default()
            },
        })
        .await?;
    assert!(!turn2.id.is_empty());
    // Ensure the second turn has a different id than the first.
    assert_ne!(turn.id, turn2.id);

    let started2: TurnStartedNotification =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_notification("turn/started")).await??;
    assert_eq!(started2.thread_id, thread.id);
    assert_eq!(started2.turn.id, turn2.id);
    assert_eq!(started2.turn.status, TurnStatus::InProgress);
    assert_eq!(started2.turn.items_view, TurnItemsView::NotLoaded);
    assert!(started2.turn.items.is_empty());

    let completed2: TurnCompletedNotification = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_notification("turn/completed"),
    )
    .await??;
    assert_eq!(completed2.thread_id, thread.id);
    assert_eq!(completed2.turn.id, turn2.id);
    assert_eq!(completed2.turn.status, TurnStatus::Completed);

    Ok(())
}

#[tokio::test]
async fn turn_start_accepts_collaboration_mode_override_v2() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    let body = responses::sse(vec![
        responses::ev_response_created("resp-1"),
        responses::ev_assistant_message("msg-1", "Done"),
        responses::ev_completed("resp-1"),
    ]);
    let response_mock = responses::mount_sse_once(&server, body).await;

    let codex_home = TempDir::new()?;
    ManagedWhisplyConfig::new().write(codex_home.path())?;

    let mut mcp = app_test_support::managed_whisply_app_server_builder!(&server.uri())
        .with_codex_home(codex_home.path())
        .build_initialized()
        .await?;

    let ThreadStartResponse { thread, .. } = mcp
        .start_thread(ThreadStartParams {
            model: Some("gpt-5.4".to_string()),
            ..Default::default()
        })
        .await?;

    let collaboration_mode = CollaborationMode {
        mode: ModeKind::Default,
        settings: Settings {
            model: "mock-model-collab".to_string(),
            reasoning_effort: Some(ReasoningEffort::High),
            developer_instructions: None,
        },
    };

    let _turn: TurnStartResponse = mcp
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                thread_id: thread.id.clone(),
                client_user_message_id: None,
                input: vec![V2UserInput::Text {
                    text: "Hello".to_string(),
                    text_elements: Vec::new(),
                }],
                model: Some("mock-model-override".to_string()),
                effort: Some(ReasoningEffort::Low),
                summary: Some(ReasoningSummary::Auto),
                output_schema: None,
                collaboration_mode: Some(collaboration_mode),
                ..Default::default()
            },
        })
        .await?;

    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let request = response_mock.single_request();
    let payload = request.body_json();
    assert_eq!(payload["model"].as_str(), Some("mock-model-collab"));
    let payload_text = payload.to_string();
    assert!(payload_text.contains(
        "Use the `request_user_input` tool only when it is listed in the available tools"
    ));

    Ok(())
}

#[tokio::test]
async fn turn_start_uses_thread_feature_overrides_for_request_user_input_tool_description_v2()
-> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    let body = responses::sse(vec![
        responses::ev_response_created("resp-1"),
        responses::ev_assistant_message("msg-1", "Done"),
        responses::ev_completed("resp-1"),
    ]);
    let response_mock = responses::mount_sse_once(&server, body).await;

    let codex_home = TempDir::new()?;
    ManagedWhisplyConfig::new().write(codex_home.path())?;

    let mut mcp = app_test_support::managed_whisply_app_server_builder!(&server.uri())
        .with_codex_home(codex_home.path())
        .build_initialized()
        .await?;

    let ThreadStartResponse { thread, .. } = mcp
        .start_thread(ThreadStartParams {
            model: Some("gpt-5.4".to_string()),
            config: Some(HashMap::from([(
                "features.default_mode_request_user_input".to_string(),
                json!(true),
            )])),
            ..Default::default()
        })
        .await?;

    let collaboration_mode = CollaborationMode {
        mode: ModeKind::Default,
        settings: Settings {
            model: "mock-model-collab".to_string(),
            reasoning_effort: Some(ReasoningEffort::High),
            developer_instructions: None,
        },
    };

    let _turn: TurnStartResponse = mcp
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                thread_id: thread.id.clone(),
                client_user_message_id: None,
                input: vec![V2UserInput::Text {
                    text: "Hello".to_string(),
                    text_elements: Vec::new(),
                }],
                model: Some("mock-model-override".to_string()),
                effort: Some(ReasoningEffort::Low),
                summary: Some(ReasoningSummary::Auto),
                output_schema: None,
                collaboration_mode: Some(collaboration_mode),
                ..Default::default()
            },
        })
        .await?;

    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let request = response_mock.single_request();
    let payload_text = request.body_json().to_string();
    assert!(payload_text.contains("This tool is only available in Default or Plan mode."));

    Ok(())
}

#[tokio::test]
async fn turn_start_accepts_personality_override_v2() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    let body = responses::sse(vec![
        responses::ev_response_created("resp-1"),
        responses::ev_assistant_message("msg-1", "Done"),
        responses::ev_completed("resp-1"),
    ]);
    let response_mock = responses::mount_sse_once(&server, body).await;

    let codex_home = TempDir::new()?;
    ManagedWhisplyConfig::new()
        .enable_feature(Feature::Personality)
        .write(codex_home.path())?;

    let mut mcp = app_test_support::managed_whisply_app_server_builder!(&server.uri())
        .with_codex_home(codex_home.path())
        .build_initialized()
        .await?;

    let ThreadStartResponse { thread, .. } = mcp
        .start_thread(ThreadStartParams {
            model: Some("exp-codex-personality".to_string()),
            ..Default::default()
        })
        .await?;

    let _turn: TurnStartResponse = mcp
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                thread_id: thread.id.clone(),
                client_user_message_id: None,
                input: vec![V2UserInput::Text {
                    text: "Hello".to_string(),
                    text_elements: Vec::new(),
                }],
                personality: Some(Personality::Friendly),
                ..Default::default()
            },
        })
        .await?;

    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let request = response_mock.single_request();
    let developer_texts = request.message_input_texts("developer");
    if developer_texts.is_empty() {
        eprintln!("request body: {}", request.body_json());
    }

    assert!(
        developer_texts
            .iter()
            .any(|text| text.contains("<personality_spec>")),
        "expected personality update message in developer input, got {developer_texts:?}"
    );

    Ok(())
}

#[tokio::test]
async fn turn_start_ignores_deprecated_multi_agent_mode() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    let body = responses::sse(vec![
        responses::ev_response_created("resp-1"),
        responses::ev_assistant_message("msg-1", "Done"),
        responses::ev_completed("resp-1"),
    ]);
    let response_mock = responses::mount_sse_once(&server, body).await;

    let codex_home = TempDir::new()?;
    ManagedWhisplyConfig::new()
        .enable_feature(Feature::MultiAgentV2)
        .write(codex_home.path())?;

    let mut mcp = app_test_support::managed_whisply_app_server_builder!(&server.uri())
        .with_codex_home(codex_home.path())
        .build_initialized()
        .await?;

    let ThreadStartResponse { thread, .. } = mcp
        .start_thread(ThreadStartParams {
            model: Some("mock-model".to_string()),
            ..Default::default()
        })
        .await?;

    let _: TurnStartResponse = mcp
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                thread_id: thread.id,
                input: vec![V2UserInput::Text {
                    text: "Hello".to_string(),
                    text_elements: Vec::new(),
                }],
                multi_agent_mode: Some(MultiAgentMode::Proactive),
                ..Default::default()
            },
        })
        .await?;

    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let developer_texts = response_mock
        .single_request()
        .message_input_texts("developer");
    assert!(developer_texts.iter().any(|text| {
        text.contains(
            "Do not spawn sub-agents unless the user or applicable AGENTS.md/skill instructions explicitly ask for sub-agents",
        )
    }));
    assert!(
        !developer_texts
            .iter()
            .any(|text| text.contains("Proactive multi-agent delegation is active."))
    );

    Ok(())
}

#[tokio::test]
async fn thread_start_ignores_deprecated_multi_agent_mode() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    let body = responses::sse(vec![
        responses::ev_response_created("resp-1"),
        responses::ev_assistant_message("msg-1", "Done"),
        responses::ev_completed("resp-1"),
    ]);
    let response_mock = responses::mount_sse_once(&server, body).await;

    let codex_home = TempDir::new()?;
    ManagedWhisplyConfig::new()
        .enable_feature(Feature::MultiAgentV2)
        .write(codex_home.path())?;

    let mut mcp = app_test_support::managed_whisply_app_server_builder!(&server.uri())
        .with_codex_home(codex_home.path())
        .build_initialized()
        .await?;

    let ThreadStartResponse {
        thread,
        multi_agent_mode,
        ..
    } = mcp
        .start_thread(ThreadStartParams {
            model: Some("mock-model".to_string()),
            multi_agent_mode: Some(MultiAgentMode::Proactive),
            ..Default::default()
        })
        .await?;
    assert_eq!(multi_agent_mode, MultiAgentMode::ExplicitRequestOnly);

    let _: TurnStartResponse = mcp
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                thread_id: thread.id,
                input: vec![V2UserInput::Text {
                    text: "Hello".to_string(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            },
        })
        .await?;

    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let developer_texts = response_mock
        .single_request()
        .message_input_texts("developer");
    assert!(developer_texts.iter().any(|text| {
        text.contains(MULTI_AGENT_MODE_OPEN_TAG)
            && text.contains(
                "Do not spawn sub-agents unless the user or applicable AGENTS.md/skill instructions explicitly ask for sub-agents",
            )
    }));
    assert!(
        !developer_texts
            .iter()
            .any(|text| text.contains("Proactive multi-agent delegation is active."))
    );

    Ok(())
}

#[tokio::test]
async fn turn_start_change_personality_mid_thread_v2() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    let sse1 = responses::sse(vec![
        responses::ev_response_created("resp-1"),
        responses::ev_assistant_message("msg-1", "Done"),
        responses::ev_completed("resp-1"),
    ]);
    let sse2 = responses::sse(vec![
        responses::ev_response_created("resp-2"),
        responses::ev_assistant_message("msg-2", "Done"),
        responses::ev_completed("resp-2"),
    ]);
    let response_mock = responses::mount_sse_sequence(&server, vec![sse1, sse2]).await;

    let codex_home = TempDir::new()?;
    ManagedWhisplyConfig::new()
        .enable_feature(Feature::Personality)
        .write(codex_home.path())?;

    let mut mcp = app_test_support::managed_whisply_app_server_builder!(&server.uri())
        .with_codex_home(codex_home.path())
        .build_initialized()
        .await?;

    let ThreadStartResponse { thread, .. } = mcp
        .start_thread(ThreadStartParams {
            model: Some("exp-codex-personality".to_string()),
            ..Default::default()
        })
        .await?;

    let _turn: TurnStartResponse = mcp
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                thread_id: thread.id.clone(),
                client_user_message_id: None,
                input: vec![V2UserInput::Text {
                    text: "Hello".to_string(),
                    text_elements: Vec::new(),
                }],
                personality: None,
                ..Default::default()
            },
        })
        .await?;

    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let _turn2: TurnStartResponse = mcp
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                thread_id: thread.id.clone(),
                client_user_message_id: None,
                input: vec![V2UserInput::Text {
                    text: "Hello again".to_string(),
                    text_elements: Vec::new(),
                }],
                personality: Some(Personality::Friendly),
                ..Default::default()
            },
        })
        .await?;

    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let requests = response_mock.requests();
    assert_eq!(requests.len(), 2, "expected two requests");

    let first_developer_texts = requests[0].message_input_texts("developer");
    assert!(
        first_developer_texts
            .iter()
            .all(|text| !text.contains("<personality_spec>")),
        "expected no personality update message in first request, got {first_developer_texts:?}"
    );

    let second_developer_texts = requests[1].message_input_texts("developer");
    assert!(
        second_developer_texts
            .iter()
            .any(|text| text.contains("<personality_spec>")),
        "expected personality update message in second request, got {second_developer_texts:?}"
    );

    Ok(())
}

#[tokio::test]
async fn turn_start_defaults_local_image_detail_to_high() -> Result<()> {
    let input_images = run_local_image_turn(/*detail*/ None).await?;

    assert_eq!(input_images.len(), 1);
    assert_eq!(
        input_images[0].get("detail").and_then(Value::as_str),
        Some("high")
    );

    Ok(())
}

#[tokio::test]
async fn turn_start_forwards_custom_local_image_detail() -> Result<()> {
    let input_images = run_local_image_turn(Some(ImageDetail::Original)).await?;

    assert_eq!(input_images.len(), 1);
    assert_eq!(
        input_images[0].get("detail").and_then(Value::as_str),
        Some("original")
    );

    Ok(())
}

#[tokio::test]
async fn turn_start_exec_approval_toggle_v2() -> Result<()> {
    // TODO(anp): Remove after shell-command approval routing supports target-native Windows cwd.
    skip_if_wine_exec!(
        Ok(()),
        "shell-command approval routing requires a host-native cwd under Wine-exec"
    );
    skip_if_no_network!(Ok(()));

    let tmp = TempDir::new()?;
    let codex_home = tmp.path().to_path_buf();
    let bearer_token = "example_bearer_token_1234567890";
    let first_shell_command = vec![
        "python3".to_string(),
        "-c".to_string(),
        "import sys; print(sys.argv[1].endswith('7890'))".to_string(),
        format!("Authorization: Bearer {bearer_token}"),
    ];
    let expected_approval_command = format_with_current_shell_display(&shlex::try_join(
        first_shell_command.iter().map(String::as_str),
    )?);
    let expected_display_command =
        expected_approval_command.replace(bearer_token, "[REDACTED_SECRET]");

    // Mock server: first turn requests a shell call (elicitation), then completes.
    // Second turn same, but we'll set approval_policy=never to avoid elicitation.
    let responses = vec![
        create_shell_command_sse_response(
            first_shell_command,
            /*workdir*/ None,
            Some(5000),
            "call1",
        )?,
        create_final_assistant_message_sse_response("done 1")?,
        create_shell_command_sse_response(
            vec![
                "python3".to_string(),
                "-c".to_string(),
                "print(42)".to_string(),
            ],
            /*workdir*/ None,
            Some(5000),
            "call2",
        )?,
        create_final_assistant_message_sse_response("done 2")?,
    ];
    let server = create_mock_responses_server_sequence(responses).await;
    // Default approval is untrusted to force elicitation on first turn.
    ManagedWhisplyConfig::new()
        .with_approval_policy("untrusted")
        .write(codex_home.as_path())?;

    let mut mcp = app_test_support::managed_whisply_app_server_builder!(&server.uri())
        .with_codex_home(codex_home.as_path())
        .build_initialized()
        .await?;
    let expected_environment_id = mcp.auto_env_params()?.environment_id;

    // thread/start
    let ThreadStartResponse { thread, .. } = mcp
        .start_thread(ThreadStartParams {
            model: Some("mock-model".to_string()),
            ..Default::default()
        })
        .await?;

    // turn/start — expect CommandExecutionRequestApproval request from server
    let first_turn_id = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id.clone(),
            client_user_message_id: None,
            input: vec![V2UserInput::Text {
                text: "run python".to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    // Acknowledge RPC
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(first_turn_id)),
    )
    .await??;

    // Receive elicitation
    let server_req = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_request_message(),
    )
    .await??;
    let ServerRequest::CommandExecutionRequestApproval { request_id, params } = server_req else {
        panic!("expected CommandExecutionRequestApproval request");
    };
    assert_eq!(params.item_id, "call1");
    assert_eq!(
        params.environment_id.as_deref(),
        Some(expected_environment_id.as_str())
    );
    assert_eq!(
        params.command.as_deref(),
        Some(expected_approval_command.as_str())
    );
    let resolved_request_id = request_id.clone();

    // Approve and wait for task completion
    mcp.send_response(
        request_id,
        serde_json::to_value(CommandExecutionRequestApprovalResponse {
            decision: CommandExecutionApprovalDecision::Accept,
        })?,
    )
    .await?;
    let mut saw_resolved = false;
    let mut saw_completed_command = false;
    loop {
        let message = timeout(DEFAULT_READ_TIMEOUT, mcp.read_next_message()).await??;
        let JSONRPCMessage::Notification(notification) = message else {
            continue;
        };
        match notification.method.as_str() {
            "item/completed" => {
                let completed: ItemCompletedNotification =
                    serde_json::from_value(notification.params.expect("item/completed params"))?;
                match completed.item {
                    ThreadItem::CommandExecution {
                        id,
                        command,
                        exit_code,
                        aggregated_output,
                        ..
                    } if id == "call1" => {
                        assert_eq!(command, expected_display_command);
                        assert_eq!(exit_code, Some(0));
                        assert!(aggregated_output.is_some_and(|output| output.contains("True")));
                        saw_completed_command = true;
                    }
                    _ => {}
                }
            }
            "serverRequest/resolved" => {
                let resolved: ServerRequestResolvedNotification = serde_json::from_value(
                    notification
                        .params
                        .clone()
                        .expect("serverRequest/resolved params"),
                )?;
                assert_eq!(resolved.thread_id, thread.id);
                assert_eq!(resolved.request_id, resolved_request_id);
                saw_resolved = true;
            }
            "turn/completed" => {
                assert!(saw_resolved, "serverRequest/resolved should arrive first");
                assert!(saw_completed_command, "expected completed command item");
                break;
            }
            _ => {}
        }
    }

    // Second turn with approval_policy=never should not elicit approval
    let _: TurnStartResponse = mcp
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                thread_id: thread.id.clone(),
                client_user_message_id: None,
                input: vec![V2UserInput::Text {
                    text: "run python again".to_string(),
                    text_elements: Vec::new(),
                }],
                approval_policy: Some(codex_app_server_protocol::AskForApproval::Never),
                sandbox_policy: Some(codex_app_server_protocol::SandboxPolicy::DangerFullAccess),
                model: Some("mock-model".to_string()),
                effort: Some(ReasoningEffort::Medium),
                summary: Some(ReasoningSummary::Auto),
                ..Default::default()
            },
        })
        .await?;

    // Ensure we do NOT receive a CommandExecutionRequestApproval request before task completes
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    Ok(())
}

#[tokio::test]
async fn turn_start_exec_approval_decline_v2() -> Result<()> {
    run_turn_start_exec_approval_rejection_v2(
        serde_json::to_value(CommandExecutionRequestApprovalResponse {
            decision: CommandExecutionApprovalDecision::Decline,
        })?,
        CommandExecutionStatus::Declined,
        "rejected by user",
    )
    .await
}

#[tokio::test]
async fn turn_start_exec_approval_invalid_response_v2() -> Result<()> {
    run_turn_start_exec_approval_rejection_v2(
        json!({ "unexpected": "response" }),
        CommandExecutionStatus::Failed,
        "approval request failed",
    )
    .await
}

async fn run_turn_start_exec_approval_rejection_v2(
    approval_response: Value,
    expected_status: CommandExecutionStatus,
    expected_rejection: &str,
) -> Result<()> {
    // TODO(anp): Remove after command approval routing accepts target-native Windows cwd.
    skip_if_wine_exec!(
        Ok(()),
        "command approval routing rejects the selected Windows cwd on the Linux host"
    );
    skip_if_no_network!(Ok(()));

    let tmp = TempDir::new()?;
    let codex_home = tmp.path().to_path_buf();
    let bearer_token = "example_bearer_token_1234567890";
    let shell_command = vec![
        "python3".to_string(),
        "-c".to_string(),
        "print(42)".to_string(),
        format!("Authorization: Bearer {bearer_token}"),
    ];
    let expected_approval_command = format_with_current_shell_display(&shlex::try_join(
        shell_command.iter().map(String::as_str),
    )?);
    let expected_display_command =
        expected_approval_command.replace(bearer_token, "[REDACTED_SECRET]");

    let responses = vec![
        create_shell_command_sse_response(
            shell_command,
            /*workdir*/ None,
            Some(5000),
            "call-decline",
        )?,
        create_final_assistant_message_sse_response("done")?,
    ];
    let server = create_mock_responses_server_sequence(responses).await;
    ManagedWhisplyConfig::new()
        .with_approval_policy("untrusted")
        .write(codex_home.as_path())?;

    let mut mcp = app_test_support::managed_whisply_app_server_builder!(&server.uri())
        .with_codex_home(codex_home.as_path())
        .build_initialized()
        .await?;

    let ThreadStartResponse { thread, .. } = mcp
        .start_thread(ThreadStartParams {
            model: Some("mock-model".to_string()),
            ..Default::default()
        })
        .await?;

    let TurnStartResponse { turn } = mcp
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                thread_id: thread.id.clone(),
                client_user_message_id: None,
                input: vec![V2UserInput::Text {
                    text: "run python".to_string(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            },
        })
        .await?;

    let started_command_execution = timeout(DEFAULT_READ_TIMEOUT, async {
        loop {
            let started: ItemStartedNotification = mcp.read_notification("item/started").await?;
            if let ThreadItem::CommandExecution { .. } = started.item {
                return Ok::<ThreadItem, anyhow::Error>(started.item);
            }
        }
    })
    .await??;
    let ThreadItem::CommandExecution {
        id,
        status,
        command,
        command_actions,
        ..
    } = started_command_execution
    else {
        unreachable!("loop ensures we break on command execution items");
    };
    assert_eq!(id, "call-decline");
    assert_eq!(status, CommandExecutionStatus::InProgress);
    assert_eq!(command, expected_display_command);
    let displayed_actions = serde_json::to_string(&command_actions)?;
    assert!(displayed_actions.contains("[REDACTED_SECRET]"));
    assert!(!displayed_actions.contains(bearer_token));

    let server_req = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_request_message(),
    )
    .await??;
    let ServerRequest::CommandExecutionRequestApproval { request_id, params } = server_req else {
        panic!("expected CommandExecutionRequestApproval request")
    };
    assert_eq!(params.item_id, "call-decline");
    assert_eq!(params.thread_id, thread.id);
    assert_eq!(params.turn_id, turn.id);
    assert_eq!(
        params.command.as_deref(),
        Some(expected_approval_command.as_str())
    );
    let approval_actions = serde_json::to_string(&params.command_actions)?;
    assert!(approval_actions.contains(bearer_token));

    mcp.send_response(request_id, approval_response).await?;

    let completed_command_execution = timeout(DEFAULT_READ_TIMEOUT, async {
        loop {
            let completed: ItemCompletedNotification =
                mcp.read_notification("item/completed").await?;
            if let ThreadItem::CommandExecution { .. } = completed.item {
                return Ok::<ThreadItem, anyhow::Error>(completed.item);
            }
        }
    })
    .await??;
    let ThreadItem::CommandExecution {
        id,
        status,
        command,
        command_actions,
        exit_code,
        aggregated_output,
        ..
    } = completed_command_execution
    else {
        unreachable!("loop ensures we break on command execution items");
    };
    assert_eq!(id, "call-decline");
    assert_eq!(status, expected_status);
    assert_eq!(command, expected_display_command);
    let displayed_actions = serde_json::to_string(&command_actions)?;
    assert!(displayed_actions.contains("[REDACTED_SECRET]"));
    assert!(!displayed_actions.contains(bearer_token));
    assert!(exit_code.is_none());
    assert!(aggregated_output.is_none());

    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let requests = server
        .received_requests()
        .await
        .context("failed to fetch received requests")?;
    assert!(
        requests.iter().any(|request| {
            request.url.path().ends_with("/responses") && body_contains(request, expected_rejection)
        }),
        "model request should include approval rejection: {expected_rejection}"
    );

    Ok(())
}

#[tokio::test]
async fn turn_start_explicit_local_environment_updates_legacy_cwd_between_turns() -> Result<()> {
    // TODO(anp): Materialize cwd and shell-display fixtures in the selected remote environment.
    skip_if_remote!(Ok(()), "cwd fixtures are only materialized on the host");
    skip_if_no_network!(Ok(()));

    let tmp = TempDir::new()?;
    let codex_home = tmp.path().join("codex_home");
    std::fs::create_dir(&codex_home)?;
    let workspace_root = tmp.path().join("workspace");
    std::fs::create_dir(&workspace_root)?;
    let first_cwd = workspace_root.join("turn1");
    let second_cwd = workspace_root.join("turn2");
    std::fs::create_dir(&first_cwd)?;
    std::fs::create_dir(&second_cwd)?;

    let responses = vec![
        create_shell_command_sse_response(
            vec!["echo".to_string(), "first".to_string(), "turn".to_string()],
            /*workdir*/ None,
            Some(5000),
            "call-first",
        )?,
        create_final_assistant_message_sse_response("done first")?,
        create_shell_command_sse_response(
            vec!["echo".to_string(), "second".to_string(), "turn".to_string()],
            /*workdir*/ None,
            Some(5000),
            "call-second",
        )?,
        create_final_assistant_message_sse_response("done second")?,
    ];
    let server = create_mock_responses_server_sequence(responses).await;
    ManagedWhisplyConfig::new()
        .with_approval_policy("untrusted")
        .write(&codex_home)?;

    let mut mcp = app_test_support::managed_whisply_app_server_builder!(&server.uri())
        .with_codex_home(&codex_home)
        .build_initialized()
        .await?;

    // thread/start
    let ThreadStartResponse { thread, .. } = mcp
        .start_thread(ThreadStartParams {
            model: Some("mock-model".to_string()),
            ..Default::default()
        })
        .await?;

    // first turn with workspace-write sandbox and first_cwd
    let first_writable_root =
        whisply_utils_absolute_path::AbsolutePathBuf::try_from(first_cwd.clone())?;
    let _: TurnStartResponse = mcp
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                environments: None,
                thread_id: thread.id.clone(),
                client_user_message_id: None,
                input: vec![V2UserInput::Text {
                    text: "first turn".to_string(),
                    text_elements: Vec::new(),
                }],
                responsesapi_client_metadata: None,
                additional_context: None,
                cwd: Some(first_cwd.clone()),
                runtime_workspace_roots: None,
                approval_policy: Some(codex_app_server_protocol::AskForApproval::Never),
                approvals_reviewer: None,
                sandbox_policy: Some(codex_app_server_protocol::SandboxPolicy::WorkspaceWrite {
                    writable_roots: vec![first_writable_root],
                    network_access: false,
                    exclude_tmpdir_env_var: true,
                    exclude_slash_tmp: true,
                }),
                permissions: None,
                model: Some("mock-model".to_string()),
                effort: Some(ReasoningEffort::Medium),
                summary: Some(ReasoningSummary::Auto),
                service_tier: None,
                personality: None,
                output_schema: None,
                collaboration_mode: None,
                multi_agent_mode: None,
            },
        })
        .await?;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;
    mcp.clear_message_buffer();

    // Select a new local cwd without the top-level compatibility parameter. The inherited
    // workspace-write sandbox must follow the local environment cwd.
    let _: TurnStartResponse = mcp
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                environments: Some(vec![TurnEnvironmentParams {
                    environment_id: LOCAL_ENVIRONMENT_ID.to_string(),
                    cwd: second_cwd.abs().into(),
                    runtime_workspace_roots: None,
                }]),
                thread_id: thread.id.clone(),
                client_user_message_id: None,
                input: vec![V2UserInput::Text {
                    text: "second turn".to_string(),
                    text_elements: Vec::new(),
                }],
                responsesapi_client_metadata: None,
                additional_context: None,
                cwd: None,
                runtime_workspace_roots: None,
                approval_policy: Some(codex_app_server_protocol::AskForApproval::Never),
                approvals_reviewer: None,
                sandbox_policy: None,
                permissions: None,
                model: Some("mock-model".to_string()),
                effort: Some(ReasoningEffort::Medium),
                summary: Some(ReasoningSummary::Auto),
                service_tier: None,
                personality: None,
                output_schema: None,
                collaboration_mode: None,
                multi_agent_mode: None,
            },
        })
        .await?;
    let settings_updated: ThreadSettingsUpdatedNotification = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_notification("thread/settings/updated"),
    )
    .await??;
    assert_eq!(settings_updated.thread_settings.cwd, second_cwd.abs());

    let command_exec_item = timeout(DEFAULT_READ_TIMEOUT, async {
        loop {
            let item_started: ItemStartedNotification =
                mcp.read_notification("item/started").await?;
            if matches!(item_started.item, ThreadItem::CommandExecution { .. }) {
                return Ok::<ThreadItem, anyhow::Error>(item_started.item);
            }
        }
    })
    .await??;
    let ThreadItem::CommandExecution {
        cwd,
        command,
        status,
        ..
    } = command_exec_item
    else {
        unreachable!("loop ensures we break on command execution items");
    };
    assert_eq!(cwd.as_str(), second_cwd.to_string_lossy().as_ref());
    let expected_command = format_with_current_shell_display("echo second turn");
    assert_eq!(command, expected_command);
    assert_eq!(status, CommandExecutionStatus::InProgress);

    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn turn_start_permission_profile_rebinds_runtime_workspace_roots_between_turns() -> Result<()>
{
    skip_if_no_network!(Ok(()));

    let tmp = TempDir::new()?;
    let codex_home = tmp.path().join("codex_home");
    std::fs::create_dir(&codex_home)?;
    let old_root = tmp.path().join("old-root");
    let new_root = tmp.path().join("new-root");
    std::fs::create_dir(&old_root)?;
    std::fs::create_dir(&new_root)?;
    let old_root_text = old_root.to_string_lossy().into_owned();
    let new_root_text = new_root.to_string_lossy().into_owned();
    let old_root = whisply_utils_absolute_path::AbsolutePathBuf::from_absolute_path(old_root)?;
    let new_root = whisply_utils_absolute_path::AbsolutePathBuf::from_absolute_path(new_root)?;

    let server = responses::start_mock_server().await;
    let response_mock = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![
                responses::ev_response_created("resp-1"),
                responses::ev_assistant_message("msg-1", "done first"),
                responses::ev_completed("resp-1"),
            ]),
            responses::sse(vec![
                responses::ev_response_created("resp-2"),
                responses::ev_assistant_message("msg-2", "done second"),
                responses::ev_completed("resp-2"),
            ]),
        ],
    )
    .await;
    ManagedWhisplyConfig::new()
        .with_additional_config(
            r#"
default_permissions = "dev"

[permissions.dev.filesystem.":workspace_roots"]
"." = "write"
"#,
        )
        .write(&codex_home)?;

    let mut mcp = app_test_support::managed_whisply_app_server_builder!(&server.uri())
        .with_codex_home(&codex_home)
        .build_initialized()
        .await?;

    let ThreadStartResponse { thread, .. } = mcp
        .start_thread(ThreadStartParams {
            model: Some("mock-model".to_string()),
            ..Default::default()
        })
        .await?;

    let _: TurnStartResponse = mcp
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                thread_id: thread.id.clone(),
                client_user_message_id: None,
                input: vec![V2UserInput::Text {
                    text: "select dev profile".to_string(),
                    text_elements: Vec::new(),
                }],
                runtime_workspace_roots: Some(vec![old_root]),
                permissions: Some("dev".to_string()),
                ..Default::default()
            },
        })
        .await?;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let _: TurnStartResponse = mcp
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                thread_id: thread.id.clone(),
                client_user_message_id: None,
                input: vec![V2UserInput::Text {
                    text: "write in new root".to_string(),
                    text_elements: Vec::new(),
                }],
                runtime_workspace_roots: Some(vec![new_root]),
                ..Default::default()
            },
        })
        .await?;

    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let requests = response_mock.requests();
    assert_eq!(requests.len(), 2, "expected two Responses API requests");
    let latest_permissions_instructions =
        |request: &core_test_support::responses::ResponsesRequest| {
            request
                .message_input_texts("developer")
                .into_iter()
                .rev()
                .find(|text| text.contains("<permissions instructions>"))
                .expect("permissions instructions")
        };
    let first_permissions = latest_permissions_instructions(&requests[0]);
    assert!(first_permissions.contains(&old_root_text));
    assert!(
        !first_permissions.contains(&new_root_text),
        "first turn should materialize the initial runtime workspace root"
    );

    let second_permissions = latest_permissions_instructions(&requests[1]);
    assert!(second_permissions.contains(&new_root_text));
    assert!(
        !second_permissions.contains(&old_root_text),
        "second turn should rebind :workspace_roots to the updated runtime workspace root"
    );

    Ok(())
}

#[tokio::test]
async fn turn_start_resolves_sticky_thread_local_environment_and_turn_overrides() -> Result<()> {
    let tmp = TempDir::new()?;
    let codex_home = tmp.path().join("codex_home");
    std::fs::create_dir(&codex_home)?;
    let workspace = tmp.path().join("workspace");
    std::fs::create_dir(&workspace)?;

    let server = create_mock_responses_server_repeating_assistant("done").await;
    ManagedWhisplyConfig::new().write(&codex_home)?;
    std::fs::write(
        codex_home.join("environments.toml"),
        r#"
[[environments]]
id = "remote"
url = "ws://127.0.0.1:1"
"#,
    )?;

    let mut mcp = app_test_support::managed_whisply_app_server_builder!(&server.uri())
        .with_codex_home(&codex_home)
        // This test owns environments.toml and explicitly compares local selections
        // with a configured remote environment, so auto env would change its subject.
        .without_auto_env()
        .build_initialized()
        .await?;

    for case in [
        EnvironmentSelectionCase {
            name: "sticky_unset_turn_unset",
            sticky: None,
            turn: None,
        },
        EnvironmentSelectionCase {
            name: "sticky_empty_turn_unset",
            sticky: Some(&[]),
            turn: None,
        },
        EnvironmentSelectionCase {
            name: "sticky_local_turn_unset",
            sticky: Some(&["local"]),
            turn: None,
        },
        EnvironmentSelectionCase {
            name: "sticky_local_turn_empty",
            sticky: Some(&["local"]),
            turn: Some(&[]),
        },
        EnvironmentSelectionCase {
            name: "sticky_empty_turn_local",
            sticky: Some(&[]),
            turn: Some(&["local"]),
        },
    ] {
        run_environment_selection_case(&mut mcp, &workspace, case).await?;
    }

    Ok(())
}

struct EnvironmentSelectionCase {
    name: &'static str,
    sticky: Option<&'static [&'static str]>,
    turn: Option<&'static [&'static str]>,
}

async fn run_environment_selection_case(
    mcp: &mut TestAppServer,
    workspace: &Path,
    case: EnvironmentSelectionCase,
) -> Result<()> {
    let thread_req = mcp
        .send_thread_start_request(ThreadStartParams {
            model: Some("mock-model".to_string()),
            cwd: Some(workspace.to_string_lossy().into_owned()),
            environments: environment_params(case.sticky, workspace),
            ..Default::default()
        })
        .await?;
    let ThreadStartResponse { thread, .. } =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(thread_req)).await??;

    let TurnStartResponse { turn } = mcp
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                thread_id: thread.id.clone(),
                client_user_message_id: None,
                input: vec![V2UserInput::Text {
                    text: format!("run {}", case.name),
                    text_elements: Vec::new(),
                }],
                environments: environment_params(case.turn, workspace),
                cwd: Some(workspace.to_path_buf()),
                model: Some("mock-model".to_string()),
                ..Default::default()
            },
        })
        .await?;

    let started: TurnStartedNotification =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_notification("turn/started")).await??;
    assert_eq!(started.turn.id, turn.id, "{}", case.name);

    let completed: TurnCompletedNotification = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_notification("turn/completed"),
    )
    .await??;
    assert_eq!(completed.turn.id, turn.id, "{}", case.name);
    assert_eq!(
        completed.turn.status,
        TurnStatus::Completed,
        "{}",
        case.name
    );

    mcp.clear_message_buffer();

    Ok(())
}

fn environment_params(ids: Option<&[&str]>, cwd: &Path) -> Option<Vec<TurnEnvironmentParams>> {
    ids.map(|ids| {
        ids.iter()
            .map(|id| TurnEnvironmentParams {
                environment_id: (*id).to_string(),
                cwd: cwd.abs().into(),
                runtime_workspace_roots: None,
            })
            .collect()
    })
}

#[tokio::test]
async fn turn_start_file_change_approval_v2() -> Result<()> {
    // TODO(anp): Materialize apply-patch workspaces in the selected remote environment.
    skip_if_remote!(
        Ok(()),
        "apply-patch workspace fixture is only materialized on the host"
    );
    skip_if_no_network!(Ok(()));

    let tmp = TempDir::new()?;
    let codex_home = tmp.path().join("codex_home");
    std::fs::create_dir(&codex_home)?;
    let workspace = tmp.path().join("workspace");
    std::fs::create_dir(&workspace)?;

    let patch = r#"*** Begin Patch
*** Add File: README.md
+new line
*** End Patch
"#;
    let responses = vec![
        create_apply_patch_sse_response(patch, "patch-call")?,
        create_final_assistant_message_sse_response("patch applied")?,
    ];
    let server = create_mock_responses_server_sequence_unchecked(responses).await;
    ManagedWhisplyConfig::new()
        .with_approval_policy("untrusted")
        // Snapshot startup is unrelated to the file-approval behavior under test.
        .disable_feature(Feature::ShellSnapshot)
        .write(&codex_home)?;

    let mut mcp = app_test_support::managed_whisply_app_server_builder!(&server.uri())
        .with_codex_home(&codex_home)
        .build_initialized()
        .await?;

    let ThreadStartResponse { thread, .. } = mcp
        .start_thread(ThreadStartParams {
            model: Some("mock-model".to_string()),
            cwd: Some(workspace.to_string_lossy().into_owned()),
            ..Default::default()
        })
        .await?;

    let TurnStartResponse { turn } = mcp
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                thread_id: thread.id.clone(),
                client_user_message_id: None,
                input: vec![V2UserInput::Text {
                    text: "apply patch".into(),
                    text_elements: Vec::new(),
                }],
                cwd: Some(workspace.clone()),
                ..Default::default()
            },
        })
        .await?;

    let started_file_change = timeout(DEFAULT_READ_TIMEOUT, async {
        loop {
            let started: ItemStartedNotification = mcp.read_notification("item/started").await?;
            if let ThreadItem::FileChange { .. } = started.item {
                return Ok::<ThreadItem, anyhow::Error>(started.item);
            }
        }
    })
    .await??;
    let ThreadItem::FileChange {
        ref id,
        status,
        ref changes,
    } = started_file_change
    else {
        unreachable!("loop ensures we break on file change items");
    };
    assert_eq!(id, "patch-call");
    assert_eq!(status, PatchApplyStatus::InProgress);
    let started_changes = changes.clone();

    let server_req = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_request_message(),
    )
    .await??;
    let ServerRequest::FileChangeRequestApproval { request_id, params } = server_req else {
        panic!("expected FileChangeRequestApproval request")
    };
    assert_eq!(params.item_id, "patch-call");
    assert_eq!(params.thread_id, thread.id);
    assert_eq!(params.turn_id, turn.id);
    let resolved_request_id = request_id.clone();
    let expected_readme_path = workspace.join("README.md");
    let expected_readme_path = expected_readme_path.to_string_lossy().into_owned();
    pretty_assertions::assert_eq!(
        started_changes,
        vec![codex_app_server_protocol::FileUpdateChange {
            path: expected_readme_path.clone(),
            kind: PatchChangeKind::Add,
            diff: "new line\n".to_string(),
        }]
    );

    mcp.send_response(
        request_id,
        serde_json::to_value(FileChangeRequestApprovalResponse {
            decision: FileChangeApprovalDecision::Accept,
        })?,
    )
    .await?;
    let mut saw_resolved = false;
    let mut completed_file_change: Option<ThreadItem> = None;
    while completed_file_change.is_none() {
        let message = timeout(DEFAULT_READ_TIMEOUT, mcp.read_next_message()).await??;
        let JSONRPCMessage::Notification(notification) = message else {
            continue;
        };
        match notification.method.as_str() {
            "serverRequest/resolved" => {
                let resolved: ServerRequestResolvedNotification = serde_json::from_value(
                    notification
                        .params
                        .clone()
                        .expect("serverRequest/resolved params"),
                )?;
                assert_eq!(resolved.thread_id, thread.id);
                assert_eq!(resolved.request_id, resolved_request_id);
                saw_resolved = true;
            }
            "item/completed" => {
                let completed: ItemCompletedNotification = serde_json::from_value(
                    notification.params.clone().expect("item/completed params"),
                )?;
                if let ThreadItem::FileChange { .. } = completed.item {
                    assert!(saw_resolved, "serverRequest/resolved should arrive first");
                    completed_file_change = Some(completed.item);
                }
            }
            _ => {}
        }
    }
    let completed_file_change =
        completed_file_change.expect("file change completion should be observed");
    let ThreadItem::FileChange { ref id, status, .. } = completed_file_change else {
        unreachable!("loop ensures we break on file change items");
    };
    assert_eq!(id, "patch-call");
    assert_eq!(status, PatchApplyStatus::Completed);

    let readme_contents = std::fs::read_to_string(expected_readme_path)?;
    assert_eq!(readme_contents, "new line\n");

    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let status = timeout(DEFAULT_READ_TIMEOUT, mcp.shutdown_gracefully()).await??;
    anyhow::ensure!(
        status.success(),
        "app-server exited unsuccessfully: {status}"
    );
    let response_requests = server
        .received_requests()
        .await
        .expect("mock server should record requests")
        .into_iter()
        .filter(|request| request.method == "POST" && request.url.path().ends_with("/responses"))
        .count();
    assert_eq!(response_requests, 2);

    Ok(())
}

#[tokio::test]
async fn turn_start_does_not_stream_apply_patch_change_updates_without_feature_v2() -> Result<()> {
    // TODO(anp): Materialize apply-patch workspaces in the selected remote environment.
    skip_if_remote!(
        Ok(()),
        "apply-patch workspace fixture is only materialized on the host"
    );
    skip_if_no_network!(Ok(()));

    let tmp = TempDir::new()?;
    let codex_home = tmp.path().join("codex_home");
    std::fs::create_dir(&codex_home)?;
    let workspace = tmp.path().join("workspace");
    std::fs::create_dir(&workspace)?;

    let call_id = "patch-call";
    let item_id = "fc-patch-call";
    let patch = "*** Begin Patch\n*** Add File: live.txt\n+live line\n*** End Patch\n";
    let patch_delta_1 = "*** Begin Patch\n*** Add File: live.txt\n+live";
    let patch_delta_2 = " line\n*** End Patch\n";
    let responses = vec![
        responses::sse(vec![
            responses::ev_response_created("resp-1"),
            serde_json::json!({
                "type": "response.output_item.added",
                "item": {
                    "type": "custom_tool_call",
                    "id": item_id,
                    "call_id": call_id,
                    "name": "apply_patch",
                    "input": "",
                    "status": "in_progress"
                }
            }),
            serde_json::json!({
                "type": "response.custom_tool_call_input.delta",
                "item_id": item_id,
                "call_id": call_id,
                "delta": patch_delta_1,
            }),
            serde_json::json!({
                "type": "response.custom_tool_call_input.delta",
                "item_id": item_id,
                "call_id": call_id,
                "delta": patch_delta_2,
            }),
            responses::ev_apply_patch_custom_tool_call(call_id, patch),
            responses::ev_completed("resp-1"),
        ]),
        create_final_assistant_message_sse_response("patch applied")?,
    ];
    let server = create_mock_responses_server_sequence(responses).await;
    ManagedWhisplyConfig::new().write(&codex_home)?;

    let mut mcp = app_test_support::managed_whisply_app_server_builder!(&server.uri())
        .with_codex_home(&codex_home)
        .build_initialized()
        .await?;

    let ThreadStartResponse { thread, .. } = mcp
        .start_thread(ThreadStartParams {
            model: Some("mock-model".to_string()),
            cwd: Some(workspace.to_string_lossy().into_owned()),
            ..Default::default()
        })
        .await?;

    let _: TurnStartResponse = mcp
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                thread_id: thread.id,
                client_user_message_id: None,
                input: vec![V2UserInput::Text {
                    text: "apply patch".into(),
                    text_elements: Vec::new(),
                }],
                cwd: Some(workspace),
                ..Default::default()
            },
        })
        .await?;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    assert!(
        !mcp.pending_notification_methods()
            .iter()
            .any(|method| method == "item/fileChange/patchUpdated")
    );

    Ok(())
}

#[tokio::test]
async fn turn_start_does_not_offer_apply_patch_when_catalog_does_not_advertise_it() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = create_mock_responses_server_sequence_unchecked(vec![
        create_final_assistant_message_sse_response("done")?,
    ])
    .await;
    let codex_home = TempDir::new()?;
    ManagedWhisplyConfig::new()
        .with_model("gpt-5.6-terra")
        .enable_feature(Feature::ApplyPatchStreamingEvents)
        .disable_feature(Feature::Plugins)
        .disable_feature(Feature::RemoteModels)
        .disable_feature(Feature::ShellSnapshot)
        .write(codex_home.path())?;

    let mut mcp = app_test_support::managed_whisply_app_server_builder!(&server.uri())
        .with_codex_home(codex_home.path())
        .build_initialized()
        .await?;

    let ThreadStartResponse { thread, .. } = mcp
        .start_thread(ThreadStartParams {
            model: Some("gpt-5.6-terra".to_string()),
            ..Default::default()
        })
        .await?;

    let _: TurnStartResponse = mcp
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                thread_id: thread.id,
                client_user_message_id: None,
                input: vec![V2UserInput::Text {
                    text: "confirm managed tool projection".into(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            },
        })
        .await?;

    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let requests = server
        .received_requests()
        .await
        .expect("managed response mock should receive a request");
    let request = requests.last().expect("expected a managed model request");
    let request_body = request
        .body_json::<Value>()
        .context("managed model request body should be JSON")?;
    let tools = request_body["tools"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        tools.iter().all(|tool| tool["name"] != "apply_patch"),
        "the signed managed catalog did not advertise apply_patch: {tools:#?}"
    );
    assert!(
        !mcp.pending_notification_methods()
            .iter()
            .any(|method| method == "item/fileChange/patchUpdated")
    );

    Ok(())
}

#[tokio::test]
async fn turn_start_spawns_agent_with_model_metadata_v2() -> Result<()> {
    skip_if_no_network!(Ok(()));

    const CHILD_PROMPT: &str = "child: do work";
    const PARENT_PROMPT: &str = "spawn a child and continue";
    const SPAWN_CALL_ID: &str = "spawn-call-1";
    const REQUESTED_MODEL: &str = "gpt-5.6-luna";
    const REQUESTED_REASONING_EFFORT: ReasoningEffort = ReasoningEffort::Low;

    let server = responses::start_mock_server().await;
    let spawn_args = serde_json::to_string(&json!({
        "message": CHILD_PROMPT,
        "task_name": "worker",
        "fork_turns": "none",
        "model": REQUESTED_MODEL,
        "reasoning_effort": REQUESTED_REASONING_EFFORT,
    }))?;
    let _parent_turn = responses::mount_sse_once_match(
        &server,
        |req: &wiremock::Request| body_contains(req, PARENT_PROMPT),
        responses::sse(vec![
            responses::ev_response_created("resp-turn1-1"),
            responses::ev_function_call_with_namespace(
                SPAWN_CALL_ID,
                MULTI_AGENT_V2_NAMESPACE,
                "spawn_agent",
                &spawn_args,
            ),
            responses::ev_completed("resp-turn1-1"),
        ]),
    )
    .await;
    let _child_turn = responses::mount_sse_once_match(
        &server,
        |req: &wiremock::Request| {
            body_contains(req, CHILD_PROMPT) && !body_contains(req, SPAWN_CALL_ID)
        },
        responses::sse(vec![
            responses::ev_response_created("resp-child-1"),
            responses::ev_assistant_message("msg-child-1", "child done"),
            responses::ev_completed("resp-child-1"),
        ]),
    )
    .await;
    let _parent_follow_up = responses::mount_sse_once_match(
        &server,
        |req: &wiremock::Request| body_contains(req, SPAWN_CALL_ID),
        responses::sse(vec![
            responses::ev_response_created("resp-turn1-2"),
            responses::ev_assistant_message("msg-turn1-2", "parent done"),
            responses::ev_completed("resp-turn1-2"),
        ]),
    )
    .await;

    let codex_home = TempDir::new()?;
    ManagedWhisplyConfig::new()
        .enable_feature(Feature::MultiAgentV2)
        .write(codex_home.path())?;

    let mut mcp = app_test_support::managed_whisply_app_server_builder!(&server.uri())
        .with_codex_home(codex_home.path())
        .build_initialized()
        .await?;

    let ThreadStartResponse { thread, .. } = mcp
        .start_thread(ThreadStartParams {
            model: Some("gpt-5.6-terra".to_string()),
            ..Default::default()
        })
        .await?;

    let turn: TurnStartResponse = mcp
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                thread_id: thread.id.clone(),
                client_user_message_id: None,
                input: vec![V2UserInput::Text {
                    text: PARENT_PROMPT.to_string(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            },
        })
        .await?;

    let (receiver_thread_id, agent_path) =
        wait_for_v2_spawned_subagent(&mut mcp, SPAWN_CALL_ID).await?;
    assert_eq!(agent_path, "/root/worker");
    assert_ne!(receiver_thread_id, thread.id);

    let turn_completed = timeout(DEFAULT_READ_TIMEOUT, async {
        loop {
            let turn_completed: TurnCompletedNotification =
                mcp.read_notification("turn/completed").await?;
            if turn_completed.thread_id == thread.id && turn_completed.turn.id == turn.turn.id {
                return Ok::<TurnCompletedNotification, anyhow::Error>(turn_completed);
            }
        }
    })
    .await??;
    assert_eq!(turn_completed.thread_id, thread.id);
    assert_eq!(turn_completed.turn.id, turn.turn.id);

    let child_request =
        response_request_payload_for_child(&server, CHILD_PROMPT, SPAWN_CALL_ID).await?;
    assert_eq!(child_request["model"], json!(REQUESTED_MODEL));
    assert_eq!(
        child_request["reasoning"]["effort"],
        json!(REQUESTED_REASONING_EFFORT)
    );

    // Reuse this live spawn setup to cover thread/delete's ThreadManager descendant path.
    let _: ThreadDeleteResponse = mcp
        .request(|request_id| ClientRequest::ThreadDelete {
            request_id,
            params: ThreadDeleteParams {
                thread_id: thread.id.clone(),
            },
        })
        .await?;

    let mut deleted_thread_ids = Vec::new();
    for _ in 0..2 {
        let deleted: ThreadDeletedNotification = timeout(
            DEFAULT_READ_TIMEOUT,
            mcp.read_notification("thread/deleted"),
        )
        .await??;
        deleted_thread_ids.push(deleted.thread_id);
    }
    assert_eq!(
        deleted_thread_ids,
        vec![receiver_thread_id, thread.id.clone()]
    );

    let ThreadLoadedListResponse { data, .. } = mcp
        .request(|request_id| ClientRequest::ThreadLoadedList {
            request_id,
            params: ThreadLoadedListParams::default(),
        })
        .await?;
    assert_eq!(data, Vec::<String>::new());

    Ok(())
}

#[tokio::test]
async fn direct_input_to_multi_agent_v2_subagent_is_rejected() -> Result<()> {
    const CHILD_PROMPT: &str = "child: do work";
    const PARENT_PROMPT: &str = "spawn a child and continue";
    const SPAWN_CALL_ID: &str = "spawn-call-direct-input-rejection";
    const ERROR_MESSAGE: &str =
        "direct app-server input is not allowed for multi-agent v2 sub-agents";

    let server = responses::start_mock_server().await;
    let spawn_args = serde_json::to_string(&json!({
        "message": CHILD_PROMPT,
        "task_name": "worker",
    }))?;
    let _parent_turn = responses::mount_sse_once_match(
        &server,
        |req: &wiremock::Request| body_contains(req, PARENT_PROMPT),
        responses::sse(vec![
            responses::ev_response_created("resp-parent-1"),
            responses::ev_function_call_with_namespace(
                SPAWN_CALL_ID,
                MULTI_AGENT_V2_NAMESPACE,
                "spawn_agent",
                &spawn_args,
            ),
            responses::ev_completed("resp-parent-1"),
        ]),
    )
    .await;
    let codex_home = TempDir::new()?;
    ManagedWhisplyConfig::new()
        .enable_feature(Feature::MultiAgentV2)
        .write(codex_home.path())?;
    write_models_cache(codex_home.path())?;

    let mut mcp = app_test_support::managed_whisply_app_server_builder!(&server.uri())
        .with_codex_home(codex_home.path())
        .build_initialized()
        .await?;

    let ThreadStartResponse { thread, .. } = mcp
        .start_thread(ThreadStartParams {
            model: Some("gpt-5.4".to_string()),
            ..Default::default()
        })
        .await?;

    let _: TurnStartResponse = mcp
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                thread_id: thread.id,
                input: vec![V2UserInput::Text {
                    text: PARENT_PROMPT.to_string(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            },
        })
        .await?;

    let child_thread_id = timeout(DEFAULT_READ_TIMEOUT, async {
        loop {
            let completed: ItemCompletedNotification =
                mcp.read_notification("item/completed").await?;
            if let ThreadItem::SubAgentActivity {
                id,
                kind: SubAgentActivityKind::Started,
                agent_thread_id,
                ..
            } = completed.item
                && id == SPAWN_CALL_ID
            {
                return Ok::<String, anyhow::Error>(agent_thread_id);
            }
        }
    })
    .await??;

    let listed: codex_app_server_protocol::ThreadListResponse = mcp
        .request(|request_id| ClientRequest::ThreadList {
            request_id,
            params: codex_app_server_protocol::ThreadListParams {
                cursor: None,
                limit: Some(10),
                sort_key: None,
                sort_direction: None,
                model_providers: None,
                source_kinds: Some(vec![
                    codex_app_server_protocol::ThreadSourceKind::SubAgentThreadSpawn,
                ]),
                archived: None,
                section_id: None,
                cwd: None,
                use_state_db_only: true,
                search_term: None,
                parent_thread_id: None,
                ancestor_thread_id: None,
            },
        })
        .await?;
    let listed_child = listed
        .data
        .iter()
        .find(|listed| listed.id == child_thread_id)
        .context("spawned child is missing from thread/list")?;
    assert!(matches!(
        &listed_child.source,
        codex_app_server_protocol::SessionSource::SubAgent(
            whisply_protocol::protocol::SubAgentSource::ThreadSpawn {
                agent_path: Some(_),
                ..
            }
        )
    ));
    assert_eq!(listed_child.can_accept_direct_input, Some(false));

    let direct_turn_req = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: child_thread_id.clone(),
            input: vec![V2UserInput::Text {
                text: "direct app-server turn".to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    let direct_turn_error: JSONRPCError = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(direct_turn_req)),
    )
    .await??;
    assert_eq!(direct_turn_error.error.code, INVALID_REQUEST_ERROR_CODE);
    assert_eq!(direct_turn_error.error.message, ERROR_MESSAGE);

    let direct_steer_req = mcp
        .send_turn_steer_request(TurnSteerParams {
            thread_id: child_thread_id,
            client_user_message_id: None,
            input: vec![V2UserInput::Text {
                text: "direct app-server steer".to_string(),
                text_elements: Vec::new(),
            }],
            responsesapi_client_metadata: None,
            additional_context: None,
            expected_turn_id: "any-active-turn".to_string(),
        })
        .await?;
    let direct_steer_error: JSONRPCError = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(direct_steer_req)),
    )
    .await??;
    assert_eq!(direct_steer_error.error.code, INVALID_REQUEST_ERROR_CODE);
    assert_eq!(direct_steer_error.error.message, ERROR_MESSAGE);

    Ok(())
}

#[tokio::test]
async fn turn_start_spawns_agent_with_effective_role_model_metadata_v2() -> Result<()> {
    skip_if_no_network!(Ok(()));

    const CHILD_PROMPT: &str = "child: do work";
    const PARENT_PROMPT: &str = "spawn a child and continue";
    const SPAWN_CALL_ID: &str = "spawn-call-1";
    const REQUESTED_MODEL: &str = "gpt-5.6-luna";
    const REQUESTED_REASONING_EFFORT: ReasoningEffort = ReasoningEffort::Low;
    const ROLE_MODEL: &str = "gpt-5.6-sol";
    const ROLE_REASONING_EFFORT: ReasoningEffort = ReasoningEffort::High;

    let server = responses::start_mock_server().await;
    let spawn_args = serde_json::to_string(&json!({
        "message": CHILD_PROMPT,
        "task_name": "worker",
        "fork_turns": "none",
        "agent_type": "custom",
        "model": REQUESTED_MODEL,
        "reasoning_effort": REQUESTED_REASONING_EFFORT,
    }))?;
    let _parent_turn = responses::mount_sse_once_match(
        &server,
        |req: &wiremock::Request| body_contains(req, PARENT_PROMPT),
        responses::sse(vec![
            responses::ev_response_created("resp-turn1-1"),
            responses::ev_function_call_with_namespace(
                SPAWN_CALL_ID,
                MULTI_AGENT_V2_NAMESPACE,
                "spawn_agent",
                &spawn_args,
            ),
            responses::ev_completed("resp-turn1-1"),
        ]),
    )
    .await;
    let _child_turn = responses::mount_sse_once_match(
        &server,
        |req: &wiremock::Request| {
            body_contains(req, CHILD_PROMPT) && !body_contains(req, SPAWN_CALL_ID)
        },
        responses::sse(vec![
            responses::ev_response_created("resp-child-1"),
            responses::ev_assistant_message("msg-child-1", "child done"),
            responses::ev_completed("resp-child-1"),
        ]),
    )
    .await;
    let _parent_follow_up = responses::mount_sse_once_match(
        &server,
        |req: &wiremock::Request| body_contains(req, SPAWN_CALL_ID),
        responses::sse(vec![
            responses::ev_response_created("resp-turn1-2"),
            responses::ev_assistant_message("msg-turn1-2", "parent done"),
            responses::ev_completed("resp-turn1-2"),
        ]),
    )
    .await;

    let codex_home = TempDir::new()?;
    ManagedWhisplyConfig::new()
        .enable_feature(Feature::MultiAgentV2)
        .write(codex_home.path())?;
    std::fs::write(
        codex_home.path().join("custom-role.toml"),
        format!("model = \"{ROLE_MODEL}\"\nmodel_reasoning_effort = \"{ROLE_REASONING_EFFORT}\"\n",),
    )?;
    let config_path = codex_home.path().join("config.toml");
    let base_config = std::fs::read_to_string(&config_path)?;
    std::fs::write(
        &config_path,
        format!(
            r#"{base_config}

[agents.custom]
description = "Custom role"
config_file = "./custom-role.toml"
"#
        ),
    )?;

    let mut mcp = app_test_support::managed_whisply_app_server_builder!(&server.uri())
        .with_codex_home(codex_home.path())
        .build_initialized()
        .await?;

    let ThreadStartResponse { thread, .. } = mcp
        .start_thread(ThreadStartParams {
            model: Some("gpt-5.6-terra".to_string()),
            ..Default::default()
        })
        .await?;

    let turn: TurnStartResponse = mcp
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                thread_id: thread.id.clone(),
                client_user_message_id: None,
                input: vec![V2UserInput::Text {
                    text: PARENT_PROMPT.to_string(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            },
        })
        .await?;

    let (receiver_thread_id, agent_path) =
        wait_for_v2_spawned_subagent(&mut mcp, SPAWN_CALL_ID).await?;
    assert_eq!(agent_path, "/root/worker");
    assert_ne!(receiver_thread_id, thread.id);

    let turn_completed = timeout(DEFAULT_READ_TIMEOUT, async {
        loop {
            let turn_completed: TurnCompletedNotification =
                mcp.read_notification("turn/completed").await?;
            if turn_completed.thread_id == thread.id && turn_completed.turn.id == turn.turn.id {
                return Ok::<TurnCompletedNotification, anyhow::Error>(turn_completed);
            }
        }
    })
    .await??;
    assert_eq!(turn_completed.thread_id, thread.id);

    let child_request =
        response_request_payload_for_child(&server, CHILD_PROMPT, SPAWN_CALL_ID).await?;
    assert_eq!(child_request["model"], json!(ROLE_MODEL));
    assert_eq!(
        child_request["reasoning"]["effort"],
        json!(ROLE_REASONING_EFFORT)
    );

    Ok(())
}

#[tokio::test]
async fn turn_start_file_change_approval_accept_for_session_persists_v2() -> Result<()> {
    // TODO(anp): Materialize apply-patch workspaces in the selected remote environment.
    skip_if_remote!(
        Ok(()),
        "apply-patch workspace fixture is only materialized on the host"
    );
    skip_if_no_network!(Ok(()));

    let tmp = TempDir::new()?;
    let codex_home = tmp.path().join("codex_home");
    std::fs::create_dir(&codex_home)?;
    let workspace = tmp.path().join("workspace");
    std::fs::create_dir(&workspace)?;

    let patch_1 = r#"*** Begin Patch
*** Add File: README.md
+new line
*** End Patch
"#;
    let patch_2 = r#"*** Begin Patch
*** Update File: README.md
@@
-new line
+updated line
*** End Patch
"#;

    let responses = vec![
        create_apply_patch_sse_response(patch_1, "patch-call-1")?,
        create_final_assistant_message_sse_response("patch 1 applied")?,
        create_apply_patch_sse_response(patch_2, "patch-call-2")?,
        create_final_assistant_message_sse_response("patch 2 applied")?,
    ];
    let server = create_mock_responses_server_sequence(responses).await;
    ManagedWhisplyConfig::new()
        .with_approval_policy("untrusted")
        .write(&codex_home)?;

    let mut mcp = app_test_support::managed_whisply_app_server_builder!(&server.uri())
        .with_codex_home(&codex_home)
        .build_initialized()
        .await?;

    let ThreadStartResponse { thread, .. } = mcp
        .start_thread(ThreadStartParams {
            model: Some("mock-model".to_string()),
            cwd: Some(workspace.to_string_lossy().into_owned()),
            ..Default::default()
        })
        .await?;

    // First turn: expect FileChangeRequestApproval, respond with AcceptForSession, and verify the file exists.
    let TurnStartResponse { turn: turn_1 } = mcp
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                thread_id: thread.id.clone(),
                client_user_message_id: None,
                input: vec![V2UserInput::Text {
                    text: "apply patch 1".into(),
                    text_elements: Vec::new(),
                }],
                cwd: Some(workspace.clone()),
                ..Default::default()
            },
        })
        .await?;

    let started_file_change_1 = timeout(DEFAULT_READ_TIMEOUT, async {
        loop {
            let started: ItemStartedNotification = mcp.read_notification("item/started").await?;
            if let ThreadItem::FileChange { .. } = started.item {
                return Ok::<ThreadItem, anyhow::Error>(started.item);
            }
        }
    })
    .await??;
    let ThreadItem::FileChange { id, status, .. } = started_file_change_1 else {
        unreachable!("loop ensures we break on file change items");
    };
    assert_eq!(id, "patch-call-1");
    assert_eq!(status, PatchApplyStatus::InProgress);

    let server_req = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_request_message(),
    )
    .await??;
    let ServerRequest::FileChangeRequestApproval { request_id, params } = server_req else {
        panic!("expected FileChangeRequestApproval request")
    };
    assert_eq!(params.item_id, "patch-call-1");
    assert_eq!(params.thread_id, thread.id);
    assert_eq!(params.turn_id, turn_1.id);

    let resolved_request_id = request_id.clone();
    mcp.send_response(
        request_id,
        serde_json::to_value(FileChangeRequestApprovalResponse {
            decision: FileChangeApprovalDecision::AcceptForSession,
        })?,
    )
    .await?;

    let mut approval_resolved = false;
    let mut patch_completed = false;
    while !approval_resolved || !patch_completed {
        let message = timeout(DEFAULT_READ_TIMEOUT, mcp.read_next_message()).await??;
        let JSONRPCMessage::Notification(notification) = message else {
            continue;
        };
        match notification.method.as_str() {
            "serverRequest/resolved" => {
                let resolved: ServerRequestResolvedNotification = serde_json::from_value(
                    notification.params.expect("serverRequest/resolved params"),
                )?;
                if resolved.request_id == resolved_request_id {
                    assert_eq!(resolved.thread_id, thread.id);
                    approval_resolved = true;
                }
            }
            "item/completed" => {
                let completed: ItemCompletedNotification =
                    serde_json::from_value(notification.params.expect("item/completed params"))?;
                if matches!(completed.item, ThreadItem::FileChange { ref id, .. } if id == "patch-call-1")
                {
                    assert_eq!(completed.thread_id, thread.id);
                    assert_eq!(completed.turn_id, turn_1.id);
                    patch_completed = true;
                }
            }
            _ => {}
        }
    }
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let readme_path = workspace.join("README.md");
    assert_eq!(std::fs::read_to_string(&readme_path)?, "new line\n");

    // Second turn: apply a patch to the same file. Approval should be skipped due to AcceptForSession.
    let TurnStartResponse { turn: turn_2 } = mcp
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                thread_id: thread.id.clone(),
                client_user_message_id: None,
                input: vec![V2UserInput::Text {
                    text: "apply patch 2".into(),
                    text_elements: Vec::new(),
                }],
                cwd: Some(workspace.clone()),
                ..Default::default()
            },
        })
        .await?;

    let started_file_change_2 = timeout(DEFAULT_READ_TIMEOUT, async {
        loop {
            let started: ItemStartedNotification = mcp.read_notification("item/started").await?;
            if let ThreadItem::FileChange { .. } = started.item {
                return Ok::<ThreadItem, anyhow::Error>(started.item);
            }
        }
    })
    .await??;
    let ThreadItem::FileChange { id, status, .. } = started_file_change_2 else {
        unreachable!("loop ensures we break on file change items");
    };
    assert_eq!(id, "patch-call-2");
    assert_eq!(status, PatchApplyStatus::InProgress);

    let completed_file_change = timeout(DEFAULT_READ_TIMEOUT, async {
        loop {
            match mcp.read_next_message().await? {
                JSONRPCMessage::Request(request) => {
                    anyhow::bail!("unexpected approval request for session-approved patch: {request:?}");
                }
                JSONRPCMessage::Notification(notification)
                    if notification.method == "item/completed" =>
                {
                    let completed: ItemCompletedNotification = serde_json::from_value(
                        notification.params.expect("item/completed params"),
                    )?;
                    if matches!(completed.item, ThreadItem::FileChange { ref id, .. } if id == "patch-call-2")
                    {
                        return Ok::<ItemCompletedNotification, anyhow::Error>(completed);
                    }
                }
                _ => {}
            }
        }
    })
    .await??;
    assert_eq!(completed_file_change.thread_id, thread.id);
    assert_eq!(completed_file_change.turn_id, turn_2.id);
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    assert_eq!(std::fs::read_to_string(readme_path)?, "updated line\n");
    let status = timeout(DEFAULT_READ_TIMEOUT, mcp.shutdown_gracefully()).await??;
    anyhow::ensure!(
        status.success(),
        "app-server exited unsuccessfully: {status}"
    );

    Ok(())
}

#[tokio::test]
async fn turn_start_file_change_approval_decline_v2() -> Result<()> {
    run_turn_start_file_change_approval_rejection_v2(
        serde_json::to_value(FileChangeRequestApprovalResponse {
            decision: FileChangeApprovalDecision::Decline,
        })?,
        "rejected by user",
    )
    .await
}

#[tokio::test]
async fn turn_start_file_change_approval_invalid_response_v2() -> Result<()> {
    run_turn_start_file_change_approval_rejection_v2(
        json!({ "unexpected": "response" }),
        "approval request failed",
    )
    .await
}

async fn run_turn_start_file_change_approval_rejection_v2(
    approval_response: Value,
    expected_rejection: &str,
) -> Result<()> {
    // TODO(anp): Materialize apply-patch workspaces in the selected remote environment.
    skip_if_remote!(
        Ok(()),
        "apply-patch workspace fixture is only materialized on the host"
    );
    skip_if_no_network!(Ok(()));

    let tmp = TempDir::new()?;
    let codex_home = tmp.path().join("codex_home");
    std::fs::create_dir(&codex_home)?;
    let workspace = tmp.path().join("workspace");
    std::fs::create_dir(&workspace)?;

    let patch = r#"*** Begin Patch
*** Add File: README.md
+new line
*** End Patch
"#;
    let responses = vec![
        create_apply_patch_sse_response(patch, "patch-call")?,
        create_final_assistant_message_sse_response("patch declined")?,
    ];
    let server = create_mock_responses_server_sequence(responses).await;
    ManagedWhisplyConfig::new()
        .with_approval_policy("untrusted")
        .write(&codex_home)?;

    let mut mcp = app_test_support::managed_whisply_app_server_builder!(&server.uri())
        .with_codex_home(&codex_home)
        .build_initialized()
        .await?;

    let ThreadStartResponse { thread, .. } = mcp
        .start_thread(ThreadStartParams {
            model: Some("mock-model".to_string()),
            cwd: Some(workspace.to_string_lossy().into_owned()),
            ..Default::default()
        })
        .await?;

    let TurnStartResponse { turn } = mcp
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                thread_id: thread.id.clone(),
                client_user_message_id: None,
                input: vec![V2UserInput::Text {
                    text: "apply patch".into(),
                    text_elements: Vec::new(),
                }],
                cwd: Some(workspace.clone()),
                ..Default::default()
            },
        })
        .await?;

    let started_file_change = timeout(DEFAULT_READ_TIMEOUT, async {
        loop {
            let started: ItemStartedNotification = mcp.read_notification("item/started").await?;
            if let ThreadItem::FileChange { .. } = started.item {
                return Ok::<ThreadItem, anyhow::Error>(started.item);
            }
        }
    })
    .await??;
    let ThreadItem::FileChange {
        ref id,
        status,
        ref changes,
    } = started_file_change
    else {
        unreachable!("loop ensures we break on file change items");
    };
    assert_eq!(id, "patch-call");
    assert_eq!(status, PatchApplyStatus::InProgress);
    let started_changes = changes.clone();

    let server_req = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_request_message(),
    )
    .await??;
    let ServerRequest::FileChangeRequestApproval { request_id, params } = server_req else {
        panic!("expected FileChangeRequestApproval request")
    };
    assert_eq!(params.item_id, "patch-call");
    assert_eq!(params.thread_id, thread.id);
    assert_eq!(params.turn_id, turn.id);
    let expected_readme_path = workspace.join("README.md");
    let expected_readme_path_str = expected_readme_path.to_string_lossy().into_owned();
    pretty_assertions::assert_eq!(
        started_changes,
        vec![codex_app_server_protocol::FileUpdateChange {
            path: expected_readme_path_str.clone(),
            kind: PatchChangeKind::Add,
            diff: "new line\n".to_string(),
        }]
    );

    mcp.send_response(request_id, approval_response).await?;

    let completed_file_change = timeout(DEFAULT_READ_TIMEOUT, async {
        loop {
            let completed: ItemCompletedNotification =
                mcp.read_notification("item/completed").await?;
            if let ThreadItem::FileChange { .. } = completed.item {
                return Ok::<ThreadItem, anyhow::Error>(completed.item);
            }
        }
    })
    .await??;
    let ThreadItem::FileChange { ref id, status, .. } = completed_file_change else {
        unreachable!("loop ensures we break on file change items");
    };
    assert_eq!(id, "patch-call");
    assert_eq!(status, PatchApplyStatus::Declined);

    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let requests = server
        .received_requests()
        .await
        .context("failed to fetch received requests")?;
    assert!(
        requests.iter().any(|request| {
            request.url.path().ends_with("/responses") && body_contains(request, expected_rejection)
        }),
        "model request should include approval rejection: {expected_rejection}"
    );

    assert!(
        !expected_readme_path.exists(),
        "declined patch should not be applied"
    );

    Ok(())
}

#[tokio::test]
#[cfg_attr(windows, ignore = "process id reporting differs on Windows")]
async fn command_execution_notifications_include_process_id() -> Result<()> {
    // TODO(anp): Add target-Windows process-id expectations for remote executors.
    skip_if_wine_exec!(
        Ok(()),
        "process id reporting differs for a Windows executor"
    );
    skip_if_no_network!(Ok(()));

    let responses = vec![
        create_exec_command_sse_response("uexec-1")?,
        create_final_assistant_message_sse_response("done")?,
    ];
    let server = create_mock_responses_server_sequence(responses).await;
    let codex_home = TempDir::new()?;
    ManagedWhisplyConfig::new()
        .with_sandbox_mode("danger-full-access")
        .enable_feature(Feature::UnifiedExec)
        .write(codex_home.path())?;

    let mut mcp = app_test_support::managed_whisply_app_server_builder!(&server.uri())
        .with_codex_home(codex_home.path())
        .build_initialized()
        .await?;

    let ThreadStartResponse { thread, .. } = mcp
        .start_thread(ThreadStartParams {
            model: Some("mock-model".to_string()),
            ..Default::default()
        })
        .await?;

    let TurnStartResponse { turn: _turn } = mcp
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                thread_id: thread.id.clone(),
                client_user_message_id: None,
                input: vec![V2UserInput::Text {
                    text: "run a command".to_string(),
                    text_elements: Vec::new(),
                }],
                sandbox_policy: Some(codex_app_server_protocol::SandboxPolicy::DangerFullAccess),
                ..Default::default()
            },
        })
        .await?;

    let started_command = timeout(DEFAULT_READ_TIMEOUT, async {
        loop {
            let started: ItemStartedNotification = mcp.read_notification("item/started").await?;
            if let ThreadItem::CommandExecution { .. } = started.item {
                return Ok::<ThreadItem, anyhow::Error>(started.item);
            }
        }
    })
    .await??;
    let ThreadItem::CommandExecution {
        id,
        process_id: started_process_id,
        status,
        ..
    } = started_command
    else {
        unreachable!("loop ensures we break on command execution items");
    };
    assert_eq!(id, "uexec-1");
    assert_eq!(status, CommandExecutionStatus::InProgress);
    let started_process_id = started_process_id.expect("process id should be present");

    let completed_command = timeout(DEFAULT_READ_TIMEOUT, async {
        loop {
            let completed: ItemCompletedNotification =
                mcp.read_notification("item/completed").await?;
            if let ThreadItem::CommandExecution { .. } = completed.item {
                return Ok::<ThreadItem, anyhow::Error>(completed.item);
            }
        }
    })
    .await??;
    let ThreadItem::CommandExecution {
        id: completed_id,
        process_id: completed_process_id,
        status: completed_status,
        exit_code,
        ..
    } = completed_command
    else {
        unreachable!("loop ensures we break on command execution items");
    };
    assert_eq!(completed_id, "uexec-1");
    assert!(
        matches!(
            completed_status,
            CommandExecutionStatus::Completed | CommandExecutionStatus::Failed
        ),
        "unexpected command execution status: {completed_status:?}"
    );
    if completed_status == CommandExecutionStatus::Completed {
        assert_eq!(exit_code, Some(0));
    } else {
        assert!(exit_code.is_some(), "expected exit_code for failed command");
    }
    assert_eq!(
        completed_process_id.as_deref(),
        Some(started_process_id.as_str())
    );

    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    Ok(())
}

#[cfg_attr(windows, ignore = "plugin attribution fixture is Unix-only")]
#[tokio::test]
async fn command_execution_notifications_include_trusted_plugin_id() -> Result<()> {
    skip_if_no_network!(Ok(()));
    skip_if_wine_exec!(Ok(()), "plugin attribution fixture is Unix-only");

    let codex_home = TempDir::new()?;
    let curated_sha = "0123456789abcdef0123456789abcdef01234567";
    let plugin_root = codex_home
        .path()
        .join("plugins/cache/openai-curated/google-calendar/01234567");
    let script_path = plugin_root.join("scripts/run.sh");
    let synced_root = codex_home.path().join(".tmp/plugins");
    for path in [
        plugin_root.join(".codex-plugin"),
        script_path
            .parent()
            .expect("script path should have parent")
            .to_path_buf(),
        synced_root.join(".agents/plugins"),
    ] {
        std::fs::create_dir_all(path)?;
    }
    std::fs::write(
        plugin_root.join(".codex-plugin/plugin.json"),
        r#"{"name":"google-calendar","version":"0.1.0"}"#,
    )?;
    std::fs::write(&script_path, "echo hi\n")?;
    std::fs::write(
        codex_home.path().join(".tmp/plugins.sha"),
        format!("{curated_sha}\n"),
    )?;
    std::fs::write(
        synced_root.join(".agents/plugins/marketplace.json"),
        r#"{
  "name": "openai-curated",
  "plugins": [{
    "name": "google-calendar",
    "source": {"source": "local", "path": "./plugins/google-calendar"}
  }]
}"#,
    )?;
    let responses = vec![
        create_shell_command_sse_response(
            vec![
                "/bin/sh".to_string(),
                script_path.to_string_lossy().into_owned(),
            ],
            /*workdir*/ None,
            /*timeout_ms*/ None,
            "plugin-command",
        )?,
        create_final_assistant_message_sse_response("done")?,
    ];
    let server = create_mock_responses_server_sequence(responses).await;
    ManagedWhisplyConfig::new()
        .with_approval_policy("untrusted")
        .with_sandbox_mode("danger-full-access")
        .enable_feature(Feature::Plugins)
        .disable_feature(Feature::RemotePlugin)
        .with_additional_config("[plugins.\"google-calendar@openai-curated\"]\nenabled = true")
        .write(codex_home.path())?;

    let mut mcp = app_test_support::managed_whisply_app_server_builder!(&server.uri())
        .with_codex_home(codex_home.path())
        .build_initialized()
        .await?;
    let ThreadStartResponse { thread, .. } = mcp
        .start_thread(ThreadStartParams {
            model: Some("mock-model".to_string()),
            ..Default::default()
        })
        .await?;
    let _: TurnStartResponse = mcp
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                thread_id: thread.id,
                input: vec![V2UserInput::Text {
                    text: "run a plugin command".to_string(),
                    text_elements: Vec::new(),
                }],
                sandbox_policy: Some(codex_app_server_protocol::SandboxPolicy::DangerFullAccess),
                ..Default::default()
            },
        })
        .await?;

    for method in ["item/started", "item/completed"] {
        let status = timeout(DEFAULT_READ_TIMEOUT, async {
            loop {
                let notification = mcp.read_stream_until_notification_message(method).await?;
                let params = notification.params.expect("item notification params");
                let item_json = params.get("item").expect("item notification item").clone();
                let item = serde_json::from_value::<ThreadItem>(item_json.clone())?;
                if let ThreadItem::CommandExecution { status, .. } = item {
                    let emitted_script_path = item_json
                        .get("scriptPath")
                        .and_then(serde_json::Value::as_str)
                        .expect("command execution item should include scriptPath");
                    assert_eq!(
                        (item_json["pluginId"].as_str(), emitted_script_path),
                        (Some("google-calendar@openai-curated"), "scripts/run.sh")
                    );
                    assert!(
                        !emitted_script_path.contains(script_path.to_string_lossy().as_ref()),
                        "scriptPath must not serialize the absolute fixture path"
                    );
                    assert!(
                        !emitted_script_path.contains("plugins/cache"),
                        "scriptPath must not serialize a plugin cache path"
                    );
                    return Ok::<CommandExecutionStatus, anyhow::Error>(status);
                }
            }
        })
        .await??;
        if method == "item/started" {
            let server_req = timeout(
                DEFAULT_READ_TIMEOUT,
                mcp.read_stream_until_request_message(),
            )
            .await??;
            let ServerRequest::CommandExecutionRequestApproval { request_id, params } = server_req
            else {
                panic!("expected CommandExecutionRequestApproval request");
            };
            assert_eq!(params.item_id, "plugin-command");
            mcp.send_response(
                request_id,
                serde_json::to_value(CommandExecutionRequestApprovalResponse {
                    decision: CommandExecutionApprovalDecision::Decline,
                })?,
            )
            .await?;
        } else {
            assert_eq!(status, CommandExecutionStatus::Declined);
        }
    }

    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    Ok(())
}

#[tokio::test]
async fn turn_start_with_elevated_override_does_not_persist_project_trust() -> Result<()> {
    let responses = vec![create_final_assistant_message_sse_response("Done")?];
    let server = create_mock_responses_server_sequence_unchecked(responses).await;

    let codex_home = TempDir::new()?;
    ManagedWhisplyConfig::new()
        .enable_feature(Feature::Personality)
        .write(codex_home.path())?;

    let workspace = TempDir::new()?;

    let mut mcp = app_test_support::managed_whisply_app_server_builder!(&server.uri())
        .with_codex_home(codex_home.path())
        .build_initialized()
        .await?;

    let ThreadStartResponse { thread, .. } = mcp
        .start_thread(ThreadStartParams {
            cwd: Some(workspace.path().display().to_string()),
            ..Default::default()
        })
        .await?;

    let _: TurnStartResponse = mcp
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                thread_id: thread.id,
                cwd: Some(workspace.path().to_path_buf()),
                sandbox_policy: Some(codex_app_server_protocol::SandboxPolicy::DangerFullAccess),
                input: vec![V2UserInput::Text {
                    text: "Hello".to_string(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            },
        })
        .await?;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let config_toml = std::fs::read_to_string(codex_home.path().join("config.toml"))?;
    assert!(!config_toml.contains("trust_level = \"trusted\""));
    assert!(!config_toml.contains(&workspace.path().display().to_string()));

    Ok(())
}

fn write_test_skill(codex_home: &Path, name: &str) -> std::io::Result<()> {
    let skill_dir = codex_home.join("skills").join(name);
    std::fs::create_dir_all(&skill_dir)?;
    std::fs::write(
        skill_dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {name} description\n---\n\n# Body\n"),
    )
}

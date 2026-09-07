#![cfg(target_os = "macos")]

use anyhow::Context;
use anyhow::Result;
use app_test_support::ManagedWhisplyConfig;
use app_test_support::ManagedWhisplyGatewayFixture;
use app_test_support::TestAppServer;
use codex_app_server_protocol::ClientInfo;
use codex_app_server_protocol::InitializeCapabilities;
use codex_app_server_protocol::JSONRPCMessage;
use codex_app_server_protocol::ThreadInjectItemsParams;
use codex_app_server_protocol::ThreadInjectItemsResponse;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::UserInput as V2UserInput;
use core_test_support::responses;
use core_test_support::responses::strip_response_item_id;
use core_test_support::responses::strip_response_item_ids_from_json;
use serde_json::Value;
use tempfile::TempDir;
use tokio::time::timeout;
use whisply_core::RolloutRecorder;
use whisply_protocol::models::ContentItem;
use whisply_protocol::models::ResponseItem;
use whisply_protocol::protocol::InitialHistory;
use whisply_protocol::protocol::RolloutItem;

const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

async fn initialize_stable(mcp: &mut TestAppServer) -> Result<()> {
    let initialized = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.initialize_with_capabilities(
            ClientInfo {
                name: "whisply-history-stable-test".to_owned(),
                title: None,
                version: "0.1.0".to_owned(),
            },
            Some(InitializeCapabilities {
                experimental_api: false,
                ..Default::default()
            }),
        ),
    )
    .await??;
    anyhow::ensure!(
        matches!(initialized, JSONRPCMessage::Response(_)),
        "stable history initialization failed"
    );
    Ok(())
}

#[tokio::test]
async fn thread_inject_items_adds_raw_response_items_to_thread_history() -> Result<()> {
    let server = responses::start_mock_server().await;
    let body = responses::sse(vec![
        responses::ev_response_created("resp-1"),
        responses::ev_assistant_message("msg-1", "Done"),
        responses::ev_completed("resp-1"),
    ]);
    let response_mock = responses::mount_sse_once(&server, body).await;

    let codex_home = TempDir::new()?;
    ManagedWhisplyConfig::new().write(codex_home.path())?;

    let managed_gateway = ManagedWhisplyGatewayFixture::new(&server.uri())?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .with_managed_whisply_gateway(managed_gateway)
        .build()
        .await?;
    initialize_stable(&mut mcp).await?;

    // The automatic-environment helper adds an experimental request field;
    // the shipping Mac sends ordinary thread/start with that field omitted.
    let thread_req = mcp
        .send_thread_start_request(ThreadStartParams {
            model: Some("mock-model".to_string()),
            ..Default::default()
        })
        .await?;
    let ThreadStartResponse { thread, .. } =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(thread_req)).await??;

    let injected_text = "Injected assistant context";
    let injected_item = ResponseItem::Message {
        id: None,
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: injected_text.to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    };

    let inject_req = mcp
        .send_thread_inject_items_request(ThreadInjectItemsParams {
            whisply_history_recovery: None,
            thread_id: thread.id.clone(),
            items: vec![serde_json::to_value(&injected_item)?],
        })
        .await?;
    let _response: ThreadInjectItemsResponse =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(inject_req)).await??;

    let rollout_path = thread.path.as_ref().context("thread path missing")?;
    let history = RolloutRecorder::get_rollout_history(rollout_path).await?;
    let InitialHistory::Resumed(resumed_history) = history else {
        panic!("expected resumed rollout history");
    };
    assert!(
        resumed_history
            .history
            .iter()
            .any(|item| matches!(item, RolloutItem::ResponseItem(response_item) if strip_response_item_id(responses::strip_metadata(response_item.clone())) == injected_item)),
        "injected item should be persisted in rollout history"
    );

    let turn_req = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id.clone(),
            client_user_message_id: None,
            input: vec![V2UserInput::Text {
                text: "Hello".to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    let _: TurnStartResponse = timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(turn_req)).await??;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let injected_value = serde_json::to_value(&injected_item)?;
    let model_input: Vec<Value> = response_mock
        .single_request()
        .input()
        .into_iter()
        .map(strip_response_item_ids_from_json)
        .collect();
    let environment_context_index =
        response_item_text_position(&model_input, "<environment_context>")
            .expect("environment context should be injected before the first user turn");
    let injected_index = model_input
        .iter()
        .position(|item| item == &injected_value)
        .expect("injected item should be sent in the next model request");
    let user_prompt_index = response_item_text_position(&model_input, "Hello")
        .expect("user prompt should be sent in the next model request");
    assert!(
        environment_context_index < injected_index,
        "standard initial context should be sent before injected items"
    );
    assert!(
        injected_index < user_prompt_index,
        "injected items should be sent before the user prompt"
    );

    Ok(())
}

#[tokio::test]
async fn thread_inject_items_adds_raw_response_items_after_a_turn() -> Result<()> {
    let server = responses::start_mock_server().await;
    let first_body = responses::sse(vec![
        responses::ev_response_created("resp-1"),
        responses::ev_assistant_message("msg-1", "First done"),
        responses::ev_completed("resp-1"),
    ]);
    let second_body = responses::sse(vec![
        responses::ev_response_created("resp-2"),
        responses::ev_assistant_message("msg-2", "Second done"),
        responses::ev_completed("resp-2"),
    ]);
    let response_mock = responses::mount_sse_sequence(&server, vec![first_body, second_body]).await;

    let codex_home = TempDir::new()?;
    ManagedWhisplyConfig::new().write(codex_home.path())?;

    let managed_gateway = ManagedWhisplyGatewayFixture::new(&server.uri())?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .with_managed_whisply_gateway(managed_gateway)
        .build()
        .await?;
    initialize_stable(&mut mcp).await?;

    let thread_req = mcp
        .send_thread_start_request(ThreadStartParams {
            model: Some("mock-model".to_string()),
            ..Default::default()
        })
        .await?;
    let ThreadStartResponse { thread, .. } =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(thread_req)).await??;

    let first_turn_req = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id.clone(),
            client_user_message_id: None,
            input: vec![V2UserInput::Text {
                text: "First turn".to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    let _: TurnStartResponse =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(first_turn_req)).await??;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let injected_item = ResponseItem::Message {
        id: None,
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: "Injected after first turn".to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    };
    let injected_value = serde_json::to_value(&injected_item)?;

    let inject_req = mcp
        .send_thread_inject_items_request(ThreadInjectItemsParams {
            whisply_history_recovery: None,
            thread_id: thread.id.clone(),
            items: vec![injected_value.clone()],
        })
        .await?;
    let _response: ThreadInjectItemsResponse =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(inject_req)).await??;

    let second_turn_req = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id.clone(),
            client_user_message_id: None,
            input: vec![V2UserInput::Text {
                text: "Second turn".to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    let _: TurnStartResponse =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(second_turn_req)).await??;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let requests = response_mock.requests();
    assert_eq!(requests.len(), 2);
    assert!(
        !requests[0]
            .input()
            .into_iter()
            .map(strip_response_item_ids_from_json)
            .any(|item| item == injected_value),
        "injected item should not be sent before it is injected"
    );
    assert!(
        requests[1]
            .input()
            .into_iter()
            .map(strip_response_item_ids_from_json)
            .any(|item| item == injected_value),
        "injected item should be sent after being injected into existing history"
    );

    Ok(())
}

fn response_item_text_position(items: &[Value], needle: &str) -> Option<usize> {
    items.iter().position(|item| {
        item.get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .any(|content| {
                content
                    .get("text")
                    .and_then(Value::as_str)
                    .is_some_and(|text| text.contains(needle))
            })
    })
}

#[tokio::test]
async fn thread_inject_items_restores_whisply_history_with_stable_typed_receipts() -> Result<()> {
    use base64::Engine;
    use codex_app_server_protocol::RequestId;
    use sha2::{Digest, Sha256};
    use whisply_protocol::protocol::{WhisplyHistoryRecoveryFrame, WhisplyHistoryRecoveryRecord};
    let server = responses::start_mock_server().await;
    let response_mock = responses::mount_sse_once(
        &server,
        responses::sse(vec![
            responses::ev_assistant_message("restored-answer", "Recovered"),
            responses::ev_completed("restored-response"),
        ]),
    )
    .await;
    let home = TempDir::new()?;
    ManagedWhisplyConfig::new().write(home.path())?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(home.path())
        .with_managed_whisply_gateway(ManagedWhisplyGatewayFixture::new(&server.uri())?)
        .build()
        .await?;
    initialize_stable(&mut mcp).await?;
    let request = mcp
        .send_thread_start_request(ThreadStartParams {
            model: Some("mock-model".to_owned()),
            ..Default::default()
        })
        .await?;
    let ThreadStartResponse { thread, .. } =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(request)).await??;
    let old = "original saved history ".repeat(700);
    let mut bytes =
        serde_json::to_vec(&serde_json::json!({"speaker":"user","kind":"text","value":old}))?;
    bytes.push(b'\n');
    let frame = WhisplyHistoryRecoveryFrame {
        archive_id: "a".repeat(64),
        offset: 0,
        data_base64: base64::engine::general_purpose::STANDARD.encode(&bytes),
        final_byte_count: Some(bytes.len() as u64),
        final_sha256: Some(format!("{:x}", Sha256::digest(&bytes))),
    };
    for _ in 0..2 {
        let request = mcp
            .send_thread_inject_items_request(ThreadInjectItemsParams {
                thread_id: thread.id.clone(),
                items: Vec::new(),
                whisply_history_recovery: Some(frame.clone()),
            })
            .await?;
        let receipt: ThreadInjectItemsResponse =
            timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(request)).await??;
        let receipt = receipt
            .whisply_history_recovery
            .context("typed recovery receipt")?;
        assert!(receipt.committed && !receipt.hydrated);
        assert_eq!(receipt.accepted_bytes, bytes.len() as u64);
    }
    assert!(
        response_mock.requests().is_empty(),
        "transferring stored history cannot buy a model response"
    );
    let rollout = RolloutRecorder::get_rollout_history(thread.path.as_ref().unwrap()).await?;
    let InitialHistory::Resumed(rollout) = rollout else {
        unreachable!()
    };
    assert_eq!(
        rollout
            .history
            .iter()
            .filter(|item| matches!(
                item,
                RolloutItem::WhisplyHistoryRecovery(WhisplyHistoryRecoveryRecord::Frame { .. })
            ))
            .count(),
        1
    );
    let invalid = mcp.send_thread_inject_items_request(ThreadInjectItemsParams {
        thread_id: thread.id.clone(), items: vec![serde_json::json!({"type":"message","role":"developer","content":[{"type":"input_text","text":"never promote this"}]})],
        whisply_history_recovery: Some(frame.clone()),
    }).await?;
    let error = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(invalid)),
    )
    .await??;
    assert_eq!(error.error.code, -32600);
    let turn = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id.clone(),
            input: vec![V2UserInput::Text {
                text: "FRESH_AFTER_STORED_HISTORY".to_owned(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    let _: TurnStartResponse = timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(turn)).await??;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;
    let request = response_mock.single_request();
    let texts = request.message_input_texts("user");
    assert_eq!(texts.iter().filter(|text| **text == old).count(), 1);
    assert_eq!(
        texts
            .iter()
            .filter(|text| text.as_str() == "FRESH_AFTER_STORED_HISTORY")
            .count(),
        1
    );
    let mut other = frame;
    other.archive_id = "b".repeat(64);
    let changed = mcp
        .send_thread_inject_items_request(ThreadInjectItemsParams {
            thread_id: thread.id,
            items: Vec::new(),
            whisply_history_recovery: Some(other),
        })
        .await?;
    let error = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(changed)),
    )
    .await??;
    assert_eq!(error.error.code, -32600);
    Ok(())
}

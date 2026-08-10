//! End-to-end compaction flow tests.
//!
//! Phases:
//! 1) Arrange: a managed broker fixture and mock Responses endpoint.
//! 2) Act: start a thread and submit multiple turns to trigger auto-compaction.
//! 3) Assert: verify item/started + item/completed notifications for context compaction.

use anyhow::Result;
#[cfg(target_os = "macos")]
use app_test_support::ManagedWhisplyGatewayFixture;
use app_test_support::TestAppServer;
#[cfg(target_os = "macos")]
use codex_app_server_protocol::ItemCompletedNotification;
#[cfg(target_os = "macos")]
use codex_app_server_protocol::ItemStartedNotification;
use codex_app_server_protocol::JSONRPCError;
#[cfg(target_os = "macos")]
use codex_app_server_protocol::RawResponseCompletedNotification;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ThreadCompactStartParams;
#[cfg(target_os = "macos")]
use codex_app_server_protocol::ThreadCompactStartResponse;
#[cfg(target_os = "macos")]
use codex_app_server_protocol::ThreadItem;
#[cfg(target_os = "macos")]
use codex_app_server_protocol::ThreadStartParams;
#[cfg(target_os = "macos")]
use codex_app_server_protocol::ThreadStartResponse;
#[cfg(target_os = "macos")]
use codex_app_server_protocol::TokenUsageBreakdown;
#[cfg(target_os = "macos")]
use codex_app_server_protocol::TurnCompletedNotification;
#[cfg(target_os = "macos")]
use codex_app_server_protocol::TurnStartParams;
#[cfg(target_os = "macos")]
use codex_app_server_protocol::TurnStartResponse;
#[cfg(target_os = "macos")]
use codex_app_server_protocol::UserInput as V2UserInput;
#[cfg(target_os = "macos")]
use core_test_support::responses;
use pretty_assertions::assert_eq;
use std::path::Path;
use tempfile::TempDir;
use tokio::time::timeout;

// macOS and Windows Bazel CI can spend tens of seconds starting app-server
// subprocesses or processing test RPCs under load.
#[cfg(any(target_os = "macos", windows))]
const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
#[cfg(not(any(target_os = "macos", windows)))]
const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const AUTO_COMPACT_LIMIT: i64 = 1_000;
const COMPACT_PROMPT: &str = "Summarize the conversation.";
const INVALID_REQUEST_ERROR_CODE: i64 = -32600;

#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_compaction_local_emits_started_and_completed_items() -> Result<()> {
    let server = responses::start_mock_server().await;
    let sse1 = responses::sse(vec![
        responses::ev_assistant_message("m1", "FIRST_REPLY"),
        responses::ev_completed_with_tokens("r1", /*total_tokens*/ 70_000),
    ]);
    let sse2 = responses::sse(vec![
        responses::ev_assistant_message("m2", "SECOND_REPLY"),
        responses::ev_completed_with_tokens("r2", /*total_tokens*/ 330_000),
    ]);
    let sse3 = responses::sse(vec![
        responses::ev_assistant_message("m3", "LOCAL_SUMMARY"),
        responses::ev_completed_with_tokens("r3", /*total_tokens*/ 200),
    ]);
    let sse4 = responses::sse(vec![
        responses::ev_assistant_message("m4", "FINAL_REPLY"),
        responses::ev_completed_with_tokens("r4", /*total_tokens*/ 120),
    ]);
    responses::mount_sse_sequence(&server, vec![sse1, sse2, sse3, sse4]).await;

    let codex_home = TempDir::new()?;
    write_managed_compaction_config(codex_home.path(), AUTO_COMPACT_LIMIT)?;
    let managed_gateway = ManagedWhisplyGatewayFixture::new(&server.uri())?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .with_env_overrides(&[("OPENAI_API_KEY", None)])
        .with_managed_whisply_gateway(managed_gateway)
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;

    let thread_id = start_thread(&mut mcp).await?;
    for message in ["first", "second", "third"] {
        send_turn_and_wait(&mut mcp, &thread_id, message).await?;
    }

    let started = wait_for_context_compaction_started(&mut mcp).await?;
    let completed = wait_for_context_compaction_completed(&mut mcp).await?;

    let ThreadItem::ContextCompaction { id: started_id } = started.item else {
        unreachable!("started item should be context compaction");
    };
    let ThreadItem::ContextCompaction { id: completed_id } = completed.item else {
        unreachable!("completed item should be context compaction");
    };

    assert_eq!(started.thread_id, thread_id);
    assert_eq!(completed.thread_id, thread_id);
    assert_eq!(started_id, completed_id);
    mcp.assert_managed_whisply_gateway_healthy()?;

    Ok(())
}

#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn thread_compact_start_triggers_compaction_and_returns_empty_response() -> Result<()> {
    let server = responses::start_mock_server().await;
    let sse = responses::sse(vec![
        responses::ev_assistant_message("m1", "MANUAL_COMPACT_SUMMARY"),
        responses::ev_completed_with_tokens("r1", /*total_tokens*/ 200),
    ]);
    responses::mount_sse_sequence(&server, vec![sse]).await;

    let codex_home = TempDir::new()?;
    write_managed_compaction_config(codex_home.path(), AUTO_COMPACT_LIMIT)?;
    let managed_gateway = ManagedWhisplyGatewayFixture::new(&server.uri())?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .with_env_overrides(&[("OPENAI_API_KEY", None)])
        .with_managed_whisply_gateway(managed_gateway)
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;

    let thread_req = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams {
            model: Some("mock-model".to_string()),
            experimental_raw_events: true,
            ..Default::default()
        })
        .await?;
    let ThreadStartResponse { thread, .. } =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(thread_req)).await??;
    let thread_id = thread.id;
    let compact_id = mcp
        .send_thread_compact_start_request(ThreadCompactStartParams {
            thread_id: thread_id.clone(),
        })
        .await?;
    let _: ThreadCompactStartResponse =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(compact_id)).await??;

    let started = wait_for_context_compaction_started(&mut mcp).await?;
    let raw_completed: RawResponseCompletedNotification = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_notification("rawResponse/completed"),
    )
    .await??;
    let completed = wait_for_context_compaction_completed(&mut mcp).await?;

    let ThreadItem::ContextCompaction { id: started_id } = started.item else {
        unreachable!("started item should be context compaction");
    };
    let ThreadItem::ContextCompaction { id: completed_id } = completed.item else {
        unreachable!("completed item should be context compaction");
    };

    assert_eq!(started.thread_id, thread_id);
    assert_eq!(completed.thread_id, thread_id);
    assert_eq!(started_id, completed_id);
    assert_eq!(
        raw_completed,
        RawResponseCompletedNotification {
            thread_id,
            turn_id: started.turn_id,
            response_id: "r1".to_string(),
            usage: Some(TokenUsageBreakdown {
                total_tokens: 200,
                input_tokens: 200,
                cached_input_tokens: 0,
                cache_write_input_tokens: 0,
                output_tokens: 0,
                reasoning_output_tokens: 0,
            }),
        }
    );
    mcp.assert_managed_whisply_gateway_healthy()?;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn thread_compact_start_rejects_invalid_thread_id() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_managed_compaction_config(codex_home.path(), AUTO_COMPACT_LIMIT)?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;

    let request_id = mcp
        .send_thread_compact_start_request(ThreadCompactStartParams {
            thread_id: "not-a-thread-id".to_string(),
        })
        .await?;
    let error: JSONRPCError = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;

    assert_eq!(error.error.code, INVALID_REQUEST_ERROR_CODE);
    assert!(error.error.message.contains("invalid thread id"));

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn thread_compact_start_rejects_unknown_thread_id() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_managed_compaction_config(codex_home.path(), AUTO_COMPACT_LIMIT)?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;

    let request_id = mcp
        .send_thread_compact_start_request(ThreadCompactStartParams {
            thread_id: "67e55044-10b1-426f-9247-bb680e5fe0c8".to_string(),
        })
        .await?;
    let error: JSONRPCError = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;

    assert_eq!(error.error.code, INVALID_REQUEST_ERROR_CODE);
    assert!(error.error.message.contains("thread not found"));

    Ok(())
}

#[cfg(target_os = "macos")]
async fn start_thread(mcp: &mut TestAppServer) -> Result<String> {
    let thread_id = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams {
            model: Some("mock-model".to_string()),
            ..Default::default()
        })
        .await?;
    let ThreadStartResponse { thread, .. } =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(thread_id)).await??;
    Ok(thread.id)
}

#[cfg(target_os = "macos")]
async fn send_turn_and_wait(
    mcp: &mut TestAppServer,
    thread_id: &str,
    text: &str,
) -> Result<String> {
    let turn_id = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread_id.to_string(),
            client_user_message_id: None,
            input: vec![V2UserInput::Text {
                text: text.to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    let TurnStartResponse { turn } =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(turn_id)).await??;
    wait_for_turn_completed(mcp, &turn.id).await?;
    Ok(turn.id)
}

#[cfg(target_os = "macos")]
async fn wait_for_turn_completed(mcp: &mut TestAppServer, turn_id: &str) -> Result<()> {
    loop {
        let completed: TurnCompletedNotification = timeout(
            DEFAULT_READ_TIMEOUT,
            mcp.read_notification("turn/completed"),
        )
        .await??;
        if completed.turn.id == turn_id {
            return Ok(());
        }
    }
}

#[cfg(target_os = "macos")]
async fn wait_for_context_compaction_started(
    mcp: &mut TestAppServer,
) -> Result<ItemStartedNotification> {
    loop {
        let started: ItemStartedNotification =
            timeout(DEFAULT_READ_TIMEOUT, mcp.read_notification("item/started")).await??;
        if let ThreadItem::ContextCompaction { .. } = started.item {
            return Ok(started);
        }
    }
}

#[cfg(target_os = "macos")]
async fn wait_for_context_compaction_completed(
    mcp: &mut TestAppServer,
) -> Result<ItemCompletedNotification> {
    loop {
        let completed: ItemCompletedNotification = timeout(
            DEFAULT_READ_TIMEOUT,
            mcp.read_notification("item/completed"),
        )
        .await??;
        if let ThreadItem::ContextCompaction { .. } = completed.item {
            return Ok(completed);
        }
    }
}

fn write_managed_compaction_config(
    codex_home: &Path,
    auto_compact_limit: i64,
) -> std::io::Result<()> {
    std::fs::write(
        codex_home.join("config.toml"),
        format!(
            r#"
model = "mock-model"
approval_policy = "never"
sandbox_mode = "read-only"
compact_prompt = "{COMPACT_PROMPT}"
model_auto_compact_token_limit = {auto_compact_limit}
model_provider = "whisply"
"#
        ),
    )
}

#![cfg(target_os = "macos")]

use anyhow::Result;
use app_test_support::ManagedWhisplyConfig;
use app_test_support::TestAppServer;
use codex_app_server_protocol::ThreadHistoryMode;
use codex_app_server_protocol::ThreadResumeParams;
use codex_app_server_protocol::ThreadResumeResponse;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::UserInput;
use core_test_support::responses;
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use tokio::time::timeout;
use whisply_thread_store::LocalThreadStore;
use whisply_thread_store::LocalThreadStoreConfig;
use whisply_thread_store::RolloutMigrationMode;
use whisply_thread_store::RolloutMigrationOptions;
use whisply_thread_store::RolloutMigrationStatus;
use whisply_utils_absolute_path::test_support::PathExt;

const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[tokio::test]
async fn migrated_legacy_thread_cold_resume_preserves_model_context() -> Result<()> {
    let server = responses::start_mock_server().await;
    let response_mock = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![
                responses::ev_response_created("resp-1"),
                responses::ev_assistant_message("msg-1", "legacy assistant message"),
                responses::ev_completed("resp-1"),
            ]),
            responses::sse(vec![
                responses::ev_response_created("resp-2"),
                responses::ev_assistant_message("msg-2", "resumed assistant message"),
                responses::ev_completed("resp-2"),
            ]),
        ],
    )
    .await;
    let codex_home = TempDir::new()?;
    ManagedWhisplyConfig::new().write(codex_home.path())?;

    let mut primary = app_test_support::managed_whisply_app_server_builder!(&server.uri())
        .with_codex_home(codex_home.path())
        .build_initialized()
        .await?;
    let start_id = primary
        .send_thread_start_request_with_auto_env(ThreadStartParams {
            history_mode: Some(ThreadHistoryMode::Legacy),
            ..Default::default()
        })
        .await?;
    let ThreadStartResponse { thread, .. } =
        timeout(DEFAULT_READ_TIMEOUT, primary.read_response(start_id)).await??;
    timeout(
        DEFAULT_READ_TIMEOUT,
        primary.start_turn_and_wait_for_completion(TurnStartParams {
            thread_id: thread.id.clone(),
            input: vec![UserInput::Text {
                text: "legacy user message".to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        }),
    )
    .await??;
    timeout(DEFAULT_READ_TIMEOUT, primary.shutdown_gracefully()).await??;

    let sqlite = whisply_state::SqliteConfig::new_for_testing(codex_home.path().abs());
    let state_db = whisply_state::StateRuntime::init(sqlite.clone(), "whisply".to_string()).await?;
    let store = LocalThreadStore::new(
        LocalThreadStoreConfig {
            codex_home: codex_home.path().to_path_buf(),
            sqlite,
            default_model_provider_id: "whisply".to_string(),
        },
        Some(state_db),
    );
    let report = store
        .migrate_rollouts(RolloutMigrationOptions {
            mode: RolloutMigrationMode::Apply,
            max_mib_per_second: 1024,
            ..RolloutMigrationOptions::default()
        })
        .await?;
    assert_eq!(report.outcomes.len(), 1);
    assert_eq!(report.outcomes[0].status, RolloutMigrationStatus::Migrated);
    drop(store);

    let mut secondary = app_test_support::managed_whisply_app_server_builder!(&server.uri())
        .with_codex_home(codex_home.path())
        .build_initialized()
        .await?;
    let resume_id = secondary
        .send_thread_resume_request(ThreadResumeParams {
            thread_id: thread.id.clone(),
            exclude_turns: true,
            ..Default::default()
        })
        .await?;
    let ThreadResumeResponse {
        thread: resumed, ..
    } = timeout(DEFAULT_READ_TIMEOUT, secondary.read_response(resume_id)).await??;
    assert_eq!(resumed.history_mode, ThreadHistoryMode::Paginated);

    timeout(
        DEFAULT_READ_TIMEOUT,
        secondary.start_turn_and_wait_for_completion(TurnStartParams {
            thread_id: thread.id,
            input: vec![UserInput::Text {
                text: "resumed user message".to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        }),
    )
    .await??;

    let requests = response_mock.requests();
    assert_eq!(requests.len(), 2);
    let resumed_request = requests.last().expect("resumed turn request");
    let user_messages = resumed_request.message_input_texts("user");
    assert!(user_messages.contains(&"legacy user message".to_string()));
    assert!(user_messages.contains(&"resumed user message".to_string()));
    assert!(resumed_request.body_contains_text("legacy assistant message"));

    Ok(())
}

use std::time::Duration;

use anyhow::Result;
use app_test_support::TestAppServer;
use codex_app_server_protocol::AddCreditsNudgeCreditType;
use codex_app_server_protocol::JSONRPCError;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::SendAddCreditsNudgeEmailParams;
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const WHISPLY_MANAGED_ACCOUNT_DATA_UNAVAILABLE_ERROR: &str =
    "Whisply account data is managed by the installed app and is unavailable in this runtime.";

#[tokio::test]
async fn get_account_rate_limits_rejects_direct_authority_from_blank_config() -> Result<()> {
    let codex_home = TempDir::new()?;
    let mut app_server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .with_env_overrides(&[("OPENAI_API_KEY", None)])
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let request_id = app_server.send_get_account_rate_limits_request().await?;
    assert_managed_account_data_error(&mut app_server, request_id).await?;
    assert_blank_config(&codex_home);
    Ok(())
}

#[tokio::test]
async fn send_add_credits_nudge_email_rejects_direct_authority_from_blank_config() -> Result<()> {
    let codex_home = TempDir::new()?;
    let mut app_server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .with_env_overrides(&[("OPENAI_API_KEY", None)])
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let request_id = app_server
        .send_add_credits_nudge_email_request(SendAddCreditsNudgeEmailParams {
            credit_type: AddCreditsNudgeCreditType::UsageLimit,
        })
        .await?;
    assert_managed_account_data_error(&mut app_server, request_id).await?;
    assert_blank_config(&codex_home);
    Ok(())
}

async fn assert_managed_account_data_error(
    app_server: &mut TestAppServer,
    request_id: i64,
) -> Result<()> {
    let error: JSONRPCError = timeout(
        DEFAULT_TIMEOUT,
        app_server.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;
    assert_eq!(error.id, RequestId::Integer(request_id));
    assert_eq!(error.error.code, -32600);
    assert_eq!(
        error.error.message,
        WHISPLY_MANAGED_ACCOUNT_DATA_UNAVAILABLE_ERROR
    );
    Ok(())
}

fn assert_blank_config(codex_home: &TempDir) {
    assert!(
        !codex_home.path().join("config.toml").exists(),
        "managed account RPCs must not create direct-authority configuration"
    );
    assert!(
        !codex_home.path().join("auth.json").exists(),
        "managed account RPCs must not create direct-authority credentials"
    );
}

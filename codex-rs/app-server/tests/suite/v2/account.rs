use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use app_test_support::TestAppServer;
use codex_app_server_protocol::AddCreditsNudgeCreditType;
use codex_app_server_protocol::ConsumeAccountRateLimitResetCreditParams;
use codex_app_server_protocol::GetAccountParams;
use codex_app_server_protocol::GetAccountResponse;
use codex_app_server_protocol::GetAuthStatusParams;
use codex_app_server_protocol::GetAuthStatusResponse;
use codex_app_server_protocol::JSONRPCError;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::SendAddCreditsNudgeEmailParams;
use serde_json::json;
use tempfile::TempDir;
use tokio::time::timeout;
use wiremock::MockServer;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const WHISPLY_MANAGED_ACCOUNT_LOGIN_ERROR: &str =
    "Whisply account login is managed by the installed app. Sign in through Whisply.";
const WHISPLY_MANAGED_ACCOUNT_LOGOUT_ERROR: &str =
    "Whisply account logout is managed by the installed app. Sign out through Whisply.";
const WHISPLY_MANAGED_ACCOUNT_DATA_UNAVAILABLE_ERROR: &str =
    "Whisply account data is managed by the installed app and is unavailable in this runtime.";

fn write_quarantined_chatgpt_auth(codex_home: &Path) -> Result<Vec<u8>> {
    let bytes = serde_json::to_vec(&json!({
        "auth_mode": "chatgpt",
        "tokens": {
            "id_token": "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.eyJlbWFpbCI6Im1hbmFnZWQtYXV0aC10ZXN0QGV4YW1wbGUuaW52YWxpZCIsImh0dHBzOi8vYXBpLm9wZW5haS5jb20vYXV0aCI6eyJjaGF0Z3B0X2FjY291bnRfaWQiOiJhY2NvdW50LTEyMyIsImNoYXRncHRfcGxhbl90eXBlIjoicGx1cyJ9fQ.c2lnbmF0dXJl",
            "access_token": "quarantined-access-token",
            "refresh_token": "quarantined-refresh-token",
            "account_id": "account-123"
        }
    }))?;
    std::fs::write(codex_home.join("auth.json"), &bytes)?;
    Ok(bytes)
}

#[tokio::test]
async fn account_login_start_rejects_every_generic_credential_before_mutation() -> Result<()> {
    let codex_home = TempDir::new()?;
    std::fs::write(codex_home.path().join("config.toml"), "")?;
    let auth_path = codex_home.path().join("auth.json");
    std::fs::write(&auth_path, "{}")?;
    let auth_before = std::fs::read(&auth_path)?;

    let mut app_server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let generic_login_params = [
        json!({ "type": "apiKey", "apiKey": "new-user-api-key" }),
        json!({
            "type": "amazonBedrock",
            "apiKey": "new-bedrock-api-key",
            "region": "us-west-2",
        }),
        json!({ "type": "chatgpt" }),
        json!({ "type": "chatgptDeviceCode" }),
        json!({
            "type": "chatgptAuthTokens",
            "accessToken": "user-token",
            "chatgptAccountId": "account-123",
            "chatgptPlanType": "plus",
        }),
    ];

    for params in generic_login_params {
        let request_id = app_server.send_login_account_request(params).await?;
        let error: JSONRPCError = timeout(
            DEFAULT_TIMEOUT,
            app_server.read_stream_until_error_message(RequestId::Integer(request_id)),
        )
        .await??;
        assert_eq!(error.error.code, -32600);
        assert_eq!(error.error.message, WHISPLY_MANAGED_ACCOUNT_LOGIN_ERROR);
        assert_eq!(std::fs::read(&auth_path)?, auth_before);
    }
    Ok(())
}

#[tokio::test]
async fn persisted_chatgpt_auth_cannot_activate_account_or_backend_client_routes() -> Result<()> {
    let outbound_probe = MockServer::start().await;
    let proxy_uri = outbound_probe.uri();
    let codex_home = TempDir::new()?;
    std::fs::write(codex_home.path().join("config.toml"), "")?;
    let auth_path = codex_home.path().join("auth.json");
    let auth_before = write_quarantined_chatgpt_auth(codex_home.path())?;

    let mut app_server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        // If a regressed route tries to reach the historical first-party
        // backend, this child-local probe receives the proxy CONNECT request.
        // Clear all inherited proxy exclusions/overrides so host environment
        // configuration cannot cause a direct request to evade the probe.
        .with_env_overrides(&[
            ("HTTP_PROXY", Some(proxy_uri.as_str())),
            ("http_proxy", Some(proxy_uri.as_str())),
            ("HTTPS_PROXY", Some(proxy_uri.as_str())),
            ("https_proxy", Some(proxy_uri.as_str())),
            ("ALL_PROXY", None),
            ("all_proxy", None),
            ("NO_PROXY", None),
            ("no_proxy", None),
        ])
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let auth_status_id = app_server
        .send_get_auth_status_request(GetAuthStatusParams {
            include_token: Some(true),
            refresh_token: Some(true),
        })
        .await?;
    let auth_status: GetAuthStatusResponse =
        timeout(DEFAULT_TIMEOUT, app_server.read_response(auth_status_id)).await??;
    assert_eq!(auth_status.auth_method, None);
    assert_eq!(auth_status.auth_token, None);
    assert_eq!(auth_status.requires_openai_auth, Some(false));

    let account_id = app_server
        .send_get_account_request(GetAccountParams {
            refresh_token: true,
        })
        .await?;
    let account: GetAccountResponse =
        timeout(DEFAULT_TIMEOUT, app_server.read_response(account_id)).await??;
    assert_eq!(account.account, None);
    assert!(!account.requires_openai_auth);

    let rate_limits_id = app_server.send_get_account_rate_limits_request().await?;
    assert_managed_account_data_error(&mut app_server, rate_limits_id).await?;

    let usage_id = app_server
        .send_raw_request("account/usage/read", /*params*/ None)
        .await?;
    assert_managed_account_data_error(&mut app_server, usage_id).await?;

    let workspace_messages_id = app_server
        .send_raw_request("account/workspaceMessages/read", /*params*/ None)
        .await?;
    assert_managed_account_data_error(&mut app_server, workspace_messages_id).await?;

    let nudge_id = app_server
        .send_add_credits_nudge_email_request(SendAddCreditsNudgeEmailParams {
            credit_type: AddCreditsNudgeCreditType::Credits,
        })
        .await?;
    assert_managed_account_data_error(&mut app_server, nudge_id).await?;

    let reset_id = app_server
        .send_consume_account_rate_limit_reset_credit_request(
            ConsumeAccountRateLimitResetCreditParams {
                idempotency_key: "direct-backend-reset-attempt".to_string(),
                credit_id: None,
            },
        )
        .await?;
    assert_managed_account_data_error(&mut app_server, reset_id).await?;

    let logout_id = app_server.send_logout_account_request().await?;
    let logout_error: JSONRPCError = timeout(
        DEFAULT_TIMEOUT,
        app_server.read_stream_until_error_message(RequestId::Integer(logout_id)),
    )
    .await??;
    assert_eq!(logout_error.error.code, -32600);
    assert_eq!(
        logout_error.error.message,
        WHISPLY_MANAGED_ACCOUNT_LOGOUT_ERROR
    );
    assert_eq!(
        std::fs::read(&auth_path)?,
        auth_before,
        "managed logout must retain quarantined legacy auth evidence"
    );

    assert!(
        outbound_probe
            .received_requests()
            .await
            .is_none_or(|requests| requests.is_empty()),
        "persisted ChatGPT auth must not send account or BackendClient traffic"
    );
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
    assert_eq!(error.error.code, -32600);
    assert_eq!(
        error.error.message,
        WHISPLY_MANAGED_ACCOUNT_DATA_UNAVAILABLE_ERROR
    );
    Ok(())
}

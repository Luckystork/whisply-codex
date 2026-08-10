use std::time::Duration;

use anyhow::Result;
use app_test_support::TestAppServer;
use codex_app_server_protocol::AppsInstalledParams;
use codex_app_server_protocol::JSONRPCError;
use codex_app_server_protocol::RequestId;
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const WHISPLY_MANAGED_APPS_UNAVAILABLE_ERROR: &str =
    "Whisply connected apps are managed by the installed app and are unavailable in this runtime.";

#[tokio::test]
async fn app_installed_rejects_direct_authority_before_upstream_io() -> Result<()> {
    let codex_home = TempDir::new()?;

    // Keep the fixture managed-safe: configuration URL rejection is covered
    // at the config boundary, while this test verifies the app RPC gate.
    let mut app_server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;
    let request_id = app_server
        .send_apps_installed_request(AppsInstalledParams {
            thread_id: None,
            force_refresh: true,
        })
        .await?;
    let error: JSONRPCError = timeout(
        DEFAULT_TIMEOUT,
        app_server.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;

    assert_eq!(error.error.code, -32600);
    assert_eq!(error.error.message, WHISPLY_MANAGED_APPS_UNAVAILABLE_ERROR);
    assert!(!codex_home.path().join("config.toml").exists());
    assert!(!codex_home.path().join("auth.json").exists());
    Ok(())
}

use std::time::Duration;

use anyhow::Result;
use app_test_support::TestAppServer;
use codex_app_server_protocol::AppsInstalledParams;
use codex_app_server_protocol::AppsListParams;
use codex_app_server_protocol::AppsReadParams;
use codex_app_server_protocol::JSONRPCError;
use codex_app_server_protocol::RequestId;
use tokio::time::timeout;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const WHISPLY_MANAGED_APPS_UNAVAILABLE_ERROR: &str =
    "Whisply connected apps are managed by the installed app and are unavailable in this runtime.";

#[tokio::test]
async fn app_requests_reject_direct_apps_authority() -> Result<()> {
    let codex_home = tempfile::TempDir::new()?;
    let mut app_server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let list_request_id = app_server
        .send_apps_list_request(AppsListParams {
            limit: None,
            cursor: None,
            thread_id: None,
            force_refetch: true,
        })
        .await?;
    assert_managed_whisply_apps_error(&mut app_server, list_request_id).await?;

    let read_request_id = app_server
        .send_apps_read_request(AppsReadParams {
            app_ids: (0..101).map(|index| format!("app-{index}")).collect(),
            include_tools: true,
        })
        .await?;
    assert_managed_whisply_apps_error(&mut app_server, read_request_id).await?;

    let installed_request_id = app_server
        .send_apps_installed_request(AppsInstalledParams {
            thread_id: None,
            force_refresh: true,
        })
        .await?;
    assert_managed_whisply_apps_error(&mut app_server, installed_request_id).await?;

    Ok(())
}

async fn assert_managed_whisply_apps_error(
    app_server: &mut TestAppServer,
    request_id: i64,
) -> Result<()> {
    let error: JSONRPCError = timeout(
        DEFAULT_TIMEOUT,
        app_server.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;

    assert_eq!(error.error.code, -32600);
    assert_eq!(error.error.message, WHISPLY_MANAGED_APPS_UNAVAILABLE_ERROR);
    Ok(())
}

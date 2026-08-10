use std::time::Duration;

use anyhow::Result;
use app_test_support::TestAppServer;
use codex_app_server_protocol::RequestId;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;
use tokio::time::timeout;
use wiremock::MockServer;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
const WHISPLY_MANAGED_REMOTE_PLUGIN_OPERATION_UNAVAILABLE_ERROR: &str = "Whisply remote plugin operations are managed by the installed app and are unavailable in this runtime.";
const REMOTE_PLUGIN_ID: &str = "plugins~Plugin_22222222222222222222222222222222";

#[tokio::test]
async fn plugin_share_crud_is_managed_and_never_uses_direct_backend_io() -> Result<()> {
    let codex_home = TempDir::new()?;
    let outbound_probe = MockServer::start().await;
    let proxy_uri = outbound_probe.uri();
    let plugin_path = codex_home.path().join("local-plugin");
    let mut app_server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
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

    let requests = [
        (
            "plugin/share/save",
            json!({
                "pluginPath": plugin_path,
                "remotePluginId": null,
                "discoverability": "PRIVATE",
                "shareTargets": []
            }),
        ),
        (
            "plugin/share/updateTargets",
            json!({
                "remotePluginId": REMOTE_PLUGIN_ID,
                "discoverability": "PRIVATE",
                "shareTargets": []
            }),
        ),
        ("plugin/share/list", json!({})),
        (
            "plugin/share/checkout",
            json!({ "remotePluginId": REMOTE_PLUGIN_ID }),
        ),
        (
            "plugin/share/delete",
            json!({ "remotePluginId": REMOTE_PLUGIN_ID }),
        ),
    ];
    for (method, params) in requests {
        let request_id = app_server.send_raw_request(method, Some(params)).await?;
        let error = timeout(
            DEFAULT_TIMEOUT,
            app_server.read_stream_until_error_message(RequestId::Integer(request_id)),
        )
        .await??;
        assert_eq!(error.error.code, -32600, "{method}");
        assert_eq!(
            error.error.message, WHISPLY_MANAGED_REMOTE_PLUGIN_OPERATION_UNAVAILABLE_ERROR,
            "{method}"
        );
    }

    assert!(
        outbound_probe
            .received_requests()
            .await
            .expect("probe should record requests")
            .is_empty(),
        "remote plugin share methods must fail before any direct backend request"
    );
    Ok(())
}

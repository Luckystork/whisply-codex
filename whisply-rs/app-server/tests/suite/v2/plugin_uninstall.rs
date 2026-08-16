use std::time::Duration;

use anyhow::Result;
use app_test_support::TestAppServer;
use codex_app_server_protocol::PluginUninstallParams;
use codex_app_server_protocol::PluginUninstallResponse;
use codex_app_server_protocol::RequestId;
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use tokio::time::timeout;
use wiremock::MockServer;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
const REMOTE_PLUGIN_ID: &str = "plugins~Plugin_22222222222222222222222222222222";
const WHISPLY_MANAGED_REMOTE_PLUGIN_OPERATION_UNAVAILABLE_ERROR: &str = "Whisply remote plugin operations are managed by the installed app and are unavailable in this runtime.";

#[tokio::test]
async fn plugin_uninstall_removes_local_plugin_cache_and_config_entry() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_installed_plugin(&codex_home, "debug", "sample-plugin")?;
    std::fs::write(
        codex_home.path().join("config.toml"),
        r#"[features]
plugins = true

[plugins."sample-plugin@debug"]
enabled = true
"#,
    )?;

    let mut app_server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let request_id = app_server
        .send_plugin_uninstall_request(PluginUninstallParams {
            plugin_id: "sample-plugin@debug".to_string(),
        })
        .await?;
    let response: PluginUninstallResponse =
        timeout(DEFAULT_TIMEOUT, app_server.read_response(request_id)).await??;
    assert_eq!(response, PluginUninstallResponse {});
    assert!(
        !codex_home
            .path()
            .join("plugins/cache/debug/sample-plugin")
            .exists()
    );
    assert!(
        !std::fs::read_to_string(codex_home.path().join("config.toml"))?
            .contains(r#"[plugins."sample-plugin@debug"]"#)
    );
    Ok(())
}

#[tokio::test]
async fn plugin_uninstall_rejects_managed_remote_plugin_before_direct_backend_io() -> Result<()> {
    let codex_home = TempDir::new()?;
    let outbound_probe = MockServer::start().await;
    let proxy_uri = outbound_probe.uri();
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

    let request_id = app_server
        .send_plugin_uninstall_request(PluginUninstallParams {
            plugin_id: REMOTE_PLUGIN_ID.to_string(),
        })
        .await?;
    let error = timeout(
        DEFAULT_TIMEOUT,
        app_server.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;
    assert_eq!(error.error.code, -32600);
    assert_eq!(
        error.error.message,
        WHISPLY_MANAGED_REMOTE_PLUGIN_OPERATION_UNAVAILABLE_ERROR
    );
    assert!(
        outbound_probe
            .received_requests()
            .await
            .expect("probe should record requests")
            .is_empty(),
        "remote uninstall must fail before any direct backend request"
    );
    Ok(())
}

fn write_installed_plugin(
    codex_home: &TempDir,
    marketplace_name: &str,
    plugin_name: &str,
) -> Result<()> {
    let plugin_root = codex_home
        .path()
        .join("plugins/cache")
        .join(marketplace_name)
        .join(plugin_name)
        .join("local");
    std::fs::create_dir_all(plugin_root.join(".codex-plugin"))?;
    std::fs::write(
        plugin_root.join(".codex-plugin/plugin.json"),
        format!(r#"{{"name":"{plugin_name}"}}"#),
    )?;
    Ok(())
}

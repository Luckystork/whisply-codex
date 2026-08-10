use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use app_test_support::TestAppServer;
use codex_app_server_protocol::PluginSearchParams;
use codex_app_server_protocol::PluginSearchResponse;
use codex_app_server_protocol::PluginSearchScope;
use codex_app_server_protocol::RequestId;
use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use tokio::time::timeout;
use wiremock::MockServer;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
const WHISPLY_MANAGED_REMOTE_PLUGIN_SEARCH_UNAVAILABLE_ERROR: &str = "Whisply remote plugin search is managed by the installed app and is unavailable in this runtime.";

#[tokio::test]
async fn plugin_search_rejects_managed_remote_catalog_before_direct_backend_io() -> Result<()> {
    let codex_home = TempDir::new()?;
    let outbound_probe = MockServer::start().await;
    let proxy_uri = outbound_probe.uri();
    write_plugins_config(codex_home.path(), /*remote_plugin*/ true)?;

    let mut mcp = TestAppServer::builder()
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

    let request_id = mcp
        .send_plugin_search_request(PluginSearchParams {
            search_term: "calendar".to_string(),
            scope: None,
            cwds: None,
            cursor: None,
            limit: None,
        })
        .await?;
    let error = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;

    assert_eq!(error.error.code, -32600);
    assert_eq!(
        error.error.message,
        WHISPLY_MANAGED_REMOTE_PLUGIN_SEARCH_UNAVAILABLE_ERROR
    );
    assert!(
        outbound_probe
            .received_requests()
            .await
            .expect("probe should record requests")
            .is_empty(),
        "managed remote plugin search must not reach a direct backend"
    );
    Ok(())
}

#[tokio::test]
async fn plugin_search_returns_local_projection_without_remote_catalog_io() -> Result<()> {
    let codex_home = TempDir::new()?;
    let workspace = TempDir::new()?;
    let outbound_probe = MockServer::start().await;
    let proxy_uri = outbound_probe.uri();
    write_plugins_config(codex_home.path(), /*remote_plugin*/ false)?;
    let marketplace_path = write_local_marketplace(workspace.path())?;

    let mut mcp = TestAppServer::builder()
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
    let cwds = Some(vec![AbsolutePathBuf::try_from(workspace.path())?]);

    for scope in [None, Some(PluginSearchScope::Personal)] {
        let request_id = mcp
            .send_plugin_search_request(PluginSearchParams {
                search_term: "calendar".to_string(),
                scope,
                cwds: cwds.clone(),
                cursor: None,
                limit: None,
            })
            .await?;
        let response: PluginSearchResponse =
            timeout(DEFAULT_TIMEOUT, mcp.read_response(request_id)).await??;

        assert_eq!(response.next_cursor, None);
        assert_eq!(response.data.len(), 1);
        assert_eq!(response.data[0].marketplace_name, "local-tools");
        assert_eq!(response.data[0].plugin.id, "calendar-notes@local-tools");
        assert_eq!(
            response.data[0].marketplace_path.as_ref(),
            Some(&marketplace_path)
        );
        assert!(!response.data[0].plugin.enabled);
    }

    let request_id = mcp
        .send_plugin_search_request(PluginSearchParams {
            search_term: "calendar".to_string(),
            scope: Some(PluginSearchScope::Global),
            cwds,
            cursor: None,
            limit: None,
        })
        .await?;
    let response: PluginSearchResponse =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(request_id)).await??;
    assert_eq!(
        response,
        PluginSearchResponse {
            data: Vec::new(),
            next_cursor: None,
        }
    );
    assert!(
        outbound_probe
            .received_requests()
            .await
            .expect("probe should record requests")
            .is_empty(),
        "local plugin search must not reach a direct backend"
    );
    Ok(())
}

fn write_plugins_config(codex_home: &Path, remote_plugin: bool) -> std::io::Result<()> {
    std::fs::write(
        codex_home.join("config.toml"),
        format!(
            r#"[features]
plugins = true
remote_plugin = {remote_plugin}
"#
        ),
    )
}

fn write_local_marketplace(root: &Path) -> Result<AbsolutePathBuf> {
    std::fs::create_dir_all(root.join(".git"))?;
    std::fs::create_dir_all(root.join(".agents/plugins"))?;
    std::fs::create_dir_all(root.join("plugins/calendar-notes/.codex-plugin"))?;
    let marketplace_path = root.join(".agents/plugins/marketplace.json");
    std::fs::write(
        &marketplace_path,
        r#"{
  "name": "local-tools",
  "plugins": [
    {
      "name": "calendar-notes",
      "source": {
        "source": "local",
        "path": "./plugins/calendar-notes"
      }
    }
  ]
}"#,
    )?;
    std::fs::write(
        root.join("plugins/calendar-notes/.codex-plugin/plugin.json"),
        r#"{
  "name": "calendar-notes",
  "interface": {
    "displayName": "Calendar Notes",
    "shortDescription": "Take notes for calendar events"
  }
}"#,
    )?;
    Ok(AbsolutePathBuf::try_from(marketplace_path)?)
}

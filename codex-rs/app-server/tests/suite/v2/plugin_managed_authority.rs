use std::time::Duration;

use anyhow::Result;
use app_test_support::TestAppServer;
use codex_app_server_protocol::AppSummary;
use codex_app_server_protocol::PluginAuthPolicy;
use codex_app_server_protocol::PluginInstallParams;
use codex_app_server_protocol::PluginInstallResponse;
use codex_app_server_protocol::PluginReadParams;
use codex_app_server_protocol::PluginReadResponse;
use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
const WHISPLY_MANAGED_PLUGIN_APP_DESCRIPTION: &str =
    "Whisply manages this app connection through the installed app.";

#[tokio::test]
async fn plugin_read_uses_managed_app_summaries_without_connector_metadata() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_plugin_runtime_config(codex_home.path())?;
    let (_repository, marketplace_path) =
        write_local_plugin_with_app("calendar", Some("Productivity"))?;

    let mut app_server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .with_env_overrides(&[("CODEX_CONNECTORS_TOKEN", Some("legacy-connectors-token"))])
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let request_id = app_server
        .send_plugin_read_request(PluginReadParams {
            marketplace_path: Some(marketplace_path),
            remote_marketplace_name: None,
            plugin_name: "managed-app-plugin".to_string(),
        })
        .await?;
    let response: PluginReadResponse =
        timeout(DEFAULT_TIMEOUT, app_server.read_response(request_id)).await??;

    assert_eq!(
        response.plugin.apps,
        vec![managed_app_summary("calendar", Some("Productivity"))]
    );
    Ok(())
}

#[tokio::test]
async fn plugin_install_uses_managed_apps_needing_auth_without_connector_metadata() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_plugin_runtime_config(codex_home.path())?;
    let (_repository, marketplace_path) =
        write_local_plugin_with_app("calendar", Some("Productivity"))?;

    let mut app_server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .with_env_overrides(&[("CODEX_CONNECTORS_TOKEN", Some("legacy-connectors-token"))])
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let request_id = app_server
        .send_plugin_install_request(PluginInstallParams {
            marketplace_path: Some(marketplace_path),
            remote_marketplace_name: None,
            plugin_name: "managed-app-plugin".to_string(),
        })
        .await?;
    let response: PluginInstallResponse =
        timeout(DEFAULT_TIMEOUT, app_server.read_response(request_id)).await??;

    assert_eq!(
        response,
        PluginInstallResponse {
            auth_policy: PluginAuthPolicy::OnInstall,
            apps_needing_auth: vec![managed_app_summary("calendar", Some("Productivity"))],
        }
    );
    Ok(())
}

fn managed_app_summary(id: &str, category: Option<&str>) -> AppSummary {
    AppSummary {
        id: id.to_string(),
        name: id.to_string(),
        description: Some(WHISPLY_MANAGED_PLUGIN_APP_DESCRIPTION.to_string()),
        install_url: None,
        category: category.map(str::to_string),
    }
}

fn write_plugin_runtime_config(codex_home: &std::path::Path) -> Result<()> {
    std::fs::write(
        codex_home.join("config.toml"),
        r#"[features]
plugins = true
apps = true
connectors = true
"#,
    )?;
    Ok(())
}

fn write_local_plugin_with_app(
    app_id: &str,
    category: Option<&str>,
) -> Result<(TempDir, AbsolutePathBuf)> {
    let repository = tempfile::tempdir()?;
    let repository_path = repository.path();
    let plugin_root = repository_path.join("managed-app-plugin");
    std::fs::create_dir_all(plugin_root.join(".codex-plugin"))?;
    std::fs::create_dir_all(repository_path.join(".git"))?;
    std::fs::create_dir_all(repository_path.join(".agents/plugins"))?;
    std::fs::write(
        repository_path.join(".agents/plugins/marketplace.json"),
        r#"{
  "name": "managed",
  "plugins": [{
    "name": "managed-app-plugin",
    "source": { "source": "local", "path": "./managed-app-plugin" }
  }]
}"#,
    )?;
    std::fs::write(
        plugin_root.join(".codex-plugin/plugin.json"),
        r#"{"name":"managed-app-plugin"}"#,
    )?;
    std::fs::write(
        plugin_root.join(".app.json"),
        serde_json::to_vec_pretty(&json!({
            "apps": {
                "managed": {
                    "id": app_id,
                    "category": category,
                }
            }
        }))?,
    )?;
    let marketplace_path =
        AbsolutePathBuf::try_from(repository_path.join(".agents/plugins/marketplace.json"))?;
    Ok((repository, marketplace_path))
}

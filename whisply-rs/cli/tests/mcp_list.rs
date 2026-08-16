use std::path::Path;

use anyhow::Result;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;
use pretty_assertions::assert_eq;
use serde_json::Value as JsonValue;
use serde_json::json;
use tempfile::TempDir;
use whisply_config::types::McpServerTransportConfig;
use whisply_core::config::edit::ConfigEditsBuilder;
use whisply_core::config::load_global_mcp_servers;
use whisply_login::CODEX_API_KEY_ENV_VAR;
use wiremock::MockServer;

fn codex_command(codex_home: &Path) -> Result<assert_cmd::Command> {
    let mut cmd = assert_cmd::Command::new(whisply_utils_cargo_bin::cargo_bin("whisply")?);
    cmd.env("WHISPLY_HOME", codex_home)
        .env("WHISPLY_HOME", codex_home);
    Ok(cmd)
}

async fn configure_http_oauth_server(codex_home: &Path, url: &str) -> Result<()> {
    let mut servers = load_global_mcp_servers(codex_home).await?;
    servers.insert(
        "oauth".to_string(),
        toml::from_str(&format!("url = \"{url}\""))?,
    );
    ConfigEditsBuilder::new(codex_home)
        .replace_mcp_servers(&servers)
        .apply_blocking()?;
    Ok(())
}

#[test]
fn list_shows_empty_state() -> Result<()> {
    let codex_home = TempDir::new()?;

    let mut cmd = codex_command(codex_home.path())?;
    let output = cmd.args(["mcp", "list"]).output()?;
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout)?;
    assert!(stdout.contains("No MCP servers configured yet."));

    Ok(())
}

#[test]
fn api_key_auth_does_not_enable_host_curated_plugin_mcp_servers() -> Result<()> {
    let codex_home = TempDir::new()?;
    std::fs::write(
        codex_home.path().join("config.toml"),
        r#"[features]
plugins = true
remote_plugin = false

[plugins."api-docs@openai-api-curated"]
enabled = true
"#,
    )?;
    let plugin_root = codex_home
        .path()
        .join("plugins/cache/openai-api-curated/api-docs/local");
    std::fs::create_dir_all(plugin_root.join(".codex-plugin"))?;
    std::fs::write(
        plugin_root.join(".codex-plugin/plugin.json"),
        r#"{"name":"api-docs","version":"local"}"#,
    )?;
    std::fs::write(
        plugin_root.join(".mcp.json"),
        r#"{
  "mcpServers": {
    "api-docs": {
      "type": "stdio",
      "command": "api-docs-mcp"
    }
  }
}"#,
    )?;

    let output = codex_command(codex_home.path())?
        .env(CODEX_API_KEY_ENV_VAR, "sk-test")
        .args(["mcp", "list", "--json"])
        .output()?;
    assert!(
        output.status.success(),
        "mcp list failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let entries: JsonValue = serde_json::from_slice(&output.stdout)?;
    assert!(entries.as_array().is_some_and(Vec::is_empty));

    Ok(())
}

#[tokio::test]
async fn list_uses_offline_oauth_status_without_discovery() -> Result<()> {
    let codex_home = TempDir::new()?;
    let server = MockServer::start().await;
    configure_http_oauth_server(codex_home.path(), &format!("{}/mcp", server.uri())).await?;
    std::fs::write(codex_home.path().join("environments.toml"), "invalid = [")?;

    let mut command = codex_command(codex_home.path())?;
    command
        .env("HTTP_PROXY", server.uri())
        .env("http_proxy", server.uri())
        .env("HTTPS_PROXY", server.uri())
        .env("https_proxy", server.uri())
        .env("ALL_PROXY", server.uri())
        .env("all_proxy", server.uri())
        .env_remove("NO_PROXY")
        .env_remove("no_proxy")
        .args([
            "-c",
            "mcp_oauth_credentials_store=\"file\"",
            "mcp",
            "list",
            "--json",
        ]);
    let output = command.output()?;
    assert!(
        output.status.success(),
        "mcp list failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let entries: JsonValue = serde_json::from_slice(&output.stdout)?;
    assert_eq!(entries[0]["name"], "oauth");
    assert!(entries[0]["auth_status"].is_string());
    assert!(
        server
            .received_requests()
            .await
            .expect("mock OAuth origin should record requests")
            .is_empty(),
        "mcp list must not discover OAuth metadata"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_uses_offline_status_when_oauth_origin_would_rate_limit() -> Result<()> {
    let codex_home = TempDir::new()?;
    let server = MockServer::start().await;
    configure_http_oauth_server(codex_home.path(), &format!("{}/mcp", server.uri())).await?;

    let output = codex_command(codex_home.path())?
        .env("NO_PROXY", "127.0.0.1,localhost")
        .env("no_proxy", "127.0.0.1,localhost")
        .args([
            "-c",
            "mcp_oauth_credentials_store=\"file\"",
            "mcp",
            "list",
            "--json",
        ])
        .output()?;
    assert!(
        output.status.success(),
        "mcp list failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let entries: JsonValue = serde_json::from_slice(&output.stdout)?;
    assert_eq!(entries[0]["name"], "oauth");
    assert!(entries[0]["auth_status"].is_string());
    assert!(
        server
            .received_requests()
            .await
            .expect("mock rate-limited origin should record requests")
            .is_empty(),
        "mcp list must not contact a rate-limited OAuth origin"
    );

    Ok(())
}

#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_with_macos_proxy_resolution_does_not_panic() -> Result<()> {
    let codex_home = TempDir::new()?;
    let server = MockServer::start().await;
    configure_http_oauth_server(codex_home.path(), &format!("{}/mcp", server.uri())).await?;

    for respect_system_proxy in [false, true] {
        let system_proxy_override = format!("features.respect_system_proxy={respect_system_proxy}");
        let mut command = codex_command(codex_home.path())?;
        command
            .env_remove("HTTP_PROXY")
            .env_remove("http_proxy")
            .env_remove("HTTPS_PROXY")
            .env_remove("https_proxy")
            .env_remove("ALL_PROXY")
            .env_remove("all_proxy")
            .env_remove("NO_PROXY")
            .env_remove("no_proxy")
            .args([
                "-c",
                &system_proxy_override,
                "-c",
                "mcp_oauth_credentials_store=\"file\"",
                "mcp",
                "list",
                "--json",
            ]);
        let output = command.output()?;
        assert!(
            output.status.success(),
            "macOS proxy resolution should not panic with respect_system_proxy={respect_system_proxy}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let entries: JsonValue = serde_json::from_slice(&output.stdout)?;
        assert_eq!(entries[0]["name"], "oauth");
    }
    assert!(
        server
            .received_requests()
            .await
            .expect("mock OAuth origin should record requests")
            .is_empty(),
        "mcp list must remain offline while resolving macOS proxy settings"
    );
    Ok(())
}

#[tokio::test]
async fn list_and_get_render_expected_output() -> Result<()> {
    let codex_home = TempDir::new()?;

    let mut add = codex_command(codex_home.path())?;
    add.args([
        "mcp",
        "add",
        "docs",
        "--env",
        "TOKEN=secret",
        "--",
        "docs-server",
        "--port",
        "4000",
    ])
    .assert()
    .success();

    let mut servers = load_global_mcp_servers(codex_home.path()).await?;
    let docs_entry = servers
        .get_mut("docs")
        .expect("docs server should exist after add");
    match &mut docs_entry.transport {
        McpServerTransportConfig::Stdio { env_vars, .. } => {
            *env_vars = vec!["APP_TOKEN".into(), "WORKSPACE_ID".into()];
        }
        other => panic!("unexpected transport: {other:?}"),
    }
    ConfigEditsBuilder::new(codex_home.path())
        .replace_mcp_servers(&servers)
        .apply_blocking()?;

    let mut list_cmd = codex_command(codex_home.path())?;
    let list_output = list_cmd.args(["mcp", "list"]).output()?;
    assert!(list_output.status.success());
    let stdout = String::from_utf8(list_output.stdout)?;
    assert!(stdout.contains("Name"));
    assert!(stdout.contains("docs"));
    assert!(stdout.contains("docs-server"));
    assert!(stdout.contains("TOKEN=*****"));
    assert!(stdout.contains("APP_TOKEN=*****"));
    assert!(stdout.contains("WORKSPACE_ID=*****"));
    assert!(stdout.contains("Status"));
    assert!(stdout.contains("Auth"));
    assert!(stdout.contains("enabled"));
    assert!(stdout.contains("Unsupported"));

    let mut list_json_cmd = codex_command(codex_home.path())?;
    let json_output = list_json_cmd.args(["mcp", "list", "--json"]).output()?;
    assert!(json_output.status.success());
    let stdout = String::from_utf8(json_output.stdout)?;
    let parsed: JsonValue = serde_json::from_str(&stdout)?;
    assert_eq!(
        parsed,
        json!([
          {
            "name": "docs",
            "enabled": true,
            "disabled_reason": null,
            "transport": {
              "type": "stdio",
              "command": "docs-server",
              "args": [
                "--port",
                "4000"
              ],
              "env": {
                "TOKEN": "secret"
              },
              "env_vars": [
                "APP_TOKEN",
                "WORKSPACE_ID"
              ],
              "cwd": null
            },
            "startup_timeout_sec": null,
            "tool_timeout_sec": null,
            "auth_status": "unsupported",
            "shadowed_declarations": []
          }
        ]
        )
    );

    let mut get_cmd = codex_command(codex_home.path())?;
    let get_output = get_cmd.args(["mcp", "get", "docs"]).output()?;
    assert!(get_output.status.success());
    let stdout = String::from_utf8(get_output.stdout)?;
    assert!(stdout.contains("docs"));
    assert!(stdout.contains("transport: stdio"));
    assert!(stdout.contains("command: docs-server"));
    assert!(stdout.contains("args: --port 4000"));
    assert!(stdout.contains("env: TOKEN=*****"));
    assert!(stdout.contains("APP_TOKEN=*****"));
    assert!(stdout.contains("WORKSPACE_ID=*****"));
    assert!(stdout.contains("enabled: true"));
    assert!(stdout.contains("remove: whisply mcp remove docs"));

    let mut get_json_cmd = codex_command(codex_home.path())?;
    get_json_cmd
        .args(["mcp", "get", "docs", "--json"])
        .assert()
        .success()
        .stdout(contains("\"name\": \"docs\"").and(contains("\"enabled\": true")));

    Ok(())
}

#[tokio::test]
async fn get_disabled_server_shows_single_line() -> Result<()> {
    let codex_home = TempDir::new()?;

    let mut add = codex_command(codex_home.path())?;
    add.args(["mcp", "add", "docs", "--", "docs-server"])
        .assert()
        .success();

    let mut servers = load_global_mcp_servers(codex_home.path()).await?;
    let docs = servers
        .get_mut("docs")
        .expect("docs server should exist after add");
    docs.enabled = false;
    ConfigEditsBuilder::new(codex_home.path())
        .replace_mcp_servers(&servers)
        .apply_blocking()?;

    let mut get_cmd = codex_command(codex_home.path())?;
    let get_output = get_cmd.args(["mcp", "get", "docs"]).output()?;
    assert!(get_output.status.success());
    let stdout = String::from_utf8(get_output.stdout)?;
    assert_eq!(stdout.trim_end(), "docs (disabled)");

    Ok(())
}

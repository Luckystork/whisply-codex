use std::path::Path;

use anyhow::Result;
use anyhow::ensure;
use codex_core::config::load_global_mcp_servers;
use pretty_assertions::assert_eq;
use serde_json::Value;
use tempfile::TempDir;
use tokio::process::Command;
use wiremock::MockServer;

fn isolated_whisply_command(codex_home: &Path, args: &[&str]) -> Result<Command> {
    let mut command = Command::new(codex_utils_cargo_bin::cargo_bin("whisply")?);
    command
        .kill_on_drop(true)
        .current_dir(codex_home)
        .env("CODEX_HOME", codex_home)
        .env("WHISPLY_HOME", codex_home)
        .env_remove("CODEX_ACCESS_TOKEN")
        .env_remove("CODEX_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .args(args);
    Ok(command)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_mcp_commands_use_only_local_configuration() -> Result<()> {
    let codex_home = TempDir::new()?;

    let output = isolated_whisply_command(
        codex_home.path(),
        &["mcp", "add", "local-docs", "--", "echo", "hello"],
    )?
    .output()
    .await?;
    ensure!(
        output.status.success(),
        "mcp add failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let output = isolated_whisply_command(codex_home.path(), &["mcp", "list", "--json"])?
        .output()
        .await?;
    ensure!(
        output.status.success(),
        "mcp list failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let entries: Vec<Value> = serde_json::from_slice(&output.stdout)?;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["name"], "local-docs");
    assert_eq!(entries[0]["transport"]["type"], "stdio");
    assert_eq!(entries[0]["transport"]["command"], "echo");
    assert_eq!(
        entries[0]["transport"]["args"],
        serde_json::json!(["hello"])
    );

    let output =
        isolated_whisply_command(codex_home.path(), &["mcp", "get", "local-docs", "--json"])?
            .output()
            .await?;
    ensure!(
        output.status.success(),
        "mcp get failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let entry: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(entry["name"], "local-docs");
    assert_eq!(entry["transport"]["command"], "echo");

    assert!(
        load_global_mcp_servers(codex_home.path())
            .await?
            .contains_key("local-docs")
    );
    assert!(
        !codex_home
            .path()
            .join("cloud-config-bundle-cache.json")
            .exists(),
        "local MCP commands must not create a cloud-config cache"
    );

    let output = isolated_whisply_command(codex_home.path(), &["mcp", "remove", "local-docs"])?
        .output()
        .await?;
    ensure!(
        output.status.success(),
        "mcp remove failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(load_global_mcp_servers(codex_home.path()).await?.is_empty());

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn login_rejects_host_owned_plugin_alias_before_network() -> Result<()> {
    let server = MockServer::start().await;
    let codex_home = TempDir::new()?;
    std::fs::write(
        codex_home.path().join("config.toml"),
        r#"[features]
plugins = true

[plugins."legacy-host-owned-plugin@test"]
enabled = true
"#,
    )?;
    let plugin_root = codex_home
        .path()
        .join("plugins/cache/test/legacy-host-owned-plugin/local");
    std::fs::create_dir_all(plugin_root.join(".codex-plugin"))?;
    std::fs::write(
        plugin_root.join(".codex-plugin/plugin.json"),
        r#"{"name":"legacy-host-owned-plugin","version":"local"}"#,
    )?;
    std::fs::write(
        plugin_root.join(".mcp.json"),
        format!(
            r#"{{
  "mcpServers": {{
    "legacy-host-owned-alias": {{
      "url": "{}/host-owned/mcp",
      "auth": "chatgpt"
    }}
  }}
}}"#,
            server.uri()
        ),
    )?;

    let list_output = isolated_whisply_command(codex_home.path(), &["mcp", "list", "--json"])?
        .output()
        .await?;
    ensure!(
        list_output.status.success(),
        "mcp list must continue to work while filtering rejected MCP servers"
    );
    assert!(!String::from_utf8_lossy(&list_output.stdout).contains("legacy-host-owned-alias"));

    let get_output = isolated_whisply_command(
        codex_home.path(),
        &["mcp", "get", "legacy-host-owned-alias", "--json"],
    )?
    .output()
    .await?;
    ensure!(
        !get_output.status.success(),
        "mcp get must not expose a rejected MCP alias"
    );
    assert!(String::from_utf8_lossy(&get_output.stderr).contains("No MCP server named"));

    let output = isolated_whisply_command(
        codex_home.path(),
        &["mcp", "login", "legacy-host-owned-alias"],
    )?
    .output()
    .await?;
    ensure!(
        !output.status.success(),
        "host-owned plugin MCP aliases must not enter OAuth login"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains(
        "Whisply does not permit OAuth login for host-owned or ChatGPT-authenticated MCP servers."
    ));
    assert!(
        server
            .received_requests()
            .await
            .unwrap_or_default()
            .is_empty(),
        "mcp login must reject the plugin alias before MCP or OAuth HTTP discovery"
    );

    Ok(())
}

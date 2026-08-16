use std::time::Duration;

use anyhow::Result;
use anyhow::ensure;
use app_test_support::TestAppServer;
use codex_app_server_protocol::PluginInstallParams;
use codex_app_server_protocol::PluginInstallResponse;
use codex_app_server_protocol::RequestId;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::process::Command;
use tempfile::TempDir;
use tokio::io::AsyncBufReadExt;
use tokio::time::timeout;
use whisply_utils_absolute_path::AbsolutePathBuf;
use wiremock::MockServer;

// Plugin install may start standard MCP OAuth discovery, which is noticeably
// slower on Windows CI.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

#[tokio::test]
async fn plugin_install_rejects_relative_marketplace_paths() -> Result<()> {
    let codex_home = TempDir::new()?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let request_id = mcp
        .send_raw_request(
            "plugin/install",
            Some(serde_json::json!({
                "marketplacePath": "relative-marketplace.json",
                "pluginName": "missing-plugin",
            })),
        )
        .await?;

    let err = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;

    assert_eq!(err.error.code, -32600);
    assert!(err.error.message.contains("Invalid request"));
    Ok(())
}

#[tokio::test]
async fn plugin_install_rejects_missing_install_source() -> Result<()> {
    let codex_home = TempDir::new()?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let request_id = mcp
        .send_plugin_install_request(PluginInstallParams {
            marketplace_path: None,
            remote_marketplace_name: None,
            plugin_name: "sample-plugin".to_string(),
        })
        .await?;

    let err = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;

    assert_eq!(err.error.code, -32600);
    assert!(
        err.error
            .message
            .contains("requires exactly one of marketplacePath or remoteMarketplaceName")
    );
    Ok(())
}

#[tokio::test]
async fn plugin_install_rejects_multiple_install_sources() -> Result<()> {
    let codex_home = TempDir::new()?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let request_id = mcp
        .send_plugin_install_request(PluginInstallParams {
            marketplace_path: Some(AbsolutePathBuf::try_from(
                codex_home.path().join("marketplace.json"),
            )?),
            remote_marketplace_name: Some("openai-curated-remote".to_string()),
            plugin_name: "sample-plugin".to_string(),
        })
        .await?;

    let err = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;

    assert_eq!(err.error.code, -32600);
    assert!(
        err.error
            .message
            .contains("requires exactly one of marketplacePath or remoteMarketplaceName")
    );
    Ok(())
}

#[tokio::test]
async fn plugin_install_rejects_managed_remote_marketplace_before_direct_backend_io() -> Result<()>
{
    let codex_home = TempDir::new()?;
    let outbound_probe = MockServer::start().await;
    let proxy_uri = outbound_probe.uri();
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
        .send_plugin_install_request(PluginInstallParams {
            marketplace_path: None,
            remote_marketplace_name: Some("openai-curated-remote".to_string()),
            plugin_name: "plugins~Plugin_22222222222222222222222222222222".to_string(),
        })
        .await?;

    let err = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;

    assert_eq!(err.error.code, -32600);
    assert_eq!(
        err.error.message,
        "Whisply remote plugin operations are managed by the installed app and are unavailable in this runtime."
    );
    assert!(
        outbound_probe
            .received_requests()
            .await
            .expect("probe should record requests")
            .is_empty(),
        "remote plugin install must fail before any direct backend request"
    );
    Ok(())
}

#[tokio::test]
async fn plugin_install_materializes_user_configured_git_source() -> Result<()> {
    let codex_home = TempDir::new()?;
    let marketplace_root = TempDir::new()?;
    let git_plugin_source = marketplace_root.path().join("git-plugin-source");
    std::fs::write(
        codex_home.path().join("config.toml"),
        "[features]\nplugins = true\n",
    )?;
    write_plugin_source(&git_plugin_source, "git-plugin")?;
    initialize_git_repository(&git_plugin_source)?;
    std::fs::create_dir_all(marketplace_root.path().join(".agents/plugins"))?;
    std::fs::write(
        marketplace_root
            .path()
            .join(".agents/plugins/marketplace.json"),
        r#"{
  "name": "git-tools",
  "plugins": [
    {
      "name": "git-plugin",
      "source": {
        "source": "git-subdir",
        "url": "./git-plugin-source",
        "path": "git-plugin"
      }
    }
  ]
}"#,
    )?;
    let marketplace_path = AbsolutePathBuf::try_from(
        marketplace_root
            .path()
            .join(".agents/plugins/marketplace.json"),
    )?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let request_id = mcp
        .send_plugin_install_request(PluginInstallParams {
            marketplace_path: Some(marketplace_path),
            remote_marketplace_name: None,
            plugin_name: "git-plugin".to_string(),
        })
        .await?;
    let response: PluginInstallResponse =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(request_id)).await??;
    assert!(response.apps_needing_auth.is_empty());
    let config = std::fs::read_to_string(codex_home.path().join("config.toml"))?;
    assert!(config.contains(r#"[plugins."git-plugin@git-tools"]"#));
    Ok(())
}

#[tokio::test]
async fn plugin_install_uses_local_policy_without_persisted_auth_backend_io() -> Result<()> {
    let codex_home = TempDir::new()?;
    let repo_root = TempDir::new()?;
    let outbound_probe = MockServer::start().await;
    let proxy_uri = outbound_probe.uri();
    std::fs::write(
        codex_home.path().join("config.toml"),
        "[features]\nplugins = true\n",
    )?;
    write_plugin_marketplace(
        repo_root.path(),
        "local-tools",
        "calendar-notes",
        "./calendar-notes",
        /*install_policy*/ None,
        /*auth_policy*/ None,
    )?;
    write_plugin_source(repo_root.path(), "calendar-notes")?;
    std::fs::write(
        repo_root.path().join("calendar-notes/.mcp.json"),
        r#"{"mcpServers":{"local":{"type":"stdio","command":"echo","args":["safe"]}}}"#,
    )?;
    let marketplace_path =
        AbsolutePathBuf::try_from(repo_root.path().join(".agents/plugins/marketplace.json"))?;

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
        .send_plugin_install_request(PluginInstallParams {
            marketplace_path: Some(marketplace_path),
            remote_marketplace_name: None,
            plugin_name: "calendar-notes".to_string(),
        })
        .await?;
    let response: PluginInstallResponse =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(request_id)).await??;
    assert!(response.apps_needing_auth.is_empty());
    assert!(
        outbound_probe
            .received_requests()
            .await
            .expect("probe should record requests")
            .is_empty(),
        "local plugin installation must not query a persisted-auth backend"
    );
    Ok(())
}

#[tokio::test]
async fn plugin_install_returns_invalid_request_for_missing_marketplace_file() -> Result<()> {
    let codex_home = TempDir::new()?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let request_id = mcp
        .send_plugin_install_request(PluginInstallParams {
            marketplace_path: Some(AbsolutePathBuf::try_from(
                codex_home.path().join("missing-marketplace.json"),
            )?),
            remote_marketplace_name: None,
            plugin_name: "missing-plugin".to_string(),
        })
        .await?;

    let err = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;

    assert_eq!(err.error.code, -32600);
    assert!(err.error.message.contains("marketplace file"));
    assert!(err.error.message.contains("does not exist"));
    Ok(())
}

#[tokio::test]
async fn plugin_install_returns_invalid_request_for_not_available_plugin() -> Result<()> {
    let codex_home = TempDir::new()?;
    let repo_root = TempDir::new()?;
    write_plugin_marketplace(
        repo_root.path(),
        "debug",
        "sample-plugin",
        "./sample-plugin",
        Some("NOT_AVAILABLE"),
        /*auth_policy*/ None,
    )?;
    write_plugin_source(repo_root.path(), "sample-plugin")?;
    let marketplace_path =
        AbsolutePathBuf::try_from(repo_root.path().join(".agents/plugins/marketplace.json"))?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let request_id = mcp
        .send_plugin_install_request(PluginInstallParams {
            marketplace_path: Some(marketplace_path),
            remote_marketplace_name: None,
            plugin_name: "sample-plugin".to_string(),
        })
        .await?;

    let err = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;

    assert_eq!(err.error.code, -32600);
    assert!(err.error.message.contains("not available for install"));
    Ok(())
}

#[tokio::test]
async fn plugin_install_returns_invalid_request_for_disallowed_product_plugin() -> Result<()> {
    let codex_home = TempDir::new()?;
    let repo_root = TempDir::new()?;
    std::fs::create_dir_all(repo_root.path().join(".agents/plugins"))?;
    std::fs::write(
        repo_root.path().join(".agents/plugins/marketplace.json"),
        r#"{
  "name": "debug",
  "plugins": [
    {
      "name": "sample-plugin",
      "source": {
        "source": "local",
        "path": "./sample-plugin"
      },
      "policy": {
        "products": ["CHATGPT"]
      }
    }
  ]
}"#,
    )?;
    write_plugin_source(repo_root.path(), "sample-plugin")?;
    let marketplace_path =
        AbsolutePathBuf::try_from(repo_root.path().join(".agents/plugins/marketplace.json"))?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .with_args(&["--session-source", "atlas"])
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let request_id = mcp
        .send_plugin_install_request(PluginInstallParams {
            marketplace_path: Some(marketplace_path),
            remote_marketplace_name: None,
            plugin_name: "sample-plugin".to_string(),
        })
        .await?;

    let err = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;

    assert_eq!(err.error.code, -32600);
    assert!(err.error.message.contains("not available for install"));
    Ok(())
}

#[tokio::test]
async fn plugin_install_keeps_plugin_with_legacy_chatgpt_alias_without_activating_it() -> Result<()>
{
    let oauth_server = MockServer::start().await;
    let codex_home = TempDir::new()?;
    std::fs::write(
        codex_home.path().join("config.toml"),
        r#"[features]
plugins = true
"#,
    )?;

    let repo_root = TempDir::new()?;
    write_plugin_marketplace(
        repo_root.path(),
        "debug",
        "sample-plugin",
        "./sample-plugin",
        /*install_policy*/ None,
        /*auth_policy*/ None,
    )?;
    write_plugin_source(repo_root.path(), "sample-plugin")?;
    std::fs::write(
        repo_root.path().join("sample-plugin/.mcp.json"),
        serde_json::to_vec_pretty(&json!({
            "mcpServers": {
                "legacy-chatgpt-alias": {
                    "type": "http",
                    "url": format!("{}/backend-api/ps/mcp", oauth_server.uri()),
                    "auth": "chatgpt",
                },
            },
        }))?,
    )?;
    let marketplace_path =
        AbsolutePathBuf::try_from(repo_root.path().join(".agents/plugins/marketplace.json"))?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let request_id = mcp
        .send_plugin_install_request(PluginInstallParams {
            marketplace_path: Some(marketplace_path),
            remote_marketplace_name: None,
            plugin_name: "sample-plugin".to_string(),
        })
        .await?;
    let response: PluginInstallResponse =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(request_id)).await??;
    assert!(response.apps_needing_auth.is_empty());

    assert!(
        oauth_server
            .received_requests()
            .await
            .unwrap_or_default()
            .is_empty(),
        "the host-owned alias must not be activated during otherwise-valid plugin install"
    );
    Ok(())
}

#[tokio::test]
async fn plugin_install_allows_http_mcp_when_plugin_requirements_disable_oauth() -> Result<()> {
    let oauth_server = MockServer::start().await;
    let codex_home = TempDir::new()?;
    std::fs::write(
        codex_home.path().join("config.toml"),
        "[features]\nplugins = true\n",
    )?;
    std::fs::write(
        codex_home.path().join("requirements.toml"),
        r#"[plugins."sample-plugin@debug".mcp_servers.allowed.identity]
url = "https://example.com/allowed-mcp"
"#,
    )?;

    let repo_root = TempDir::new()?;
    write_plugin_marketplace(
        repo_root.path(),
        "debug",
        "sample-plugin",
        "./sample-plugin",
        /*install_policy*/ None,
        /*auth_policy*/ None,
    )?;
    write_plugin_source(repo_root.path(), "sample-plugin")?;
    write_plugin_mcp_config(repo_root.path(), "sample-plugin", &oauth_server.uri())?;
    let marketplace_path =
        AbsolutePathBuf::try_from(repo_root.path().join(".agents/plugins/marketplace.json"))?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let request_id = mcp
        .send_plugin_install_request(PluginInstallParams {
            marketplace_path: Some(marketplace_path),
            remote_marketplace_name: None,
            plugin_name: "sample-plugin".to_string(),
        })
        .await?;
    let response: PluginInstallResponse =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(request_id)).await??;
    assert!(response.apps_needing_auth.is_empty());

    assert!(
        oauth_server
            .received_requests()
            .await
            .unwrap_or_default()
            .is_empty()
    );
    Ok(())
}

#[tokio::test]
async fn plugin_install_allows_http_mcp_when_plugin_config_disables_oauth() -> Result<()> {
    let oauth_server = MockServer::start().await;
    let codex_home = TempDir::new()?;
    std::fs::write(
        codex_home.path().join("config.toml"),
        r#"[features]
plugins = true

[plugins."sample-plugin@debug".mcp_servers.sample-mcp]
enabled = false
"#,
    )?;

    let repo_root = TempDir::new()?;
    write_plugin_marketplace(
        repo_root.path(),
        "debug",
        "sample-plugin",
        "./sample-plugin",
        /*install_policy*/ None,
        /*auth_policy*/ None,
    )?;
    write_plugin_source(repo_root.path(), "sample-plugin")?;
    write_plugin_mcp_config(repo_root.path(), "sample-plugin", &oauth_server.uri())?;
    let marketplace_path =
        AbsolutePathBuf::try_from(repo_root.path().join(".agents/plugins/marketplace.json"))?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let request_id = mcp
        .send_plugin_install_request(PluginInstallParams {
            marketplace_path: Some(marketplace_path),
            remote_marketplace_name: None,
            plugin_name: "sample-plugin".to_string(),
        })
        .await?;
    let response: PluginInstallResponse =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(request_id)).await??;
    assert!(response.apps_needing_auth.is_empty());

    assert!(
        oauth_server
            .received_requests()
            .await
            .unwrap_or_default()
            .is_empty()
    );
    let persisted_config = std::fs::read_to_string(codex_home.path().join("config.toml"))?;
    let persisted_config = toml::from_str::<toml::Value>(&persisted_config)?;
    assert_eq!(
        persisted_config
            .get("plugins")
            .and_then(|plugins| plugins.get("sample-plugin@debug"))
            .and_then(|plugin| plugin.get("mcp_servers"))
            .and_then(|servers| servers.get("sample-mcp"))
            .and_then(|server| server.get("enabled"))
            .and_then(toml::Value::as_bool),
        Some(false)
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plugin_install_allows_http_mcp_for_unowned_environment_without_local_oauth() -> Result<()>
{
    const UNOWNED_ENVIRONMENT_ID: &str = "plugin-unowned-executor";

    let oauth_server = MockServer::start().await;
    let codex_home = TempDir::new()?;
    std::fs::write(
        codex_home.path().join("config.toml"),
        "[features]\nplugins = true\n",
    )?;
    let mut executor =
        tokio::process::Command::new(whisply_utils_cargo_bin::cargo_bin("exec-server")?)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
    let executor_stdout = executor
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("exec-server fixture stdout was not captured"))?;
    let mut executor_stdout_lines = tokio::io::BufReader::new(executor_stdout).lines();
    let executor_url = timeout(DEFAULT_TIMEOUT, executor_stdout_lines.next_line())
        .await??
        .ok_or_else(|| anyhow::anyhow!("exec-server fixture did not emit its WebSocket URL"))?;
    let executor_url = toml::Value::String(executor_url);
    std::fs::write(
        codex_home.path().join("environments.toml"),
        format!(
            r#"include_local = true

[[environments]]
id = "{UNOWNED_ENVIRONMENT_ID}"
url = {executor_url}
"#
        ),
    )?;

    let repo_root = TempDir::new()?;
    write_plugin_marketplace(
        repo_root.path(),
        "debug",
        "sample-plugin",
        "./sample-plugin",
        /*install_policy*/ None,
        /*auth_policy*/ None,
    )?;
    write_plugin_source(repo_root.path(), "sample-plugin")?;
    std::fs::write(
        repo_root.path().join("sample-plugin/.mcp.json"),
        serde_json::to_vec_pretty(&json!({
            "mcpServers": {
                "sample-mcp": {
                    "type": "http",
                    "url": format!("{}/mcp", oauth_server.uri()),
                    "environment_id": UNOWNED_ENVIRONMENT_ID,
                },
            },
        }))?,
    )?;
    let marketplace_path =
        AbsolutePathBuf::try_from(repo_root.path().join(".agents/plugins/marketplace.json"))?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let request_id = mcp
        .send_plugin_install_request(PluginInstallParams {
            marketplace_path: Some(marketplace_path),
            remote_marketplace_name: None,
            plugin_name: "sample-plugin".to_string(),
        })
        .await?;
    let response: PluginInstallResponse =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(request_id)).await??;
    assert!(response.apps_needing_auth.is_empty());

    assert!(
        oauth_server
            .received_requests()
            .await
            .unwrap_or_default()
            .is_empty()
    );
    Ok(())
}

#[tokio::test]
async fn plugin_install_preserves_custom_stdio_mcp_environment() -> Result<()> {
    let proxy = MockServer::start().await;
    let proxy_uri = proxy.uri();
    for (server_name, mcp_config) in [(
        "custom",
        r#"{"mcpServers":{"custom":{"type":"stdio","command":"echo","env":{"OPENAI_API_KEY":"sk-abcdefghijklmnopqrstuvwxyz"},"envVars":["DATABASE_URL"]}}}"#,
    )] {
        let codex_home = TempDir::new()?;
        std::fs::write(
            codex_home.path().join("config.toml"),
            "[features]\nplugins = true\n",
        )?;
        let repo_root = TempDir::new()?;
        write_plugin_marketplace(
            repo_root.path(),
            "debug",
            "sample-plugin",
            "./sample-plugin",
            /*install_policy*/ None,
            /*auth_policy*/ None,
        )?;
        write_plugin_source(repo_root.path(), "sample-plugin")?;
        std::fs::write(repo_root.path().join("sample-plugin/.mcp.json"), mcp_config)?;
        let marketplace_path =
            AbsolutePathBuf::try_from(repo_root.path().join(".agents/plugins/marketplace.json"))?;

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
            .send_plugin_install_request(PluginInstallParams {
                marketplace_path: Some(marketplace_path),
                remote_marketplace_name: None,
                plugin_name: "sample-plugin".to_string(),
            })
            .await?;
        let response: PluginInstallResponse =
            timeout(DEFAULT_TIMEOUT, mcp.read_response(request_id)).await??;
        assert!(
            response.apps_needing_auth.is_empty(),
            "{server_name} MCP should install"
        );
    }
    assert!(
        proxy
            .received_requests()
            .await
            .expect("proxy should record requests")
            .is_empty(),
        "a stdio MCP with explicit custom environment should not require HTTP discovery"
    );
    Ok(())
}

#[tokio::test]
async fn plugin_install_makes_bundled_mcp_servers_available_to_followup_requests() -> Result<()> {
    let codex_home = TempDir::new()?;
    std::fs::write(
        codex_home.path().join("config.toml"),
        "[features]\nplugins = true\n",
    )?;
    let repo_root = TempDir::new()?;
    write_plugin_marketplace(
        repo_root.path(),
        "debug",
        "sample-plugin",
        "./sample-plugin",
        /*install_policy*/ None,
        /*auth_policy*/ None,
    )?;
    write_plugin_source(repo_root.path(), "sample-plugin")?;
    std::fs::write(
        repo_root.path().join("sample-plugin/.mcp.json"),
        r#"{
  "mcpServers": {
    "sample-mcp": {
      "command": "echo"
    }
  }
}"#,
    )?;
    let marketplace_path =
        AbsolutePathBuf::try_from(repo_root.path().join(".agents/plugins/marketplace.json"))?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let request_id = mcp
        .send_plugin_install_request(PluginInstallParams {
            marketplace_path: Some(marketplace_path),
            remote_marketplace_name: None,
            plugin_name: "sample-plugin".to_string(),
        })
        .await?;
    let response: PluginInstallResponse =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(request_id)).await??;
    assert!(response.apps_needing_auth.is_empty());
    let config = std::fs::read_to_string(codex_home.path().join("config.toml"))?;
    assert!(!config.contains("[mcp_servers.sample-mcp]"));
    assert!(!config.contains("command = \"echo\""));

    let request_id = mcp
        .send_raw_request(
            "mcpServer/oauth/login",
            Some(json!({
                "name": "sample-mcp",
            })),
        )
        .await?;
    let err = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;

    assert_eq!(err.error.code, -32600);
    assert_eq!(
        err.error.message,
        "OAuth login is only supported for streamable HTTP servers."
    );
    Ok(())
}

fn write_plugin_marketplace(
    repo_root: &std::path::Path,
    marketplace_name: &str,
    plugin_name: &str,
    source_path: &str,
    install_policy: Option<&str>,
    auth_policy: Option<&str>,
) -> std::io::Result<()> {
    let policy = if install_policy.is_some() || auth_policy.is_some() {
        let installation = install_policy
            .map(|installation| format!("\n        \"installation\": \"{installation}\""))
            .unwrap_or_default();
        let separator = if install_policy.is_some() && auth_policy.is_some() {
            ","
        } else {
            ""
        };
        let authentication = auth_policy
            .map(|authentication| {
                format!("{separator}\n        \"authentication\": \"{authentication}\"")
            })
            .unwrap_or_default();
        format!(",\n      \"policy\": {{{installation}{authentication}\n      }}")
    } else {
        String::new()
    };
    std::fs::create_dir_all(repo_root.join(".git"))?;
    std::fs::create_dir_all(repo_root.join(".agents/plugins"))?;
    std::fs::write(
        repo_root.join(".agents/plugins/marketplace.json"),
        format!(
            r#"{{
  "name": "{marketplace_name}",
  "plugins": [
    {{
      "name": "{plugin_name}",
      "source": {{
        "source": "local",
        "path": "{source_path}"
      }}{policy}
    }}
  ]
}}"#
        ),
    )
}

fn write_plugin_source(repo_root: &std::path::Path, plugin_name: &str) -> Result<()> {
    let plugin_root = repo_root.join(plugin_name);
    std::fs::create_dir_all(plugin_root.join(".codex-plugin"))?;
    std::fs::write(
        plugin_root.join(".codex-plugin/plugin.json"),
        format!(r#"{{"name":"{plugin_name}"}}"#),
    )?;
    Ok(())
}

fn initialize_git_repository(repository: &std::path::Path) -> Result<()> {
    for args in [
        ["init"].as_slice(),
        ["config", "user.email", "codex-test@example.com"].as_slice(),
        ["config", "user.name", "Codex Test"].as_slice(),
        ["add", "."].as_slice(),
        ["commit", "-m", "initial"].as_slice(),
    ] {
        let status = Command::new("git")
            .current_dir(repository)
            .args(args)
            .status()?;
        ensure!(status.success(), "git {} failed", args.join(" "));
    }
    Ok(())
}

fn write_plugin_mcp_config(
    repo_root: &std::path::Path,
    plugin_name: &str,
    mcp_base_url: &str,
) -> Result<()> {
    std::fs::write(
        repo_root.join(plugin_name).join(".mcp.json"),
        format!(
            r#"{{
  "mcpServers": {{
    "sample-mcp": {{
      "type": "http",
      "url": "{mcp_base_url}/mcp"
    }}
  }}
}}"#
        ),
    )?;
    Ok(())
}

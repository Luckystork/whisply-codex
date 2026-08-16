use std::time::Duration;

use anyhow::Result;
use app_test_support::TestAppServer;
use app_test_support::to_response;
use codex_app_server_protocol::HookEventName;
use codex_app_server_protocol::HookHandlerType;
use codex_app_server_protocol::JSONRPCError;
use codex_app_server_protocol::JSONRPCResponse;
use codex_app_server_protocol::PluginAuthPolicy;
use codex_app_server_protocol::PluginInstallPolicy;
use codex_app_server_protocol::PluginReadParams;
use codex_app_server_protocol::PluginReadResponse;
use codex_app_server_protocol::RequestId;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;
use tokio::time::timeout;
use whisply_utils_absolute_path::AbsolutePathBuf;
use wiremock::MockServer;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

#[tokio::test]
async fn plugin_read_rejects_missing_read_source() -> Result<()> {
    let codex_home = TempDir::new()?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let request_id = mcp
        .send_plugin_read_request(PluginReadParams {
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
async fn plugin_read_rejects_multiple_read_sources() -> Result<()> {
    let codex_home = TempDir::new()?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let request_id = mcp
        .send_plugin_read_request(PluginReadParams {
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
async fn plugin_read_and_skill_read_reject_managed_remote_catalog_before_direct_backend_io()
-> Result<()> {
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
        .send_plugin_read_request(PluginReadParams {
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

    let request_id = mcp
        .send_plugin_skill_read_request(codex_app_server_protocol::PluginSkillReadParams {
            remote_marketplace_name: "openai-curated-remote".to_string(),
            remote_plugin_id: "plugins~Plugin_22222222222222222222222222222222".to_string(),
            skill_name: "example".to_string(),
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
        "remote plugin reads must fail before any direct backend request"
    );
    Ok(())
}

#[tokio::test]
async fn plugin_read_returns_canonical_openai_curated_marketplace_name() -> Result<()> {
    let codex_home = TempDir::new()?;
    let repo_root = TempDir::new()?;
    write_plugin_marketplace(
        repo_root.path(),
        "openai-curated",
        "demo-plugin",
        "./demo-plugin",
    )?;
    std::fs::create_dir_all(repo_root.path().join("demo-plugin/.codex-plugin"))?;
    std::fs::write(
        repo_root
            .path()
            .join("demo-plugin/.codex-plugin/plugin.json"),
        r#"{
  "name": "demo-plugin",
  "description": "OpenAI curated plugin"
}"#,
    )?;
    std::fs::write(
        codex_home.path().join("config.toml"),
        r#"[features]
plugins = true

[plugins."demo-plugin@openai-curated"]
enabled = true
"#,
    )?;
    write_installed_plugin(&codex_home, "openai-curated", "demo-plugin")?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let marketplace_path =
        AbsolutePathBuf::try_from(repo_root.path().join(".agents/plugins/marketplace.json"))?;
    let request_id = mcp
        .send_plugin_read_request(PluginReadParams {
            marketplace_path: Some(marketplace_path.clone()),
            remote_marketplace_name: None,
            plugin_name: "demo-plugin".to_string(),
        })
        .await?;

    let response: JSONRPCResponse = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    assert_eq!(response.result["plugin"]["scheduledTasks"], json!(null));
    let response: PluginReadResponse = to_response(response)?;

    assert_eq!(response.plugin.marketplace_name, "openai-curated");
    assert_eq!(response.plugin.marketplace_path, Some(marketplace_path));
    assert_eq!(response.plugin.summary.id, "demo-plugin@openai-curated");
    assert_eq!(response.plugin.summary.name, "demo-plugin");
    Ok(())
}

#[tokio::test]
async fn plugin_read_falls_back_to_local_share_context_without_remote_auth() -> Result<()> {
    let codex_home = TempDir::new()?;
    let repo_root = TempDir::new()?;
    write_plugins_enabled_config(&codex_home)?;
    write_plugin_marketplace(
        repo_root.path(),
        "codex-curated",
        "demo-plugin",
        "./demo-plugin",
    )?;
    write_plugin_source(repo_root.path(), "demo-plugin")?;
    let plugin_path = AbsolutePathBuf::try_from(repo_root.path().join("demo-plugin"))?;
    write_plugin_share_local_path_mapping(codex_home.path(), "plugins_123", &plugin_path)?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let request_id = mcp
        .send_plugin_read_request(PluginReadParams {
            marketplace_path: Some(AbsolutePathBuf::try_from(
                repo_root.path().join(".agents/plugins/marketplace.json"),
            )?),
            remote_marketplace_name: None,
            plugin_name: "demo-plugin".to_string(),
        })
        .await?;

    let response: PluginReadResponse =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(request_id)).await??;

    assert_eq!(response.plugin.summary.remote_plugin_id, None);
    assert_eq!(response.plugin.summary.local_version, None);
    let share_context = response
        .plugin
        .summary
        .share_context
        .as_ref()
        .expect("expected share context");
    assert_eq!(share_context.remote_plugin_id, "plugins_123");
    assert_eq!(share_context.remote_version, None);
    assert_eq!(share_context.discoverability, None);
    assert_eq!(share_context.share_url, None);
    assert_eq!(share_context.creator_account_user_id, None);
    assert_eq!(share_context.creator_name, None);
    assert_eq!(share_context.share_principals, None);
    Ok(())
}

#[tokio::test]
async fn plugin_read_fails_on_malformed_share_mapping() -> Result<()> {
    let codex_home = TempDir::new()?;
    let repo_root = TempDir::new()?;
    write_plugins_enabled_config(&codex_home)?;
    write_plugin_marketplace(
        repo_root.path(),
        "codex-curated",
        "demo-plugin",
        "./demo-plugin",
    )?;
    write_plugin_source(repo_root.path(), "demo-plugin")?;
    std::fs::create_dir_all(codex_home.path().join(".tmp"))?;
    std::fs::write(
        codex_home
            .path()
            .join(".tmp/plugin-share-local-paths-v1.json"),
        "not valid json\n",
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let request_id = mcp
        .send_plugin_read_request(PluginReadParams {
            marketplace_path: Some(AbsolutePathBuf::try_from(
                repo_root.path().join(".agents/plugins/marketplace.json"),
            )?),
            remote_marketplace_name: None,
            plugin_name: "demo-plugin".to_string(),
        })
        .await?;

    let error: JSONRPCError = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;

    assert_eq!(error.error.code, -32603);
    assert!(
        error
            .error
            .message
            .contains("failed to load plugin share local path mapping")
    );
    Ok(())
}

#[tokio::test]
async fn plugin_read_returns_plugin_details_with_bundle_contents() -> Result<()> {
    let codex_home = TempDir::new()?;
    let repo_root = TempDir::new()?;
    let plugin_root = repo_root.path().join("plugins/demo-plugin");
    std::fs::create_dir_all(repo_root.path().join(".git"))?;
    std::fs::create_dir_all(repo_root.path().join(".agents/plugins"))?;
    std::fs::create_dir_all(plugin_root.join(".codex-plugin"))?;
    std::fs::create_dir_all(plugin_root.join("hooks"))?;
    std::fs::create_dir_all(plugin_root.join("skills/thread-summarizer"))?;
    std::fs::create_dir_all(plugin_root.join("skills/chatgpt-only"))?;
    std::fs::write(
        repo_root.path().join(".agents/plugins/marketplace.json"),
        r#"{
  "name": "codex-curated",
  "plugins": [
    {
      "name": "demo-plugin",
      "source": {
        "source": "local",
        "path": "./plugins/demo-plugin"
      },
      "policy": {
        "installation": "AVAILABLE",
        "authentication": "ON_INSTALL"
      },
      "category": "Design"
    }
  ]
}"#,
    )?;
    std::fs::write(
        plugin_root.join(".codex-plugin/plugin.json"),
        r##"{
  "name": "demo-plugin",
  "description": "Longer manifest description",
  "keywords": ["api-key", "developer tools"],
  "interface": {
    "displayName": "Plugin Display Name",
    "shortDescription": "Short description for subtitle",
    "longDescription": "Long description for details page",
    "developerName": "OpenAI",
    "category": "Productivity",
    "capabilities": ["Interactive", "Write"],
    "websiteURL": "https://openai.com/",
    "privacyPolicyURL": "https://openai.com/policies/row-privacy-policy/",
    "termsOfServiceURL": "https://openai.com/policies/row-terms-of-use/",
    "defaultPrompt": [
      "Draft the reply",
      "Find my next action"
    ],
    "brandColor": "#3B82F6",
    "composerIcon": "./assets/icon.png",
    "logo": "./assets/logo.png",
    "logoDark": "./assets/logo-dark.png",
    "screenshots": ["./assets/screenshot1.png"]
  }
}"##,
    )?;
    std::fs::write(
        plugin_root.join("skills/thread-summarizer/SKILL.md"),
        r#"---
name: thread-summarizer
description: Summarize email threads
---

# Thread Summarizer
"#,
    )?;
    std::fs::write(
        plugin_root.join("skills/chatgpt-only/SKILL.md"),
        r#"---
name: chatgpt-only
description: Visible only for ChatGPT
---

# ChatGPT Only
"#,
    )?;
    std::fs::create_dir_all(plugin_root.join("skills/thread-summarizer/agents"))?;
    std::fs::write(
        plugin_root.join("skills/thread-summarizer/agents/openai.yaml"),
        r#"policy:
  products:
    - CODEX
"#,
    )?;
    std::fs::create_dir_all(plugin_root.join("skills/chatgpt-only/agents"))?;
    std::fs::write(
        plugin_root.join("skills/chatgpt-only/agents/openai.yaml"),
        r#"policy:
  products:
    - CHATGPT
"#,
    )?;
    std::fs::write(
        plugin_root.join(".mcp.json"),
        r#"{
  "mcpServers": {
    "demo": {
      "command": "demo-server"
    }
  }
}"#,
    )?;
    std::fs::write(
        plugin_root.join("hooks/hooks.json"),
        r#"{
  "hooks": {
    "SessionStart": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "echo startup"
          }
        ]
      }
    ],
    "PreToolUse": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "echo first"
          },
          {
            "type": "command",
            "command": "echo second"
          }
        ]
      }
    ]
  }
}"#,
    )?;
    std::fs::write(
        codex_home.path().join("config.toml"),
        r#"[features]
plugins = true

[[skills.config]]
name = "demo-plugin:thread-summarizer"
enabled = false

[plugins."demo-plugin@codex-curated"]
enabled = true

[hooks.state."demo-plugin@codex-curated:hooks/hooks.json:pre_tool_use:0:0"]
enabled = false
"#,
    )?;
    write_installed_plugin(&codex_home, "codex-curated", "demo-plugin")?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let marketplace_path =
        AbsolutePathBuf::try_from(repo_root.path().join(".agents/plugins/marketplace.json"))?;
    let request_id = mcp
        .send_plugin_read_request(PluginReadParams {
            marketplace_path: Some(marketplace_path.clone()),
            remote_marketplace_name: None,
            plugin_name: "demo-plugin".to_string(),
        })
        .await?;

    let response: PluginReadResponse =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(request_id)).await??;

    assert_eq!(response.plugin.marketplace_name, "codex-curated");
    assert_eq!(response.plugin.marketplace_path, Some(marketplace_path));
    assert_eq!(response.plugin.summary.id, "demo-plugin@codex-curated");
    assert_eq!(response.plugin.summary.name, "demo-plugin");
    assert_eq!(
        response.plugin.description.as_deref(),
        Some("Longer manifest description")
    );
    assert_eq!(response.plugin.summary.installed, true);
    assert_eq!(response.plugin.summary.enabled, true);
    assert_eq!(
        response.plugin.summary.install_policy,
        PluginInstallPolicy::Available
    );
    assert_eq!(
        response.plugin.summary.auth_policy,
        PluginAuthPolicy::OnInstall
    );
    assert_eq!(
        response
            .plugin
            .summary
            .interface
            .as_ref()
            .and_then(|interface| interface.display_name.as_deref()),
        Some("Plugin Display Name")
    );
    assert_eq!(
        response
            .plugin
            .summary
            .interface
            .as_ref()
            .and_then(|interface| interface.category.as_deref()),
        Some("Design")
    );
    assert_eq!(
        response
            .plugin
            .summary
            .interface
            .as_ref()
            .and_then(|interface| interface.default_prompt.clone()),
        Some(vec![
            "Draft the reply".to_string(),
            "Find my next action".to_string()
        ])
    );
    assert_eq!(
        response
            .plugin
            .summary
            .interface
            .as_ref()
            .and_then(|interface| interface.logo_dark.as_ref()),
        Some(
            &AbsolutePathBuf::try_from(plugin_root.join("assets/logo-dark.png"))
                .expect("absolute dark logo path")
        )
    );
    assert_eq!(
        response.plugin.summary.keywords,
        vec!["api-key".to_string(), "developer tools".to_string()]
    );
    assert_eq!(response.plugin.skills.len(), 1);
    assert_eq!(
        response.plugin.skills[0].name,
        "demo-plugin:thread-summarizer"
    );
    assert_eq!(
        response.plugin.skills[0].description,
        "Summarize email threads"
    );
    assert!(!response.plugin.skills[0].enabled);
    assert_eq!(
        response.plugin.hooks,
        vec![
            codex_app_server_protocol::PluginHookSummary {
                key: "demo-plugin@codex-curated:hooks/hooks.json:pre_tool_use:0:0".to_string(),
                event_name: HookEventName::PreToolUse,
                handler_type: HookHandlerType::Command,
                command: Some("echo first".to_string()),
                matcher: None,
            },
            codex_app_server_protocol::PluginHookSummary {
                key: "demo-plugin@codex-curated:hooks/hooks.json:pre_tool_use:0:1".to_string(),
                event_name: HookEventName::PreToolUse,
                handler_type: HookHandlerType::Command,
                command: Some("echo second".to_string()),
                matcher: None,
            },
            codex_app_server_protocol::PluginHookSummary {
                key: "demo-plugin@codex-curated:hooks/hooks.json:session_start:0:0".to_string(),
                event_name: HookEventName::SessionStart,
                handler_type: HookHandlerType::Command,
                command: Some("echo startup".to_string()),
                matcher: None,
            },
        ]
    );
    assert_eq!(response.plugin.mcp_servers.len(), 1);
    assert_eq!(response.plugin.mcp_servers[0], "demo");
    Ok(())
}

#[tokio::test]
async fn plugin_read_accepts_legacy_string_default_prompt() -> Result<()> {
    let codex_home = TempDir::new()?;
    let repo_root = TempDir::new()?;
    let plugin_root = repo_root.path().join("plugins/demo-plugin");
    std::fs::create_dir_all(repo_root.path().join(".git"))?;
    std::fs::create_dir_all(repo_root.path().join(".agents/plugins"))?;
    std::fs::create_dir_all(plugin_root.join(".codex-plugin"))?;
    std::fs::write(
        repo_root.path().join(".agents/plugins/marketplace.json"),
        r#"{
  "name": "codex-curated",
  "plugins": [
    {
      "name": "demo-plugin",
      "source": {
        "source": "local",
        "path": "./plugins/demo-plugin"
      }
    }
  ]
}"#,
    )?;
    std::fs::write(
        plugin_root.join(".codex-plugin/plugin.json"),
        r##"{
  "name": "demo-plugin",
  "interface": {
    "defaultPrompt": "Starter prompt for trying a plugin"
  }
}"##,
    )?;
    write_plugins_enabled_config(&codex_home)?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let request_id = mcp
        .send_plugin_read_request(PluginReadParams {
            marketplace_path: Some(AbsolutePathBuf::try_from(
                repo_root.path().join(".agents/plugins/marketplace.json"),
            )?),
            remote_marketplace_name: None,
            plugin_name: "demo-plugin".to_string(),
        })
        .await?;

    let response: PluginReadResponse =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(request_id)).await??;

    assert_eq!(
        response
            .plugin
            .summary
            .interface
            .as_ref()
            .and_then(|interface| interface.default_prompt.clone()),
        Some(vec!["Starter prompt for trying a plugin".to_string()])
    );
    Ok(())
}

#[tokio::test]
async fn plugin_read_describes_uninstalled_git_source_without_cloning() -> Result<()> {
    let codex_home = TempDir::new()?;
    let repo_root = TempDir::new()?;
    let missing_remote_repo = repo_root.path().join("missing-remote-plugin-repo");
    let missing_remote_repo_url = url::Url::from_directory_path(&missing_remote_repo)
        .unwrap()
        .to_string();
    std::fs::create_dir_all(repo_root.path().join(".git"))?;
    std::fs::create_dir_all(repo_root.path().join(".agents/plugins"))?;
    std::fs::write(
        repo_root.path().join(".agents/plugins/marketplace.json"),
        format!(
            r#"{{
  "name": "debug",
  "plugins": [
    {{
      "name": "toolkit",
      "source": {{
        "source": "git-subdir",
        "url": "{missing_remote_repo_url}",
        "path": "plugins/toolkit"
      }}
    }}
  ]
}}"#
        ),
    )?;
    write_plugins_enabled_config(&codex_home)?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let request_id = mcp
        .send_plugin_read_request(PluginReadParams {
            marketplace_path: Some(AbsolutePathBuf::try_from(
                repo_root.path().join(".agents/plugins/marketplace.json"),
            )?),
            remote_marketplace_name: None,
            plugin_name: "toolkit".to_string(),
        })
        .await?;

    let response: PluginReadResponse =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(request_id)).await??;

    let expected_description = format!(
        "This is a cross-repo plugin. Install it to view more detailed information. The source of the plugin is {missing_remote_repo_url}, path `plugins/toolkit`."
    );
    assert_eq!(
        response.plugin.description.as_deref(),
        Some(expected_description.as_str())
    );
    assert!(!response.plugin.summary.installed);
    assert!(response.plugin.skills.is_empty());
    assert!(response.plugin.apps.is_empty());
    assert!(response.plugin.mcp_servers.is_empty());
    assert!(
        !codex_home
            .path()
            .join("plugins/.marketplace-plugin-source-staging")
            .exists()
    );
    Ok(())
}

#[tokio::test]
async fn plugin_read_returns_invalid_request_when_plugin_is_missing() -> Result<()> {
    let codex_home = TempDir::new()?;
    let repo_root = TempDir::new()?;
    std::fs::create_dir_all(repo_root.path().join(".git"))?;
    std::fs::create_dir_all(repo_root.path().join(".agents/plugins"))?;
    std::fs::write(
        repo_root.path().join(".agents/plugins/marketplace.json"),
        r#"{
  "name": "codex-curated",
  "plugins": [
    {
      "name": "demo-plugin",
      "source": {
        "source": "local",
        "path": "./plugins/demo-plugin"
      }
    }
  ]
}"#,
    )?;
    write_plugins_enabled_config(&codex_home)?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let request_id = mcp
        .send_plugin_read_request(PluginReadParams {
            marketplace_path: Some(AbsolutePathBuf::try_from(
                repo_root.path().join(".agents/plugins/marketplace.json"),
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
    assert!(
        err.error
            .message
            .contains("plugin `missing-plugin` was not found")
    );
    Ok(())
}

#[tokio::test]
async fn plugin_read_returns_invalid_request_when_plugin_manifest_is_missing() -> Result<()> {
    let codex_home = TempDir::new()?;
    let repo_root = TempDir::new()?;
    let plugin_root = repo_root.path().join("plugins/demo-plugin");
    std::fs::create_dir_all(repo_root.path().join(".git"))?;
    std::fs::create_dir_all(repo_root.path().join(".agents/plugins"))?;
    std::fs::create_dir_all(&plugin_root)?;
    std::fs::write(
        repo_root.path().join(".agents/plugins/marketplace.json"),
        r#"{
  "name": "codex-curated",
  "plugins": [
    {
      "name": "demo-plugin",
      "source": {
        "source": "local",
        "path": "./plugins/demo-plugin"
      }
    }
  ]
}"#,
    )?;
    write_plugins_enabled_config(&codex_home)?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let request_id = mcp
        .send_plugin_read_request(PluginReadParams {
            marketplace_path: Some(AbsolutePathBuf::try_from(
                repo_root.path().join(".agents/plugins/marketplace.json"),
            )?),
            remote_marketplace_name: None,
            plugin_name: "demo-plugin".to_string(),
        })
        .await?;

    let err = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;

    assert_eq!(err.error.code, -32600);
    assert!(err.error.message.contains("missing or invalid plugin.json"));
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
        .join("local/.codex-plugin");
    std::fs::create_dir_all(&plugin_root)?;
    std::fs::write(
        plugin_root.join("plugin.json"),
        format!(r#"{{"name":"{plugin_name}"}}"#),
    )?;
    Ok(())
}

fn write_plugins_enabled_config(codex_home: &TempDir) -> Result<()> {
    std::fs::write(
        codex_home.path().join("config.toml"),
        r#"[features]
plugins = true
"#,
    )?;
    Ok(())
}

fn write_plugin_marketplace(
    repo_root: &std::path::Path,
    marketplace_name: &str,
    plugin_name: &str,
    source_path: &str,
) -> std::io::Result<()> {
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
      }}
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

fn write_plugin_share_local_path_mapping(
    codex_home: &std::path::Path,
    remote_plugin_id: &str,
    plugin_path: &AbsolutePathBuf,
) -> std::io::Result<()> {
    let mut local_plugin_paths_by_remote_plugin_id = serde_json::Map::new();
    local_plugin_paths_by_remote_plugin_id.insert(
        remote_plugin_id.to_string(),
        serde_json::to_value(plugin_path).map_err(std::io::Error::other)?,
    );
    let contents = serde_json::to_string_pretty(&json!({
        "localPluginPathsByRemotePluginId": local_plugin_paths_by_remote_plugin_id,
    }))
    .map_err(std::io::Error::other)?;
    std::fs::create_dir_all(codex_home.join(".tmp"))?;
    std::fs::write(
        codex_home.join(".tmp/plugin-share-local-paths-v1.json"),
        format!("{contents}\n"),
    )
}

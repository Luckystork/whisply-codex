use std::time::Duration;

use anyhow::Result;
use app_test_support::TestAppServer;
use app_test_support::to_response;
use codex_app_server_protocol::JSONRPCResponse;
use codex_app_server_protocol::PluginAuthPolicy;
use codex_app_server_protocol::PluginInstallPolicy;
use codex_app_server_protocol::PluginInstalledParams;
use codex_app_server_protocol::PluginInstalledResponse;
use codex_app_server_protocol::PluginListMarketplaceKind;
use codex_app_server_protocol::PluginListParams;
use codex_app_server_protocol::PluginListResponse;
use codex_app_server_protocol::PluginMarketplaceEntry;
use codex_app_server_protocol::PluginSource;
use codex_app_server_protocol::PluginSummary;
use codex_app_server_protocol::RequestId;
use codex_config::PROJECT_CONFIG_DIRECTORY;
use codex_core::config::set_project_trust_level;
use codex_protocol::config_types::TrustLevel;
use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use tokio::time::timeout;
use wiremock::MockServer;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const TEST_CURATED_PLUGIN_SHA: &str = "0123456789abcdef0123456789abcdef01234567";
const WHISPLY_MANAGED_REMOTE_PLUGIN_MARKETPLACES_UNAVAILABLE_ERROR: &str = "Whisply remote plugin marketplaces are managed by the installed app and are unavailable in this runtime.";
const ALTERNATE_MARKETPLACE_RELATIVE_PATH: &str = ".claude-plugin/marketplace.json";
const ALTERNATE_PLUGIN_MANIFEST_RELATIVE_PATH: &str = ".claude-plugin/plugin.json";

fn write_plugins_enabled_config(codex_home: &std::path::Path) -> std::io::Result<()> {
    std::fs::write(
        codex_home.join("config.toml"),
        r#"[features]
plugins = true
"#,
    )
}

#[tokio::test]
async fn plugin_list_skips_invalid_marketplace_file_and_reports_error() -> Result<()> {
    let codex_home = TempDir::new()?;
    let repo_root = TempDir::new()?;
    std::fs::create_dir_all(repo_root.path().join(".git"))?;
    std::fs::create_dir_all(repo_root.path().join(".agents/plugins"))?;
    write_plugins_enabled_config(codex_home.path())?;
    let marketplace_path =
        AbsolutePathBuf::try_from(repo_root.path().join(".agents/plugins/marketplace.json"))?;
    std::fs::write(marketplace_path.as_path(), "{not json")?;

    let home = codex_home.path().to_string_lossy().into_owned();
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .with_env_overrides(&[
            ("HOME", Some(home.as_str())),
            ("USERPROFILE", Some(home.as_str())),
        ])
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let request_id = mcp
        .send_plugin_list_request(PluginListParams {
            cwds: Some(vec![AbsolutePathBuf::try_from(repo_root.path())?]),
            marketplace_kinds: None,
            force_refetch: false,
        })
        .await?;

    let response: PluginListResponse =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(request_id)).await??;

    assert!(
        response
            .marketplaces
            .iter()
            .all(|marketplace| { marketplace.path.as_ref() != Some(&marketplace_path) }),
        "invalid marketplace should be skipped"
    );
    assert_eq!(response.marketplace_load_errors.len(), 1);
    assert_eq!(
        response.marketplace_load_errors[0].marketplace_path,
        marketplace_path
    );
    assert!(
        response.marketplace_load_errors[0]
            .message
            .contains("invalid marketplace file"),
        "unexpected error: {:?}",
        response.marketplace_load_errors
    );
    Ok(())
}

#[tokio::test]
async fn plugin_installed_includes_installed_plugins_and_explicit_install_suggestions() -> Result<()>
{
    let codex_home = TempDir::new()?;
    write_openai_curated_marketplace(
        codex_home.path(),
        &["linear", "computer-use", "not-mentioned"],
    )?;
    write_installed_plugin(&codex_home, "openai-curated", "linear")?;
    std::fs::write(
        codex_home.path().join("config.toml"),
        r#"[features]
plugins = true

[plugins."linear@openai-curated"]
enabled = true
"#,
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let request_id = mcp
        .send_plugin_installed_request(PluginInstalledParams {
            cwds: None,
            install_suggestion_plugin_names: Some(vec!["computer-use".to_string()]),
        })
        .await?;

    let response: PluginInstalledResponse =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(request_id)).await??;

    assert_eq!(response.marketplaces.len(), 1);
    assert_eq!(response.marketplaces[0].name, "openai-curated");
    assert_eq!(
        response.marketplaces[0]
            .plugins
            .iter()
            .map(|plugin| (plugin.id.clone(), plugin.installed, plugin.enabled))
            .collect::<Vec<_>>(),
        vec![
            ("linear@openai-curated".to_string(), true, true),
            ("computer-use@openai-curated".to_string(), false, false),
        ]
    );
    assert_eq!(response.marketplace_load_errors, Vec::new());
    assert!(
        response.marketplaces[0]
            .plugins
            .iter()
            .all(|plugin| plugin.install_policy_source.is_none())
    );
    Ok(())
}

#[tokio::test]
async fn plugin_installed_ignores_local_cache_without_catalog() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_installed_plugin(&codex_home, "openai-curated", "linear")?;
    std::fs::write(
        codex_home.path().join("config.toml"),
        r#"[features]
plugins = true

[plugins."linear@openai-curated"]
enabled = true
"#,
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let request_id = mcp
        .send_plugin_installed_request(PluginInstalledParams {
            cwds: None,
            install_suggestion_plugin_names: None,
        })
        .await?;

    let response: PluginInstalledResponse =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(request_id)).await??;

    assert_eq!(response.marketplaces, Vec::new());
    assert_eq!(response.marketplace_load_errors, Vec::new());
    Ok(())
}

#[tokio::test]
async fn plugin_list_rejects_relative_cwds() -> Result<()> {
    let codex_home = TempDir::new()?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let request_id = mcp
        .send_raw_request(
            "plugin/list",
            Some(serde_json::json!({
                "cwds": ["relative-root"],
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
async fn plugin_list_keeps_valid_marketplaces_when_another_marketplace_fails_to_load() -> Result<()>
{
    let codex_home = TempDir::new()?;
    let valid_repo_root = TempDir::new()?;
    let invalid_repo_root = TempDir::new()?;
    std::fs::create_dir_all(valid_repo_root.path().join(".git"))?;
    std::fs::create_dir_all(valid_repo_root.path().join(".agents/plugins"))?;
    std::fs::create_dir_all(
        valid_repo_root
            .path()
            .join("plugins/valid-plugin/.codex-plugin"),
    )?;
    std::fs::create_dir_all(invalid_repo_root.path().join(".git"))?;
    std::fs::create_dir_all(invalid_repo_root.path().join(".agents/plugins"))?;
    write_plugins_enabled_config(codex_home.path())?;

    let valid_marketplace_path = AbsolutePathBuf::try_from(
        valid_repo_root
            .path()
            .join(".agents/plugins/marketplace.json"),
    )?;
    let invalid_marketplace_path = AbsolutePathBuf::try_from(
        invalid_repo_root
            .path()
            .join(".agents/plugins/marketplace.json"),
    )?;
    let valid_plugin_path =
        AbsolutePathBuf::try_from(valid_repo_root.path().join("plugins/valid-plugin"))?;

    std::fs::write(
        valid_marketplace_path.as_path(),
        r#"{
  "name": "valid-marketplace",
  "plugins": [
    {
      "name": "valid-plugin",
      "source": {
        "source": "local",
        "path": "./plugins/valid-plugin"
      }
    }
  ]
}"#,
    )?;
    std::fs::write(
        valid_repo_root
            .path()
            .join("plugins/valid-plugin/.codex-plugin/plugin.json"),
        r#"{"name":"valid-plugin","keywords":["api-key","developer tools"]}"#,
    )?;
    std::fs::write(invalid_marketplace_path.as_path(), "{not json")?;

    let home = codex_home.path().to_string_lossy().into_owned();
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .with_env_overrides(&[
            ("HOME", Some(home.as_str())),
            ("USERPROFILE", Some(home.as_str())),
        ])
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let request_id = mcp
        .send_plugin_list_request(PluginListParams {
            cwds: Some(vec![
                AbsolutePathBuf::try_from(valid_repo_root.path())?,
                AbsolutePathBuf::try_from(invalid_repo_root.path())?,
            ]),
            marketplace_kinds: None,
            force_refetch: false,
        })
        .await?;

    let response: PluginListResponse =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(request_id)).await??;

    assert_eq!(
        response.marketplaces,
        vec![PluginMarketplaceEntry {
            name: "valid-marketplace".to_string(),
            path: Some(valid_marketplace_path),
            interface: None,
            plugins: vec![PluginSummary {
                id: "valid-plugin@valid-marketplace".to_string(),
                remote_plugin_id: None,
                version: None,
                local_version: None,
                name: "valid-plugin".to_string(),
                share_context: None,
                source: PluginSource::Local {
                    path: valid_plugin_path,
                },
                installed: false,
                installed_at: None,
                enabled: false,
                install_policy: PluginInstallPolicy::Available,
                install_policy_source: None,
                must_show_installation_interstitial: None,
                auth_policy: PluginAuthPolicy::OnInstall,
                availability: codex_app_server_protocol::PluginAvailability::Available,
                disabled_reason: None,
                eligible_plan_types: None,
                interface: None,
                keywords: vec!["api-key".to_string(), "developer tools".to_string()],
            }],
        }]
    );
    assert_eq!(response.marketplace_load_errors.len(), 1);
    assert_eq!(
        response.marketplace_load_errors[0].marketplace_path,
        invalid_marketplace_path
    );
    assert!(
        response.marketplace_load_errors[0]
            .message
            .contains("invalid marketplace file"),
        "unexpected error: {:?}",
        response.marketplace_load_errors
    );
    assert!(response.featured_plugin_ids.is_empty());
    Ok(())
}

#[tokio::test]
async fn plugin_list_uses_alternate_discoverable_manifest_and_keeps_undiscoverable_plugins()
-> Result<()> {
    let codex_home = TempDir::new()?;
    let repo_root = TempDir::new()?;
    let valid_plugin_root = repo_root.path().join("plugins/valid-plugin");
    std::fs::create_dir_all(repo_root.path().join(".git"))?;
    std::fs::create_dir_all(
        repo_root
            .path()
            .join(ALTERNATE_MARKETPLACE_RELATIVE_PATH)
            .parent()
            .unwrap(),
    )?;
    std::fs::create_dir_all(
        valid_plugin_root
            .join(ALTERNATE_PLUGIN_MANIFEST_RELATIVE_PATH)
            .parent()
            .unwrap(),
    )?;
    write_plugins_enabled_config(codex_home.path())?;

    let marketplace_path =
        AbsolutePathBuf::try_from(repo_root.path().join(ALTERNATE_MARKETPLACE_RELATIVE_PATH))?;
    let valid_plugin_path = AbsolutePathBuf::try_from(valid_plugin_root.clone())?;

    std::fs::write(
        marketplace_path.as_path(),
        r#"{
  "name": "alternate-marketplace",
  "plugins": [
    {
      "name": "valid-plugin",
      "source": "./plugins/valid-plugin"
    },
    {
      "name": "missing-plugin",
      "source": "./plugins/missing-plugin"
    }
  ]
}"#,
    )?;
    std::fs::write(
        valid_plugin_root.join(ALTERNATE_PLUGIN_MANIFEST_RELATIVE_PATH),
        r#"{
  "name": "valid-plugin",
  "interface": {
    "displayName": "Valid Plugin"
  }
}"#,
    )?;

    let home = codex_home.path().to_string_lossy().into_owned();
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .with_env_overrides(&[
            ("HOME", Some(home.as_str())),
            ("USERPROFILE", Some(home.as_str())),
        ])
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let request_id = mcp
        .send_plugin_list_request(PluginListParams {
            cwds: Some(vec![AbsolutePathBuf::try_from(repo_root.path())?]),
            marketplace_kinds: None,
            force_refetch: false,
        })
        .await?;

    let response: PluginListResponse =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(request_id)).await??;

    assert_eq!(
        response.marketplaces,
        vec![PluginMarketplaceEntry {
            name: "alternate-marketplace".to_string(),
            path: Some(marketplace_path),
            interface: None,
            plugins: vec![
                PluginSummary {
                    id: "valid-plugin@alternate-marketplace".to_string(),
                    remote_plugin_id: None,
                    version: None,
                    local_version: None,
                    name: "valid-plugin".to_string(),
                    share_context: None,
                    source: PluginSource::Local {
                        path: valid_plugin_path,
                    },
                    installed: false,
                    installed_at: None,
                    enabled: false,
                    install_policy: PluginInstallPolicy::Available,
                    install_policy_source: None,
                    must_show_installation_interstitial: None,
                    auth_policy: PluginAuthPolicy::OnInstall,
                    availability: codex_app_server_protocol::PluginAvailability::Available,
                    disabled_reason: None,
                    eligible_plan_types: None,
                    interface: Some(codex_app_server_protocol::PluginInterface {
                        display_name: Some("Valid Plugin".to_string()),
                        short_description: None,
                        long_description: None,
                        developer_name: None,
                        category: None,
                        capabilities: Vec::new(),
                        website_url: None,
                        privacy_policy_url: None,
                        terms_of_service_url: None,
                        default_prompt: None,
                        brand_color: None,
                        composer_icon: None,
                        composer_icon_url: None,
                        logo: None,
                        logo_dark: None,
                        logo_url: None,
                        logo_url_dark: None,
                        screenshots: Vec::new(),
                        screenshot_urls: Vec::new(),
                    }),
                    keywords: Vec::new(),
                },
                PluginSummary {
                    id: "missing-plugin@alternate-marketplace".to_string(),
                    remote_plugin_id: None,
                    version: None,
                    local_version: None,
                    name: "missing-plugin".to_string(),
                    share_context: None,
                    source: PluginSource::Local {
                        path: AbsolutePathBuf::try_from(
                            repo_root.path().join("plugins/missing-plugin"),
                        )?,
                    },
                    installed: false,
                    installed_at: None,
                    enabled: false,
                    install_policy: PluginInstallPolicy::Available,
                    install_policy_source: None,
                    must_show_installation_interstitial: None,
                    auth_policy: PluginAuthPolicy::OnInstall,
                    availability: codex_app_server_protocol::PluginAvailability::Available,
                    disabled_reason: None,
                    eligible_plan_types: None,
                    interface: None,
                    keywords: Vec::new(),
                },
            ],
        }]
    );
    assert!(response.marketplace_load_errors.is_empty());
    Ok(())
}

#[tokio::test]
async fn plugin_list_accepts_omitted_cwds() -> Result<()> {
    let codex_home = TempDir::new()?;
    std::fs::create_dir_all(codex_home.path().join(".agents/plugins"))?;
    write_plugins_enabled_config(codex_home.path())?;
    std::fs::write(
        codex_home.path().join(".agents/plugins/marketplace.json"),
        r#"{
  "name": "codex-curated",
  "plugins": [
    {
      "name": "home-plugin",
      "source": {
        "source": "local",
        "path": "./home-plugin"
      }
    }
  ]
}"#,
    )?;
    let home = codex_home.path().to_string_lossy().into_owned();
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .with_env_overrides(&[
            ("HOME", Some(home.as_str())),
            ("USERPROFILE", Some(home.as_str())),
        ])
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let request_id = mcp
        .send_plugin_list_request(PluginListParams {
            cwds: None,
            marketplace_kinds: None,
            force_refetch: false,
        })
        .await?;

    let _: PluginListResponse = timeout(DEFAULT_TIMEOUT, mcp.read_response(request_id)).await??;
    Ok(())
}

#[tokio::test]
async fn plugin_list_returns_share_context_for_shared_local_plugin() -> Result<()> {
    let codex_home = TempDir::new()?;
    let repo_root = TempDir::new()?;
    let plugin_root = repo_root.path().join("plugins/demo-plugin");
    std::fs::create_dir_all(repo_root.path().join(".git"))?;
    std::fs::create_dir_all(repo_root.path().join(".agents/plugins"))?;
    std::fs::create_dir_all(plugin_root.join(".codex-plugin"))?;
    write_plugins_enabled_config(codex_home.path())?;
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
        r#"{"name":"demo-plugin","version":"1.2.3"}"#,
    )?;
    write_plugin_share_local_path_mapping(
        codex_home.path(),
        "plugins_123",
        &AbsolutePathBuf::try_from(plugin_root)?,
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let request_id = mcp
        .send_plugin_list_request(PluginListParams {
            cwds: Some(vec![AbsolutePathBuf::try_from(repo_root.path())?]),
            marketplace_kinds: None,
            force_refetch: false,
        })
        .await?;

    let response: PluginListResponse =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(request_id)).await??;

    let plugin = response
        .marketplaces
        .iter()
        .flat_map(|marketplace| marketplace.plugins.iter())
        .find(|plugin| plugin.name == "demo-plugin")
        .expect("expected demo-plugin entry");
    assert_eq!(plugin.remote_plugin_id, None);
    assert_eq!(plugin.local_version.as_deref(), Some("1.2.3"));
    let share_context = plugin
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
async fn plugin_list_force_refetch_waits_for_same_path_local_plugin_upgrade() -> Result<()> {
    let codex_home = TempDir::new()?;
    let marketplace_root = TempDir::new()?;
    std::fs::create_dir_all(marketplace_root.path().join(".git"))?;
    std::fs::create_dir_all(marketplace_root.path().join(".agents/plugins"))?;
    let source_manifest = marketplace_root
        .path()
        .join("sample-plugin/.codex-plugin/plugin.json");
    std::fs::create_dir_all(source_manifest.parent().expect("source manifest parent"))?;
    std::fs::write(
        &source_manifest,
        r#"{"name":"sample-plugin","version":"1.0.0"}"#,
    )?;
    std::fs::write(
        marketplace_root
            .path()
            .join(".agents/plugins/marketplace.json"),
        r#"{
  "name": "sample-marketplace",
  "plugins": [
    {
      "name": "sample-plugin",
      "source": {
        "source": "local",
        "path": "./sample-plugin"
      }
    }
  ]
}"#,
    )?;
    std::fs::write(
        codex_home.path().join("config.toml"),
        r#"[features]
plugins = true
[plugins."sample-plugin@sample-marketplace"]
enabled = true
"#,
    )?;
    write_installed_plugin_with_version(
        &codex_home,
        "sample-marketplace",
        "sample-plugin",
        "1.0.0",
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build()
        .await?;
    timeout(DEFAULT_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_plugin_list_request(PluginListParams {
            cwds: Some(vec![AbsolutePathBuf::try_from(marketplace_root.path())?]),
            marketplace_kinds: Some(vec![PluginListMarketplaceKind::Local]),
            force_refetch: true,
        })
        .await?;
    let initial_response: JSONRPCResponse = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let _: PluginListResponse = to_response(initial_response)?;

    std::fs::write(
        &source_manifest,
        r#"{"name":"sample-plugin","version":"1.1.0"}"#,
    )?;

    let request_id = mcp
        .send_plugin_list_request(PluginListParams {
            cwds: Some(vec![AbsolutePathBuf::try_from(marketplace_root.path())?]),
            marketplace_kinds: Some(vec![PluginListMarketplaceKind::Local]),
            force_refetch: true,
        })
        .await?;
    let response: JSONRPCResponse = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let response: PluginListResponse = to_response(response)?;
    let plugin = response
        .marketplaces
        .iter()
        .find(|marketplace| marketplace.name == "sample-marketplace")
        .and_then(|marketplace| {
            marketplace
                .plugins
                .iter()
                .find(|plugin| plugin.name == "sample-plugin")
        })
        .expect("upgraded local plugin should appear in its marketplace response");
    assert!(plugin.installed);
    assert!(plugin.enabled);
    assert_eq!(plugin.local_version.as_deref(), Some("1.1.0"));

    let plugin_cache = codex_home
        .path()
        .join("plugins/cache/sample-marketplace/sample-plugin");
    let installed_manifest = plugin_cache.join("1.1.0/.codex-plugin/plugin.json");
    assert!(
        installed_manifest.is_file(),
        "force-refetched plugin/list must finish installing the newer local plugin before responding"
    );
    assert!(
        !plugin_cache.join("1.0.0").exists(),
        "force-refetched plugin/list must remove the superseded local plugin before responding"
    );
    let installed_manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(installed_manifest)?)?;
    assert_eq!(installed_manifest["version"], serde_json::json!("1.1.0"));

    Ok(())
}

#[tokio::test]
async fn plugin_list_includes_install_and_enabled_state_from_config() -> Result<()> {
    let codex_home = TempDir::new()?;
    let repo_root = TempDir::new()?;
    std::fs::create_dir_all(repo_root.path().join(".git"))?;
    std::fs::create_dir_all(repo_root.path().join(".agents/plugins"))?;
    write_installed_plugin(&codex_home, "codex-curated", "enabled-plugin")?;
    write_installed_plugin(&codex_home, "codex-curated", "disabled-plugin")?;
    std::fs::write(
        repo_root.path().join(".agents/plugins/marketplace.json"),
        r#"{
  "name": "codex-curated",
  "interface": {
    "displayName": "ChatGPT Official"
  },
  "plugins": [
    {
      "name": "enabled-plugin",
      "source": {
        "source": "local",
        "path": "./enabled-plugin"
      }
    },
    {
      "name": "disabled-plugin",
      "source": {
        "source": "local",
        "path": "./disabled-plugin"
      }
    },
    {
      "name": "uninstalled-plugin",
      "source": {
        "source": "local",
        "path": "./uninstalled-plugin"
      }
    }
  ]
}"#,
    )?;
    std::fs::write(
        codex_home.path().join("config.toml"),
        r#"[features]
plugins = true

[plugins."enabled-plugin@codex-curated"]
enabled = true

[plugins."disabled-plugin@codex-curated"]
enabled = false
"#,
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let request_id = mcp
        .send_plugin_list_request(PluginListParams {
            cwds: Some(vec![AbsolutePathBuf::try_from(repo_root.path())?]),
            marketplace_kinds: None,
            force_refetch: false,
        })
        .await?;

    let response: PluginListResponse =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(request_id)).await??;

    let marketplace = response
        .marketplaces
        .into_iter()
        .find(|marketplace| {
            marketplace.path.as_ref()
                == Some(
                    &AbsolutePathBuf::try_from(
                        repo_root.path().join(".agents/plugins/marketplace.json"),
                    )
                    .expect("absolute marketplace path"),
                )
        })
        .expect("expected repo marketplace entry");

    assert_eq!(marketplace.name, "codex-curated");
    assert_eq!(
        marketplace
            .interface
            .as_ref()
            .and_then(|interface| interface.display_name.as_deref()),
        Some("ChatGPT Official")
    );
    assert_eq!(marketplace.plugins.len(), 3);
    assert_eq!(marketplace.plugins[0].id, "enabled-plugin@codex-curated");
    assert_eq!(marketplace.plugins[0].name, "enabled-plugin");
    assert_eq!(marketplace.plugins[0].installed, true);
    assert_eq!(marketplace.plugins[0].enabled, true);
    assert_eq!(
        marketplace.plugins[0].install_policy,
        PluginInstallPolicy::Available
    );
    assert_eq!(
        marketplace.plugins[0].auth_policy,
        PluginAuthPolicy::OnInstall
    );
    assert_eq!(marketplace.plugins[1].id, "disabled-plugin@codex-curated");
    assert_eq!(marketplace.plugins[1].name, "disabled-plugin");
    assert_eq!(marketplace.plugins[1].installed, true);
    assert_eq!(marketplace.plugins[1].enabled, false);
    assert_eq!(
        marketplace.plugins[1].install_policy,
        PluginInstallPolicy::Available
    );
    assert_eq!(
        marketplace.plugins[1].auth_policy,
        PluginAuthPolicy::OnInstall
    );
    assert_eq!(
        marketplace.plugins[2].id,
        "uninstalled-plugin@codex-curated"
    );
    assert_eq!(marketplace.plugins[2].name, "uninstalled-plugin");
    assert_eq!(marketplace.plugins[2].installed, false);
    assert_eq!(marketplace.plugins[2].enabled, false);
    assert_eq!(
        marketplace.plugins[2].install_policy,
        PluginInstallPolicy::Available
    );
    assert_eq!(
        marketplace.plugins[2].auth_policy,
        PluginAuthPolicy::OnInstall
    );
    Ok(())
}

#[tokio::test]
async fn plugin_list_uses_home_config_for_enabled_state() -> Result<()> {
    let codex_home = TempDir::new()?;
    std::fs::create_dir_all(codex_home.path().join(".agents/plugins"))?;
    write_installed_plugin(&codex_home, "codex-curated", "shared-plugin")?;
    std::fs::write(
        codex_home.path().join(".agents/plugins/marketplace.json"),
        r#"{
  "name": "codex-curated",
  "plugins": [
    {
      "name": "shared-plugin",
      "source": {
        "source": "local",
        "path": "./shared-plugin"
      }
    }
  ]
}"#,
    )?;
    std::fs::write(
        codex_home.path().join("config.toml"),
        r#"[features]
plugins = true

[plugins."shared-plugin@codex-curated"]
enabled = true
"#,
    )?;

    let workspace_enabled = TempDir::new()?;
    std::fs::create_dir_all(workspace_enabled.path().join(".git"))?;
    std::fs::create_dir_all(workspace_enabled.path().join(".agents/plugins"))?;
    std::fs::write(
        workspace_enabled
            .path()
            .join(".agents/plugins/marketplace.json"),
        r#"{
  "name": "codex-curated",
  "plugins": [
    {
      "name": "shared-plugin",
      "source": {
        "source": "local",
        "path": "./shared-plugin"
      }
    }
  ]
}"#,
    )?;
    std::fs::create_dir_all(workspace_enabled.path().join(PROJECT_CONFIG_DIRECTORY))?;
    std::fs::write(
        workspace_enabled
            .path()
            .join(PROJECT_CONFIG_DIRECTORY)
            .join("config.toml"),
        r#"[plugins."shared-plugin@codex-curated"]
enabled = false
"#,
    )?;
    set_project_trust_level(
        codex_home.path(),
        workspace_enabled.path(),
        TrustLevel::Trusted,
    )?;

    let workspace_default = TempDir::new()?;
    let home = codex_home.path().to_string_lossy().into_owned();
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .with_env_overrides(&[
            ("HOME", Some(home.as_str())),
            ("USERPROFILE", Some(home.as_str())),
        ])
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let request_id = mcp
        .send_plugin_list_request(PluginListParams {
            cwds: Some(vec![
                AbsolutePathBuf::try_from(workspace_enabled.path())?,
                AbsolutePathBuf::try_from(workspace_default.path())?,
            ]),
            marketplace_kinds: None,
            force_refetch: false,
        })
        .await?;

    let response: PluginListResponse =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(request_id)).await??;

    let shared_plugin = response
        .marketplaces
        .iter()
        .flat_map(|marketplace| marketplace.plugins.iter())
        .find(|plugin| plugin.name == "shared-plugin")
        .expect("expected shared-plugin entry");
    assert_eq!(shared_plugin.id, "shared-plugin@codex-curated");
    assert_eq!(shared_plugin.installed, true);
    assert_eq!(shared_plugin.enabled, true);
    Ok(())
}

#[tokio::test]
async fn plugin_list_returns_plugin_interface_with_absolute_asset_paths() -> Result<()> {
    let codex_home = TempDir::new()?;
    let repo_root = TempDir::new()?;
    let plugin_root = repo_root.path().join("plugins/demo-plugin");
    std::fs::create_dir_all(repo_root.path().join(".git"))?;
    std::fs::create_dir_all(repo_root.path().join(".agents/plugins"))?;
    std::fs::create_dir_all(plugin_root.join(".codex-plugin"))?;
    write_plugins_enabled_config(codex_home.path())?;
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
      "Starter prompt for trying a plugin",
      "Find my next action"
    ],
    "brandColor": "#3B82F6",
    "composerIcon": "./assets/icon.png",
    "logo": "./assets/logo.png",
    "screenshots": ["./assets/screenshot1.png", "./assets/screenshot2.png"]
  }
}"##,
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let request_id = mcp
        .send_plugin_list_request(PluginListParams {
            cwds: Some(vec![AbsolutePathBuf::try_from(repo_root.path())?]),
            marketplace_kinds: None,
            force_refetch: false,
        })
        .await?;

    let response: PluginListResponse =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(request_id)).await??;

    let plugin = response
        .marketplaces
        .iter()
        .flat_map(|marketplace| marketplace.plugins.iter())
        .find(|plugin| plugin.name == "demo-plugin")
        .expect("expected demo-plugin entry");

    assert_eq!(plugin.id, "demo-plugin@codex-curated");
    assert_eq!(plugin.installed, false);
    assert_eq!(plugin.enabled, false);
    assert_eq!(plugin.install_policy, PluginInstallPolicy::Available);
    assert_eq!(plugin.auth_policy, PluginAuthPolicy::OnInstall);
    let interface = plugin
        .interface
        .as_ref()
        .expect("expected plugin interface");
    assert_eq!(
        interface.display_name.as_deref(),
        Some("Plugin Display Name")
    );
    assert_eq!(interface.category.as_deref(), Some("Design"));
    assert_eq!(
        interface.website_url.as_deref(),
        Some("https://openai.com/")
    );
    assert_eq!(
        interface.privacy_policy_url.as_deref(),
        Some("https://openai.com/policies/row-privacy-policy/")
    );
    assert_eq!(
        interface.terms_of_service_url.as_deref(),
        Some("https://openai.com/policies/row-terms-of-use/")
    );
    assert_eq!(
        interface.default_prompt,
        Some(vec![
            "Starter prompt for trying a plugin".to_string(),
            "Find my next action".to_string()
        ])
    );
    assert_eq!(
        interface.composer_icon,
        Some(AbsolutePathBuf::try_from(
            plugin_root.join("assets/icon.png")
        )?)
    );
    assert_eq!(
        interface.logo,
        Some(AbsolutePathBuf::try_from(
            plugin_root.join("assets/logo.png")
        )?)
    );
    assert_eq!(
        interface.screenshots,
        vec![
            AbsolutePathBuf::try_from(plugin_root.join("assets/screenshot1.png"))?,
            AbsolutePathBuf::try_from(plugin_root.join("assets/screenshot2.png"))?,
        ]
    );
    Ok(())
}

#[tokio::test]
async fn plugin_list_accepts_legacy_string_default_prompt() -> Result<()> {
    let codex_home = TempDir::new()?;
    let repo_root = TempDir::new()?;
    let plugin_root = repo_root.path().join("plugins/demo-plugin");
    std::fs::create_dir_all(repo_root.path().join(".git"))?;
    std::fs::create_dir_all(repo_root.path().join(".agents/plugins"))?;
    std::fs::create_dir_all(plugin_root.join(".codex-plugin"))?;
    write_plugins_enabled_config(codex_home.path())?;
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

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let request_id = mcp
        .send_plugin_list_request(PluginListParams {
            cwds: Some(vec![AbsolutePathBuf::try_from(repo_root.path())?]),
            marketplace_kinds: None,
            force_refetch: false,
        })
        .await?;

    let response: PluginListResponse =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(request_id)).await??;

    let plugin = response
        .marketplaces
        .iter()
        .flat_map(|marketplace| marketplace.plugins.iter())
        .find(|plugin| plugin.name == "demo-plugin")
        .expect("expected demo-plugin entry");
    assert_eq!(
        plugin
            .interface
            .as_ref()
            .and_then(|interface| interface.default_prompt.clone()),
        Some(vec!["Starter prompt for trying a plugin".to_string()])
    );
    Ok(())
}

#[tokio::test]
async fn plugin_list_returns_installed_git_source_interface_from_cache() -> Result<()> {
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
      }},
      "category": "Developer Tools"
    }}
  ]
}}"#
        ),
    )?;
    let cached_plugin_root = codex_home.path().join("plugins/cache/debug/toolkit/local");
    std::fs::create_dir_all(cached_plugin_root.join(".codex-plugin"))?;
    std::fs::write(
        cached_plugin_root.join(".codex-plugin/plugin.json"),
        r##"{
  "name": "toolkit",
  "interface": {
    "displayName": "Toolkit",
    "shortDescription": "Search cached data",
    "category": "Cached Category",
    "brandColor": "#3B82F6",
    "composerIcon": "./assets/icon.png",
    "logo": "./assets/logo.png"
  }
}"##,
    )?;
    std::fs::write(
        codex_home.path().join("config.toml"),
        r#"[features]
plugins = true

[plugins."toolkit@debug"]
enabled = true
"#,
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let request_id = mcp
        .send_plugin_list_request(PluginListParams {
            cwds: Some(vec![AbsolutePathBuf::try_from(repo_root.path())?]),
            marketplace_kinds: None,
            force_refetch: false,
        })
        .await?;

    let response: PluginListResponse =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(request_id)).await??;

    let plugin = response
        .marketplaces
        .iter()
        .flat_map(|marketplace| marketplace.plugins.iter())
        .find(|plugin| plugin.name == "toolkit")
        .expect("expected toolkit entry");

    assert_eq!(plugin.id, "toolkit@debug");
    assert_eq!(plugin.installed, true);
    assert_eq!(plugin.enabled, true);
    assert_eq!(
        plugin.source,
        PluginSource::Git {
            url: missing_remote_repo_url,
            path: Some("plugins/toolkit".to_string()),
            ref_name: None,
            sha: None,
        }
    );
    let interface = plugin
        .interface
        .as_ref()
        .expect("expected cached plugin interface");
    assert_eq!(interface.display_name.as_deref(), Some("Toolkit"));
    assert_eq!(
        interface.short_description.as_deref(),
        Some("Search cached data")
    );
    assert_eq!(interface.category.as_deref(), Some("Developer Tools"));
    assert_eq!(interface.brand_color.as_deref(), Some("#3B82F6"));
    let canonical_cached_plugin_root = std::fs::canonicalize(&cached_plugin_root)?;
    assert_eq!(
        interface.composer_icon,
        Some(AbsolutePathBuf::try_from(
            canonical_cached_plugin_root.join("assets/icon.png")
        )?)
    );
    assert_eq!(
        interface.logo,
        Some(AbsolutePathBuf::try_from(
            canonical_cached_plugin_root.join("assets/logo.png")
        )?)
    );
    Ok(())
}

#[tokio::test]
async fn plugin_list_rejects_managed_remote_marketplaces_before_direct_backend_io() -> Result<()> {
    let codex_home = TempDir::new()?;
    let outbound_probe = MockServer::start().await;
    let proxy_uri = outbound_probe.uri();
    write_plugins_enabled_config(codex_home.path())?;

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

    for marketplace_kind in [
        PluginListMarketplaceKind::Vertical,
        PluginListMarketplaceKind::WorkspaceDirectory,
        PluginListMarketplaceKind::SharedWithMe,
        PluginListMarketplaceKind::CreatedByMeRemote,
    ] {
        let request_id = mcp
            .send_plugin_list_request(PluginListParams {
                cwds: None,
                marketplace_kinds: Some(vec![marketplace_kind]),
                force_refetch: true,
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
            WHISPLY_MANAGED_REMOTE_PLUGIN_MARKETPLACES_UNAVAILABLE_ERROR
        );
    }

    let request_id = mcp
        .send_plugin_list_request(PluginListParams {
            cwds: None,
            marketplace_kinds: None,
            force_refetch: true,
        })
        .await?;
    let response: PluginListResponse =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(request_id)).await??;
    assert_eq!(
        response,
        PluginListResponse {
            marketplaces: Vec::new(),
            marketplace_load_errors: Vec::new(),
            featured_plugin_ids: Vec::new(),
        }
    );

    assert!(
        outbound_probe
            .received_requests()
            .await
            .expect("probe should record requests")
            .is_empty(),
        "managed remote marketplace requests must not reach the direct backend"
    );
    assert!(
        !codex_home
            .path()
            .join("cache/remote_plugin_catalog")
            .exists()
    );
    Ok(())
}

#[tokio::test]
async fn plugin_installed_lists_local_suggestions_without_direct_backend_io() -> Result<()> {
    let codex_home = TempDir::new()?;
    let workspace = TempDir::new()?;
    let outbound_probe = MockServer::start().await;
    let proxy_uri = outbound_probe.uri();
    write_plugins_enabled_config(codex_home.path())?;
    std::fs::create_dir_all(workspace.path().join(".git"))?;
    std::fs::create_dir_all(workspace.path().join(".agents/plugins"))?;
    std::fs::create_dir_all(workspace.path().join("plugins/local-linear/.codex-plugin"))?;
    std::fs::write(
        workspace
            .path()
            .join("plugins/local-linear/.codex-plugin/plugin.json"),
        r#"{ "name": "local-linear" }"#,
    )?;
    std::fs::write(
        workspace.path().join(".agents/plugins/marketplace.json"),
        r#"{
  "name": "local-tools",
  "plugins": [
    {
      "name": "local-linear",
      "source": {
        "source": "local",
        "path": "./plugins/local-linear"
      }
    }
  ]
}"#,
    )?;
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
        .send_plugin_installed_request(PluginInstalledParams {
            cwds: Some(vec![AbsolutePathBuf::try_from(workspace.path())?]),
            install_suggestion_plugin_names: Some(vec!["local-linear".to_string()]),
        })
        .await?;
    let response: PluginInstalledResponse =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(request_id)).await??;
    assert_eq!(response.marketplaces.len(), 1);
    assert_eq!(response.marketplaces[0].name, "local-tools");
    assert_eq!(
        response.marketplaces[0]
            .plugins
            .iter()
            .map(|plugin| plugin.id.as_str())
            .collect::<Vec<_>>(),
        vec!["local-linear@local-tools"]
    );
    assert!(!response.marketplaces[0].plugins[0].installed);
    assert!(
        outbound_probe
            .received_requests()
            .await
            .expect("probe should record requests")
            .is_empty(),
        "plugin/installed must not synchronize against a direct backend"
    );
    assert!(
        !codex_home
            .path()
            .join("cache/remote_plugin_catalog")
            .exists()
    );
    Ok(())
}

#[tokio::test]
async fn plugin_startup_tasks_do_not_contact_direct_backends() -> Result<()> {
    let codex_home = TempDir::new()?;
    let outbound_probe = MockServer::start().await;
    let proxy_uri = outbound_probe.uri();
    std::fs::write(
        codex_home.path().join("config.toml"),
        r#"[features]
plugins = true
remote_plugin = true
"#,
    )?;

    let _app_server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .with_plugin_startup_tasks()
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

    tokio::time::sleep(Duration::from_millis(250)).await;
    assert!(
        outbound_probe
            .received_requests()
            .await
            .expect("probe should record requests")
            .is_empty(),
        "BrokerOnly startup must not schedule direct plugin catalog, bundle, featured, or curated sync requests"
    );
    Ok(())
}

fn write_installed_plugin(
    codex_home: &TempDir,
    marketplace_name: &str,
    plugin_name: &str,
) -> Result<()> {
    write_installed_plugin_with_version(codex_home, marketplace_name, plugin_name, "local")
}

fn write_installed_plugin_with_version(
    codex_home: &TempDir,
    marketplace_name: &str,
    plugin_name: &str,
    plugin_version: &str,
) -> Result<()> {
    let plugin_root = codex_home
        .path()
        .join("plugins/cache")
        .join(marketplace_name)
        .join(plugin_name)
        .join(plugin_version)
        .join(".codex-plugin");
    std::fs::create_dir_all(&plugin_root)?;
    std::fs::write(
        plugin_root.join("plugin.json"),
        format!(r#"{{"name":"{plugin_name}"}}"#),
    )?;
    Ok(())
}

fn write_openai_curated_marketplace(
    codex_home: &std::path::Path,
    plugin_names: &[&str],
) -> std::io::Result<()> {
    write_curated_marketplace(
        codex_home,
        "marketplace.json",
        "openai-curated",
        /*display_name*/ None,
        plugin_names,
    )
}

fn write_curated_marketplace(
    codex_home: &std::path::Path,
    manifest_name: &str,
    marketplace_name: &str,
    display_name: Option<&str>,
    plugin_names: &[&str],
) -> std::io::Result<()> {
    let curated_root = codex_home.join(".tmp/plugins");
    std::fs::create_dir_all(curated_root.join(".git"))?;
    std::fs::create_dir_all(curated_root.join(".agents/plugins"))?;
    let plugins = plugin_names
        .iter()
        .map(|plugin_name| {
            format!(
                r#"{{
      "name": "{plugin_name}",
      "source": {{
        "source": "local",
        "path": "./plugins/{plugin_name}"
      }}
    }}"#
            )
        })
        .collect::<Vec<_>>()
        .join(",\n");
    let interface = display_name
        .map(|display_name| {
            format!(
                r#"
  "interface": {{
    "displayName": "{display_name}"
  }},"#
            )
        })
        .unwrap_or_default();
    std::fs::write(
        curated_root.join(".agents/plugins").join(manifest_name),
        format!(
            r#"{{
  "name": "{marketplace_name}",{interface}
  "plugins": [
{plugins}
  ]
}}"#
        ),
    )?;

    for plugin_name in plugin_names {
        let plugin_root = curated_root.join(format!("plugins/{plugin_name}/.codex-plugin"));
        std::fs::create_dir_all(&plugin_root)?;
        std::fs::write(
            plugin_root.join("plugin.json"),
            format!(r#"{{"name":"{plugin_name}"}}"#),
        )?;
    }
    std::fs::create_dir_all(codex_home.join(".tmp"))?;
    std::fs::write(
        codex_home.join(".tmp/plugins.sha"),
        format!("{TEST_CURATED_PLUGIN_SHA}\n"),
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
    let contents = serde_json::to_string_pretty(&serde_json::json!({
        "localPluginPathsByRemotePluginId": local_plugin_paths_by_remote_plugin_id,
    }))
    .map_err(std::io::Error::other)?;
    std::fs::create_dir_all(codex_home.join(".tmp"))?;
    std::fs::write(
        codex_home.join(".tmp/plugin-share-local-paths-v1.json"),
        format!("{contents}\n"),
    )
}

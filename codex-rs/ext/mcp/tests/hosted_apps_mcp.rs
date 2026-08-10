use std::sync::Arc;

use codex_core::McpManager;
use codex_core::config::Config;
use codex_core::config::ConfigBuilder;
use codex_core_plugins::PluginsManager;
use codex_extension_api::ExtensionRegistryBuilder;
use codex_extension_api::McpServerContribution;
use codex_extension_api::McpServerContributionContext;
use codex_extension_api::McpServerContributor;
use codex_login::AuthCredentialsStoreMode;
use codex_login::AuthDotJson;
use codex_login::AuthKeyringBackendKind;
use codex_login::CodexAuth;
use codex_login::save_auth;
use codex_login::token_data::TokenData;
use codex_login::token_data::parse_chatgpt_jwt_claims;
use codex_mcp::CODEX_APPS_MCP_SERVER_NAME;
use codex_protocol::auth::AuthMode;
use wiremock::MockServer;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[tokio::test]
async fn hosted_registry_suppresses_host_owned_apps_and_preserves_external_mcp() -> TestResult {
    let codex_home = tempfile::tempdir()?;
    let config = ConfigBuilder::default()
        .codex_home(codex_home.path().to_path_buf())
        .fallback_cwd(Some(codex_home.path().to_path_buf()))
        .cli_overrides(vec![
            ("features.apps".to_string(), true.into()),
            (
                "mcp_servers.external.command".to_string(),
                "external-mcp".into(),
            ),
        ])
        .build()
        .await?;
    let auth = persisted_chatgpt_auth(codex_home.path()).await?;
    let manager = installed_manager(&config);

    let servers = manager.effective_servers(&config, Some(&auth)).await;
    assert!(
        !servers.contains_key(CODEX_APPS_MCP_SERVER_NAME),
        "the installed extension registry must not restore the host-owned Apps MCP"
    );
    assert!(
        servers.contains_key("external"),
        "suppressing the reserved name must retain genuine external MCP servers"
    );

    Ok(())
}

#[tokio::test]
async fn installed_plugin_chatgpt_alias_is_rejected_before_auth_or_network() -> TestResult {
    let codex_home = tempfile::tempdir()?;
    let remote = MockServer::start().await;
    let plugin_root = codex_home
        .path()
        .join("plugins/cache/test/legacy-chatgpt-plugin/local");
    std::fs::create_dir_all(plugin_root.join(".codex-plugin"))?;
    std::fs::write(
        plugin_root.join(".codex-plugin/plugin.json"),
        r#"{"name":"legacy-chatgpt-plugin"}"#,
    )?;
    std::fs::write(
        plugin_root.join(".mcp.json"),
        format!(
            r#"{{
  "mcpServers": {{
    "installed_chatgpt_alias": {{
      "url": "{}/backend-api/ps/mcp",
      "auth": "chatgpt"
    }}
  }}
}}"#,
            remote.uri()
        ),
    )?;
    std::fs::write(
        codex_home.path().join("config.toml"),
        r#"
[features]
plugins = true

[plugins."legacy-chatgpt-plugin@test"]
enabled = true
"#,
    )?;
    let config = ConfigBuilder::default()
        .codex_home(codex_home.path().to_path_buf())
        .fallback_cwd(Some(codex_home.path().to_path_buf()))
        .build()
        .await?;
    let auth = persisted_chatgpt_auth(codex_home.path()).await?;
    let manager = installed_manager(&config);

    let runtime_servers = manager.runtime_servers(&config).await;
    let effective_servers = manager.effective_servers(&config, Some(&auth)).await;

    assert!(
        !runtime_servers.contains_key("installed_chatgpt_alias"),
        "the installed plugin's ChatGPT-auth alias must be rejected before auth gating"
    );
    assert!(
        !effective_servers.contains_key("installed_chatgpt_alias"),
        "persisted legacy ChatGPT auth must not reactivate an installed plugin alias"
    );
    assert!(
        remote
            .received_requests()
            .await
            .expect("wiremock should retain request history")
            .is_empty(),
        "rejecting the plugin alias must not make an HTTP request"
    );
    Ok(())
}

#[tokio::test]
async fn config_load_rejects_aliased_chatgpt_mcp_with_persisted_legacy_auth() -> TestResult {
    let codex_home = tempfile::tempdir()?;
    std::fs::write(
        codex_home.path().join("config.toml"),
        r#"
[mcp_servers.legacy_chatgpt_alias]
url = "https://chatgpt.com/backend-api/ps/mcp"
auth = "chatgpt"
"#,
    )?;
    let _auth = persisted_chatgpt_auth(codex_home.path()).await?;

    let error = ConfigBuilder::default()
        .codex_home(codex_home.path().to_path_buf())
        .fallback_cwd(Some(codex_home.path().to_path_buf()))
        .build()
        .await
        .expect_err("aliased ChatGPT MCP configuration must fail before auth can be used");
    let message = error.to_string();
    assert!(message.contains("legacy_chatgpt_alias"));
    assert!(message.contains("auth = \"chatgpt\""));
    Ok(())
}

async fn persisted_chatgpt_auth(
    codex_home: &std::path::Path,
) -> Result<CodexAuth, Box<dyn std::error::Error>> {
    let id_token = parse_chatgpt_jwt_claims(
        "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.eyJodHRwczovL2FwaS5vcGVuYWkuY29tL2F1dGgiOnsiY2hhdGdwdF9wbGFuX3R5cGUiOiJwcm8iLCJjaGF0Z3B0X3VzZXJfaWQiOiJsZWdhY3ktdXNlciIsImNoYXRncHRfYWNjb3VudF9pZCI6ImxlZ2FjeS1hY2NvdW50In19.signature",
    )?;
    let auth_dot_json = AuthDotJson {
        auth_mode: Some(AuthMode::Chatgpt),
        openai_api_key: None,
        tokens: Some(TokenData {
            id_token,
            access_token: "legacy-chatgpt-token".to_string(),
            refresh_token: "legacy-refresh-token".to_string(),
            account_id: Some("legacy-account".to_string()),
        }),
        last_refresh: None,
        agent_identity: None,
        personal_access_token: None,
        bedrock_api_key: None,
    };
    save_auth(
        codex_home,
        &auth_dot_json,
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )?;
    CodexAuth::from_auth_storage(
        codex_home,
        AuthCredentialsStoreMode::File,
        /*chatgpt_base_url*/ None,
        AuthKeyringBackendKind::default(),
        &codex_login::test_support::transport_default_auth_route_config(),
    )
    .await?
    .ok_or_else(|| "persisted ChatGPT auth was not loaded".into())
}

#[tokio::test]
async fn runtime_veto_removes_late_reserved_overlay() -> TestResult {
    let codex_home = tempfile::tempdir()?;
    let config = ConfigBuilder::default()
        .codex_home(codex_home.path().to_path_buf())
        .fallback_cwd(Some(codex_home.path().to_path_buf()))
        .cli_overrides(vec![("features.apps".to_string(), true.into())])
        .build()
        .await?;
    let auth = CodexAuth::create_dummy_chatgpt_auth_for_testing();
    let mut builder = ExtensionRegistryBuilder::new();
    codex_mcp_extension::install(&mut builder);
    builder.mcp_server_contributor(Arc::new(AttemptCodexAppsOverlay));
    let manager = McpManager::new_with_extensions(
        Arc::new(PluginsManager::new(config.codex_home.to_path_buf())),
        Arc::new(builder.build()),
        codex_core::CodexAppsToolsCache::default(),
    );

    let servers = manager.effective_servers(&config, Some(&auth)).await;

    assert!(!servers.contains_key(CODEX_APPS_MCP_SERVER_NAME));
    Ok(())
}

#[tokio::test]
async fn host_owned_apps_mcp_is_suppressed_for_api_key_auth() -> TestResult {
    let codex_home = tempfile::tempdir()?;
    let config = ConfigBuilder::default()
        .codex_home(codex_home.path().to_path_buf())
        .fallback_cwd(Some(codex_home.path().to_path_buf()))
        .cli_overrides(vec![("features.apps".to_string(), true.into())])
        .build()
        .await?;
    let auth = CodexAuth::from_api_key("test");
    let manager = installed_manager(&config);

    let servers = manager.effective_servers(&config, Some(&auth)).await;
    assert!(!servers.contains_key(CODEX_APPS_MCP_SERVER_NAME));

    Ok(())
}

fn installed_manager(config: &Config) -> McpManager {
    let mut builder = ExtensionRegistryBuilder::new();
    codex_mcp_extension::install(&mut builder);
    McpManager::new_with_extensions(
        Arc::new(PluginsManager::new(config.codex_home.to_path_buf())),
        Arc::new(builder.build()),
        codex_core::CodexAppsToolsCache::default(),
    )
}

struct AttemptCodexAppsOverlay;

impl McpServerContributor<Config> for AttemptCodexAppsOverlay {
    fn id(&self) -> &'static str {
        "attempt_codex_apps_overlay"
    }

    fn contribute<'a>(
        &'a self,
        _context: McpServerContributionContext<'a, Config>,
    ) -> codex_extension_api::ExtensionFuture<'a, Vec<McpServerContribution>> {
        Box::pin(async move {
            let config = serde_json::from_value(serde_json::json!({
                "command": "late-host-owned-apps-mcp"
            }))
            .expect("test MCP config should deserialize");
            vec![McpServerContribution::Set {
                name: CODEX_APPS_MCP_SERVER_NAME.to_string(),
                config: Box::new(config),
            }]
        })
    }
}

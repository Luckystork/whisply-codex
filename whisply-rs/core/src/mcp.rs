use std::collections::HashMap;
use std::sync::Arc;

use crate::config::Config;
use whisply_config::McpServerConfig;
use whisply_connectors::ConnectorRuntimeManager;
use whisply_connectors::ConnectorSnapshot;
use whisply_connectors::PluginConnectorSource;
use whisply_core_plugins::PluginsManager;
use whisply_exec_server::ExecutorCapabilityDiscoverySnapshot;
use whisply_extension_api::ExtensionData;
use whisply_extension_api::ExtensionDataInit;
use whisply_extension_api::ExtensionRegistry;
use whisply_extension_api::McpServerContribution;
use whisply_extension_api::McpServerContributionContext;
use whisply_login::CodexAuth;
use whisply_mcp::CODEX_APPS_MCP_SERVER_NAME;
use whisply_mcp::EffectiveMcpServer;
use whisply_mcp::McpConfig;
use whisply_mcp::McpPluginAttribution;
use whisply_mcp::McpServerConflict;
use whisply_mcp::McpServerRegistration;
use whisply_mcp::McpToolCatalogCache;
use whisply_mcp::ToolInfo;
use whisply_mcp::configured_mcp_servers;
use whisply_mcp::effective_mcp_servers;
use whisply_mcp::is_whisply_rejected_mcp_server;
use whisply_plugin::AppConnectorId;
use whisply_protocol::capabilities::SelectedCapabilityRoot;
use whisply_protocol::protocol::SessionSource;

const LEGACY_CODEX_APPS_REGISTRATION_ID: &str = "legacy_codex_apps";
const WHISPLY_MCP_ADMISSION_FILTER_REGISTRATION_ID: &str = "whisply_mcp_admission_filter";

/// MCP configuration and capability availability derived from the same inputs.
#[derive(Clone)]
pub(crate) struct McpRuntimeProjection {
    pub(crate) config: McpConfig,
    pub(crate) plugins_available: bool,
}

pub(crate) struct McpThreadIdentity<'a> {
    pub(crate) session_source: &'a SessionSource,
    pub(crate) originator: &'a str,
}

enum OrderedMcpOverlay {
    Set {
        contributor_id: &'static str,
        contribution_order: usize,
        name: String,
        config: Box<McpServerConfig>,
    },
    Remove {
        contributor_id: &'static str,
        contribution_order: usize,
        name: String,
    },
}

#[derive(Clone)]
pub struct McpManager {
    plugins_manager: Arc<PluginsManager>,
    extensions: Arc<ExtensionRegistry<Config>>,
    codex_apps_tools_cache: ConnectorRuntimeManager<ToolInfo>,
    tool_catalog_cache: McpToolCatalogCache,
}

impl McpManager {
    pub fn new(plugins_manager: Arc<PluginsManager>) -> Self {
        Self::new_with_extensions(
            plugins_manager,
            whisply_extension_api::empty_extension_registry(),
            ConnectorRuntimeManager::default(),
        )
    }

    /// Creates a manager that resolves host-installed MCP contributions.
    pub fn new_with_extensions(
        plugins_manager: Arc<PluginsManager>,
        extensions: Arc<ExtensionRegistry<Config>>,
        codex_apps_tools_cache: ConnectorRuntimeManager<ToolInfo>,
    ) -> Self {
        Self {
            plugins_manager,
            extensions,
            codex_apps_tools_cache,
            tool_catalog_cache: McpToolCatalogCache::default(),
        }
    }

    pub fn codex_apps_tools_cache(&self) -> ConnectorRuntimeManager<ToolInfo> {
        self.codex_apps_tools_cache.clone()
    }

    pub fn tool_catalog_cache(&self) -> McpToolCatalogCache {
        self.tool_catalog_cache.clone()
    }

    /// Returns the MCP config after applying compatibility built-ins and
    /// runtime-only extension overlays.
    pub async fn runtime_config(&self, config: &Config) -> McpConfig {
        self.runtime_config_with_context(
            McpServerContributionContext::global(config),
            // Threadless discovery and control-plane paths have no effective thread
            // originator; active-thread tool calls use runtime_config_for_step below.
            /*originator*/
            None,
        )
        .await
        .config
    }

    #[tracing::instrument(name = "mcp.runtime_config.project_for_step", skip_all)]
    pub(crate) async fn runtime_config_for_step(
        &self,
        config: &Config,
        thread_init: &ExtensionDataInit,
        thread_store: &ExtensionData,
        identity: McpThreadIdentity<'_>,
        ready_selected_capability_roots: &[SelectedCapabilityRoot],
        executor_capability_discovery: Option<&ExecutorCapabilityDiscoverySnapshot>,
    ) -> McpRuntimeProjection {
        self.runtime_config_with_context(
            McpServerContributionContext::for_step(
                config,
                thread_init,
                thread_store,
                identity.originator,
                ready_selected_capability_roots,
                executor_capability_discovery,
            )
            .with_session_source(identity.session_source),
            Some(identity.originator),
        )
        .await
    }

    async fn runtime_config_with_context(
        &self,
        context: McpServerContributionContext<'_, Config>,
        _originator: Option<&str>,
    ) -> McpRuntimeProjection {
        let config = context.config();
        let mut selected_plugin_available = false;
        let mut selected_plugin_connector_sources = Vec::new();
        let mut selected_plugin_registrations = Vec::new();
        let mut overlays = Vec::new();
        // A contributor can emit multiple ordered actions, so order each action globally rather
        // than enumerating contributors.
        let mut contribution_order = 0;
        for contributor in self.extensions.mcp_server_contributors() {
            for contribution in contributor.contribute(context).await {
                match contribution {
                    McpServerContribution::Set { name, config } => {
                        overlays.push(OrderedMcpOverlay::Set {
                            contributor_id: contributor.id(),
                            contribution_order,
                            name,
                            config,
                        });
                    }
                    McpServerContribution::SelectedPlugin {
                        name,
                        plugin_id,
                        plugin_display_name,
                        selection_order,
                        config,
                    } => selected_plugin_registrations.push(
                        McpServerRegistration::from_selected_plugin(
                            name,
                            McpPluginAttribution::new(plugin_id, plugin_display_name),
                            selection_order,
                            *config,
                        ),
                    ),
                    McpServerContribution::SelectedPluginPackage {
                        plugin_id,
                        plugin_display_name,
                        connector_ids,
                    } => {
                        selected_plugin_available = true;
                        if !connector_ids.is_empty() {
                            selected_plugin_connector_sources.push(
                                PluginConnectorSource::from_connector_ids(
                                    plugin_id,
                                    plugin_display_name,
                                    connector_ids.into_iter().map(AppConnectorId),
                                ),
                            );
                        }
                    }
                    McpServerContribution::Remove { name } => {
                        overlays.push(OrderedMcpOverlay::Remove {
                            contributor_id: contributor.id(),
                            contribution_order,
                            name,
                        });
                    }
                }
                contribution_order += 1;
            }
        }

        let loaded_plugins = self
            .plugins_manager
            .plugins_for_config(&config.plugins_config_input())
            .await;
        let plugins_available =
            selected_plugin_available || !loaded_plugins.capability_summaries().is_empty();
        let mut mcp_config = config
            .to_mcp_config_with_loaded_plugins(&loaded_plugins, selected_plugin_registrations);
        let mut catalog = mcp_config.mcp_server_catalog.to_builder();
        // Whisply does not delegate connector authority to the host-owned
        // ChatGPT Apps MCP. Keep only the removal keyed to its compatibility
        // registration so configured and plugin-contributed MCP servers retain
        // their normal resolution boundaries.
        catalog.remove_compatibility(
            CODEX_APPS_MCP_SERVER_NAME.to_string(),
            LEGACY_CODEX_APPS_REGISTRATION_ID,
        );

        for overlay in overlays {
            match overlay {
                OrderedMcpOverlay::Set {
                    contributor_id,
                    contribution_order,
                    name,
                    config,
                } => catalog.register(McpServerRegistration::from_extension(
                    name,
                    contributor_id,
                    contribution_order,
                    *config,
                )),
                OrderedMcpOverlay::Remove {
                    contributor_id,
                    contribution_order,
                    name,
                } => catalog.remove_extension(name, contributor_id, contribution_order),
            }
        }
        // Plugin manifests, executor-selected plugins, and extensions can
        // deserialize McpServerConfig without passing through ConfigToml. Apply
        // this source-independent admission filter only after every source has
        // resolved. It prevents either the host-owned name or ChatGPT auth from
        // materializing into a runtime MCP, while retaining ordinary OAuth and
        // local servers.
        let resolved_catalog = catalog.build();
        let rejected_server_names = resolved_catalog
            .configured_servers()
            .into_iter()
            .filter_map(|(name, server)| {
                is_whisply_rejected_mcp_server(&name, &server).then_some(name)
            })
            .collect::<Vec<_>>();
        let mut catalog = resolved_catalog.to_builder();
        for (filter_order, name) in rejected_server_names.into_iter().enumerate() {
            tracing::warn!(
                server = name,
                "Whisply rejected a host-owned or ChatGPT-authenticated MCP server"
            );
            catalog.remove_extension(
                name,
                WHISPLY_MCP_ADMISSION_FILTER_REGISTRATION_ID,
                contribution_order + filter_order,
            );
        }
        let catalog = catalog.build();
        for conflict in catalog.conflicts() {
            tracing::warn!(
                server = conflict.name,
                outcome = ?conflict.outcome,
                contenders = ?conflict.contenders,
                "conflicting MCP server actions; using resolved catalog outcome"
            );
        }
        mcp_config.mcp_server_catalog = catalog;
        mcp_config.connector_snapshot =
            mcp_config
                .connector_snapshot
                .merged_with(&ConnectorSnapshot::from_plugin_sources(
                    selected_plugin_connector_sources,
                ));
        McpRuntimeProjection {
            config: mcp_config,
            plugins_available,
        }
    }

    /// Returns config- and plugin-backed servers without runtime contributions.
    pub async fn configured_servers(&self, config: &Config) -> HashMap<String, McpServerConfig> {
        self.configured_servers_and_conflicts(config).await.0
    }

    /// The same servers, plus the name collisions resolving them produced.
    ///
    /// Resolution already puts configuration above plugins, so a plugin cannot
    /// take over a name the user configured. What was missing is the user
    /// hearing about it: a collision was recorded and logged, which means it
    /// existed only for whoever was reading trace output.
    pub async fn configured_servers_and_conflicts(
        &self,
        config: &Config,
    ) -> (HashMap<String, McpServerConfig>, Vec<McpServerConflict>) {
        let mcp_config = config.to_mcp_config(self.plugins_manager.as_ref()).await;
        let conflicts = mcp_config.mcp_server_catalog.conflicts().to_vec();
        (configured_mcp_servers(&mcp_config), conflicts)
    }

    /// Returns configured and host-contributed servers before auth gating.
    pub async fn runtime_servers(&self, config: &Config) -> HashMap<String, McpServerConfig> {
        let mcp_config = self.runtime_config(config).await;
        configured_mcp_servers(&mcp_config)
    }

    /// Returns runtime servers after auth gating and compatibility built-ins.
    pub async fn effective_servers(
        &self,
        config: &Config,
        auth: Option<&CodexAuth>,
    ) -> HashMap<String, EffectiveMcpServer> {
        let mcp_config = self.runtime_config(config).await;
        effective_mcp_servers(&mcp_config, auth)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ConfigBuilder;
    use serde_json::json;
    use std::collections::HashMap;
    use tempfile::TempDir;
    use whisply_extension_api::ExtensionRegistryBuilder;
    use whisply_extension_api::McpServerContribution;
    use whisply_extension_api::McpServerContributionContext;
    use whisply_extension_api::McpServerContributor;
    use whisply_login::AuthCredentialsStoreMode;
    use whisply_login::AuthDotJson;
    use whisply_login::AuthKeyringBackendKind;
    use whisply_login::CodexAuth;
    use whisply_login::save_auth;
    use whisply_login::token_data::TokenData;
    use whisply_login::token_data::parse_chatgpt_jwt_claims;
    use whisply_protocol::auth::AuthMode;
    use wiremock::MockServer;

    #[tokio::test]
    async fn runtime_projection_omits_host_owned_apps_with_chatgpt_auth() -> anyhow::Result<()> {
        let codex_home = TempDir::new()?;
        let mut config = ConfigBuilder::default()
            .codex_home(codex_home.path().to_path_buf())
            .build()
            .await?;
        let external_server: McpServerConfig = serde_json::from_value(json!({
            "command": "external-mcp"
        }))?;
        let reserved_server: McpServerConfig = serde_json::from_value(json!({
            "url": "https://example.invalid/mcp"
        }))?;
        config.mcp_servers.set(HashMap::from([
            (CODEX_APPS_MCP_SERVER_NAME.to_string(), reserved_server),
            ("external".to_string(), external_server),
        ]))?;
        let manager = McpManager::new(Arc::new(PluginsManager::new(
            config.codex_home.to_path_buf(),
        )));
        let legacy_chatgpt_auth = CodexAuth::from_external_chatgpt_tokens(
            "header.e30.signature",
            "legacy-account",
            /*chatgpt_plan_type*/ None,
        )?;

        let runtime_config = manager.runtime_config(&config).await;
        assert!(
            runtime_config
                .mcp_server_catalog
                .server(CODEX_APPS_MCP_SERVER_NAME)
                .is_none(),
            "Whisply must not admit a configured or host-owned Apps MCP"
        );
        assert!(
            runtime_config
                .mcp_server_catalog
                .server("external")
                .is_some(),
            "removing host-owned Apps must not remove configured external MCP servers"
        );
        assert!(
            manager
                .effective_servers(&config, Some(&legacy_chatgpt_auth))
                .await
                .get(CODEX_APPS_MCP_SERVER_NAME)
                .is_none(),
            "legacy ChatGPT auth must not reactivate host-owned Apps"
        );
        assert!(
            manager
                .effective_servers(&config, Some(&legacy_chatgpt_auth))
                .await
                .contains_key("external"),
            "legacy auth must not remove configured external MCP servers"
        );
        Ok(())
    }

    #[tokio::test]
    async fn runtime_projection_rejects_selected_plugin_and_extension_chatgpt_aliases()
    -> anyhow::Result<()> {
        let codex_home = TempDir::new()?;
        let remote = MockServer::start().await;
        let config = ConfigBuilder::default()
            .codex_home(codex_home.path().to_path_buf())
            .fallback_cwd(Some(codex_home.path().to_path_buf()))
            .build()
            .await?;
        let auth = persisted_chatgpt_auth(codex_home.path()).await?;
        let mut registry = ExtensionRegistryBuilder::new();
        registry.mcp_server_contributor(Arc::new(SelectedExecutorPluginChatgptAlias {
            url: remote.uri(),
        }));
        registry.mcp_server_contributor(Arc::new(ArbitraryExtensionChatgptAlias {
            url: remote.uri(),
        }));
        let manager = McpManager::new_with_extensions(
            Arc::new(PluginsManager::new(config.codex_home.to_path_buf())),
            Arc::new(registry.build()),
            ConnectorRuntimeManager::default(),
        );

        let runtime_servers = manager.runtime_servers(&config).await;
        let effective_servers = manager.effective_servers(&config, Some(&auth)).await;

        for name in [
            "selected_executor_plugin_chatgpt_alias",
            "arbitrary_extension_chatgpt_alias",
        ] {
            assert!(
                !runtime_servers.contains_key(name),
                "the resolved runtime catalog must reject {name} before auth gating"
            );
            assert!(
                !effective_servers.contains_key(name),
                "persisted legacy ChatGPT auth must not reactivate {name}"
            );
        }
        assert!(
            remote
                .received_requests()
                .await
                .expect("wiremock should retain request history")
                .is_empty(),
            "the admission filter must prevent all HTTP to rejected MCP aliases"
        );
        Ok(())
    }

    async fn persisted_chatgpt_auth(codex_home: &std::path::Path) -> anyhow::Result<CodexAuth> {
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
            &whisply_login::test_support::transport_default_auth_route_config(),
        )
        .await?
        .ok_or_else(|| anyhow::anyhow!("persisted ChatGPT auth was not loaded"))
    }

    struct SelectedExecutorPluginChatgptAlias {
        url: String,
    }

    impl McpServerContributor<Config> for SelectedExecutorPluginChatgptAlias {
        fn id(&self) -> &'static str {
            "selected_executor_plugin_chatgpt_alias"
        }

        fn contribute<'a>(
            &'a self,
            _context: McpServerContributionContext<'a, Config>,
        ) -> whisply_extension_api::ExtensionFuture<'a, Vec<McpServerContribution>> {
            let url = self.url.clone();
            Box::pin(async move {
                let config = serde_json::from_value(json!({
                    "url": format!("{url}/backend-api/ps/mcp"),
                    "auth": "chatgpt",
                }))
                .expect("test MCP config should deserialize");
                vec![McpServerContribution::SelectedPlugin {
                    name: "selected_executor_plugin_chatgpt_alias".to_string(),
                    plugin_id: "selected-executor-plugin".to_string(),
                    plugin_display_name: "Selected Executor Plugin".to_string(),
                    selection_order: 0,
                    config: Box::new(config),
                }]
            })
        }
    }

    struct ArbitraryExtensionChatgptAlias {
        url: String,
    }

    impl McpServerContributor<Config> for ArbitraryExtensionChatgptAlias {
        fn id(&self) -> &'static str {
            "arbitrary_extension_chatgpt_alias"
        }

        fn contribute<'a>(
            &'a self,
            _context: McpServerContributionContext<'a, Config>,
        ) -> whisply_extension_api::ExtensionFuture<'a, Vec<McpServerContribution>> {
            let url = self.url.clone();
            Box::pin(async move {
                let config = serde_json::from_value(json!({
                    "url": format!("{url}/backend-api/ps/mcp"),
                    "auth": "chatgpt",
                }))
                .expect("test MCP config should deserialize");
                vec![McpServerContribution::Set {
                    name: "arbitrary_extension_chatgpt_alias".to_string(),
                    config: Box::new(config),
                }]
            })
        }
    }
}

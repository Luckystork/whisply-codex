use whisply_core::config::Config;
use whisply_extension_api::ExtensionFuture;
use whisply_extension_api::ExtensionRegistryBuilder;
use whisply_extension_api::McpServerContribution;
use whisply_extension_api::McpServerContributionContext;
use whisply_extension_api::McpServerContributor;
use whisply_mcp::CODEX_APPS_MCP_SERVER_NAME;

mod executor_plugin;

struct HostedPluginRuntimeExtension;

impl McpServerContributor<Config> for HostedPluginRuntimeExtension {
    fn id(&self) -> &'static str {
        "hosted_plugin_runtime"
    }

    fn contribute<'a>(
        &'a self,
        _context: McpServerContributionContext<'a, Config>,
    ) -> ExtensionFuture<'a, Vec<McpServerContribution>> {
        Box::pin(async move {
            // `codex_apps` is reserved for the old host-owned ChatGPT connector
            // runtime. Whisply has no native broker snapshot to materialize here,
            // so reject the compatibility contribution at its source. McpManager
            // repeats the name veto after every overlay for configured or later
            // extension contributions.
            vec![McpServerContribution::Remove {
                name: CODEX_APPS_MCP_SERVER_NAME.to_string(),
            }]
        })
    }
}

pub fn install(builder: &mut ExtensionRegistryBuilder<Config>) {
    builder.mcp_server_contributor(std::sync::Arc::new(HostedPluginRuntimeExtension));
}

/// Installs discovery for MCP servers declared by thread-selected executor plugins.
pub fn install_executor_plugins(
    builder: &mut ExtensionRegistryBuilder<Config>,
    environment_manager: std::sync::Arc<whisply_exec_server::EnvironmentManager>,
) {
    builder.mcp_server_contributor(std::sync::Arc::new(
        executor_plugin::SelectedExecutorPluginMcpContributor::new(environment_manager),
    ));
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;

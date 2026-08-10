use super::*;
use codex_core::config::ConfigBuilder;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn hosted_plugin_runtime_rejects_host_owned_apps_mcp()
-> Result<(), Box<dyn std::error::Error>> {
    let codex_home = tempfile::tempdir()?;
    let config = ConfigBuilder::default()
        .codex_home(codex_home.path().to_path_buf())
        .fallback_cwd(Some(codex_home.path().to_path_buf()))
        .cli_overrides(vec![("features.apps".to_string(), true.into())])
        .build()
        .await?;

    let contributions = HostedPluginRuntimeExtension
        .contribute(McpServerContributionContext::global(&config))
        .await;
    let [McpServerContribution::Remove { name }] = contributions.as_slice() else {
        panic!("Whisply should reject the host-owned Apps MCP contribution");
    };
    assert_eq!(name, CODEX_APPS_MCP_SERVER_NAME);

    Ok(())
}

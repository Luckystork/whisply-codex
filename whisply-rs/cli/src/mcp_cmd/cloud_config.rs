use anyhow::Context;
use anyhow::Result;
use whisply_config::CloudConfigBundleLoader;
use whisply_core::config::Config;
use whisply_core::config::ConfigBuilder;
use whisply_core::config::LoaderOverrides;
use whisply_core::config::find_codex_home;
use whisply_utils_cli::CliConfigOverrides;

pub(super) async fn load_mcp_config(
    config_overrides: &CliConfigOverrides,
    loader_overrides: LoaderOverrides,
) -> Result<Config> {
    let cli_overrides = config_overrides
        .parse_overrides()
        .map_err(anyhow::Error::msg)?;
    let codex_home = find_codex_home().context("failed to resolve WHISPLY_HOME")?;

    // MCP configuration keeps static local, MDM, and project layers without
    // constructing an authenticated cloud-config loader.
    ConfigBuilder::default()
        .codex_home(codex_home.to_path_buf())
        .cli_overrides(cli_overrides)
        .loader_overrides(loader_overrides)
        .cloud_config_bundle(CloudConfigBundleLoader::default())
        .build()
        .await
        .context("failed to load configuration")
}

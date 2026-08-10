use anyhow::Context;
use anyhow::Result;
use codex_config::CloudConfigBundleLoader;
use codex_core::config::Config;
use codex_core::config::ConfigBuilder;
use codex_core::config::LoaderOverrides;
use codex_core::config::find_codex_home;
use codex_utils_cli::CliConfigOverrides;

pub(super) async fn load_mcp_config(
    config_overrides: &CliConfigOverrides,
    loader_overrides: LoaderOverrides,
) -> Result<Config> {
    let cli_overrides = config_overrides
        .parse_overrides()
        .map_err(anyhow::Error::msg)?;
    let codex_home = find_codex_home().context("failed to resolve CODEX_HOME")?;

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

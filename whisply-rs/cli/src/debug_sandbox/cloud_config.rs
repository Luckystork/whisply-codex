use toml::Value as TomlValue;
use whisply_config::CloudConfigBundleLoader;
use whisply_utils_absolute_path::AbsolutePathBuf;

use super::DebugSandboxConfigOptions;

pub(super) async fn bootstrap_cloud_config_bundle(
    _cli_overrides: &[(String, TomlValue)],
    _options: &DebugSandboxConfigOptions,
    _resolve_codex_home: impl FnOnce() -> std::io::Result<AbsolutePathBuf>,
    _strict_config: bool,
) -> anyhow::Result<CloudConfigBundleLoader> {
    // Whisply's public runtime has one authenticated authority: the verified
    // native broker plus its release-owned gateway descriptor. In particular,
    // `codex sandbox` must not bootstrap an upstream cloud bundle through a
    // local auth store, API key, or configurable ChatGPT base URL.
    Ok(CloudConfigBundleLoader::default())
}

#[cfg(test)]
#[path = "cloud_config_tests.rs"]
mod tests;

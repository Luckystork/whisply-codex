use anyhow::Result;
use tempfile::TempDir;
use whisply_config::LoaderOverrides;

use super::super::DebugSandboxConfigOptions;
use super::super::ManagedRequirementsMode;
use super::bootstrap_cloud_config_bundle;

#[tokio::test]
async fn debug_sandbox_never_bootstraps_remote_cloud_managed_config() -> Result<()> {
    let codex_home = TempDir::new()?;
    let options = DebugSandboxConfigOptions {
        sandbox_state: Default::default(),
        permissions_profile: Some("managed-cloud".to_string()),
        cwd: Some(codex_home.path().to_path_buf()),
        managed_requirements_mode: ManagedRequirementsMode::Include,
        loader_overrides: LoaderOverrides::without_managed_config_for_tests(),
    };
    let cloud_config_bundle = bootstrap_cloud_config_bundle(
        &[],
        &options,
        || panic!("managed cloud bootstrap must not resolve a local auth home"),
        /*strict_config*/ false,
    )
    .await?;

    assert!(cloud_config_bundle.get().await?.is_none());
    assert!(
        !codex_home
            .path()
            .join("cloud-config-bundle-cache.json")
            .exists()
    );

    Ok(())
}

use anyhow::Result;
use app_test_support::ManagedWhisplyConfig;
use tempfile::TempDir;
use tokio::process::Command;
use whisply_features::Feature;

#[tokio::test]
async fn app_server_rejects_remote_code_mode_host_in_broker_only() -> Result<()> {
    let codex_home = TempDir::new()?;
    ManagedWhisplyConfig::new()
        .enable_feature(Feature::CodeModeOnly)
        .write(codex_home.path())?;
    let config_path = codex_home.path().join("config.toml");
    let original_config = std::fs::read_to_string(&config_path)?;

    let output = Command::new(whisply_utils_cargo_bin::cargo_bin("codex-app-server")?)
        .args(["--code-mode-host", "ws://127.0.0.1:0"])
        .current_dir(codex_home.path())
        .env("WHISPLY_HOME", codex_home.path())
        .env_remove("WHISPLY_HOME")
        .env(
            "CODEX_APP_SERVER_MANAGED_CONFIG_PATH",
            codex_home.path().join("managed_config.toml"),
        )
        .output()
        .await?;

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("remote code-mode hosts are unavailable in BrokerOnly")
    );
    assert_eq!(std::fs::read_to_string(config_path)?, original_config);

    Ok(())
}

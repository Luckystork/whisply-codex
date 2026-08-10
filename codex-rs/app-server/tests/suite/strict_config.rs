use std::process::Command;

use anyhow::Result;
use tempfile::TempDir;

#[test]
fn strict_config_rejects_unknown_config_fields_for_standalone_app_server() -> Result<()> {
    let codex_home = TempDir::new()?;
    std::fs::write(
        codex_home.path().join("config.toml"),
        r#"
foo = "bar"
"#,
    )?;

    let output = Command::new(codex_utils_cargo_bin::cargo_bin("codex-app-server")?)
        .env("WHISPLY_HOME", codex_home.path())
        .env_remove("CODEX_HOME")
        .env(
            "CODEX_APP_SERVER_MANAGED_CONFIG_PATH",
            codex_home.path().join("managed_config.toml"),
        )
        .args(["--strict-config", "--listen", "off"])
        .output()?;

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr)?;
    assert!(
        stderr.contains("unknown configuration field `foo`"),
        "expected strict config error in stderr, got: {stderr}"
    );

    Ok(())
}

#[test]
fn broker_only_standalone_app_server_ignores_legacy_auth_requirements() -> Result<()> {
    for requirements in [
        "allowed_login_methods = []\n",
        "allowed_login_methods = [\"chatgpt\"]\nallowed_chatgpt_workspaces = []\n",
    ] {
        let codex_home = TempDir::new()?;
        std::fs::write(codex_home.path().join("requirements.toml"), requirements)?;

        let output = Command::new(codex_utils_cargo_bin::cargo_bin("codex-app-server")?)
            .env("WHISPLY_HOME", codex_home.path())
            .env_remove("CODEX_HOME")
            .env(
                "CODEX_APP_SERVER_MANAGED_CONFIG_PATH",
                codex_home.path().join("managed_config.toml"),
            )
            // BrokerOnly deliberately does not materialize legacy direct
            // credentials, so legacy login requirements must not make the
            // standalone managed runtime unavailable.
            .args(["--listen", "stdio://"])
            .output()?;

        assert!(output.status.success());
        let stderr = String::from_utf8(output.stderr)?;
        assert!(
            !stderr.contains("authentication requirements do not permit any usable login method"),
            "BrokerOnly must not re-enable legacy auth validation: {stderr}"
        );
    }

    Ok(())
}

//! Public CLI provider authority is fixed to the authenticated Whisply
//! gateway. These integration checks deliberately run without broker FDs: a
//! forbidden configuration must fail before any request could be attempted.

use std::path::Path;

use anyhow::Result;
use predicates::prelude::*;
use tempfile::TempDir;

fn whisply_command(product_home: &Path) -> Result<assert_cmd::Command> {
    let mut command = assert_cmd::Command::new(whisply_utils_cargo_bin::cargo_bin("whisply")?);
    command
        .env("WHISPLY_HOME", product_home)
        .env_remove("WHISPLY_GATEWAY_AUTH_FD")
        .env_remove("WHISPLY_GATEWAY_ENDPOINT_FD")
        .env_remove("WHISPLY_NATIVE_BROKER_CAPABILITY_FD")
        .env_remove("WHISPLY_NATIVE_BROKER_SOCKET")
        .env_remove("WHISPLY_RELEASE_MANIFEST_SHA256")
        .env_remove("OPENAI_API_KEY")
        .env_remove("CODEX_ACCESS_TOKEN");
    Ok(command)
}

#[test]
fn exec_rejects_cli_model_provider_override_before_execution() -> Result<()> {
    let product_home = TempDir::new()?;

    whisply_command(product_home.path())?
        .args([
            "-c",
            "model_provider=\"openai\"",
            "exec",
            "--skip-git-repo-check",
            "never reach a provider",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("managed gateway"));

    Ok(())
}

#[test]
fn exec_rejects_configured_remote_base_urls_and_custom_provider_maps() -> Result<()> {
    let product_home = TempDir::new()?;
    std::fs::write(
        product_home.path().join("config.toml"),
        r#"
openai_base_url = "https://attacker.invalid/v1"

[model_providers.attacker]
name = "attacker"
base_url = "https://attacker.invalid/v1"
env_key = "ATTACKER_TOKEN"
wire_api = "responses"
"#,
    )?;

    whisply_command(product_home.path())?
        .args(["exec", "--skip-git-repo-check", "never reach a provider"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("base URLs"))
        .stderr(predicate::str::contains("ATTACKER_TOKEN").not());

    Ok(())
}

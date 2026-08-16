//! Public login is deliberately broker-owned. These tests guard against the
//! removed upstream credential and ChatGPT/OAuth paths without pretending a
//! fixture process can satisfy the native peer/manifest/capability checks.

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
fn login_rejects_all_raw_credential_and_custom_oauth_paths() -> Result<()> {
    let product_home = TempDir::new()?;
    for arguments in [
        vec!["login", "--with-api-key"],
        vec!["login", "--with-access-token"],
        vec!["login", "--api-key=test-secret"],
        vec!["login", "--device-auth"],
        vec!["login", "--experimental_issuer", "https://attacker.invalid"],
    ] {
        whisply_command(product_home.path())?
            .args(arguments)
            .write_stdin("test-secret\n")
            .assert()
            .code(2)
            .stderr(predicate::str::contains(
                "does not accept API keys, access tokens, or custom OAuth settings",
            ))
            .stderr(predicate::str::contains("test-secret").not());
    }
    assert!(!product_home.path().join("auth.json").exists());
    Ok(())
}

#[test]
fn login_status_requires_the_managed_native_broker_not_local_auth_storage() -> Result<()> {
    let product_home = TempDir::new()?;
    std::fs::write(product_home.path().join("auth.json"), "{not broker auth}")?;

    whisply_command(product_home.path())?
        .args(["login", "status"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "managed Whisply broker is unavailable",
        ));

    Ok(())
}

#[test]
fn logout_and_whoami_require_the_managed_native_broker() -> Result<()> {
    let product_home = TempDir::new()?;
    for arguments in [vec!["logout"], vec!["whoami"], vec!["login", "whoami"]] {
        whisply_command(product_home.path())?
            .args(arguments)
            .assert()
            .failure()
            .stderr(predicate::str::contains(
                "managed Whisply broker is unavailable",
            ));
    }
    Ok(())
}

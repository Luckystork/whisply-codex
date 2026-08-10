use anyhow::Result;
use codex_config::MarketplaceConfigUpdate;
use codex_config::record_user_marketplace;
use predicates::str::contains;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::path::Path;
use tempfile::TempDir;

fn codex_command(codex_home: &Path) -> Result<assert_cmd::Command> {
    let mut cmd = assert_cmd::Command::new(codex_utils_cargo_bin::cargo_bin("whisply")?);
    cmd.env("CODEX_HOME", codex_home)
        .env("WHISPLY_HOME", codex_home);
    Ok(cmd)
}

#[tokio::test]
async fn marketplace_upgrade_runs_under_plugin() -> Result<()> {
    let codex_home = TempDir::new()?;

    codex_command(codex_home.path())?
        .args(["plugin", "marketplace", "upgrade"])
        .assert()
        .success()
        .stdout(contains("No configured Git marketplaces to upgrade."));

    Ok(())
}

#[tokio::test]
async fn marketplace_upgrade_json_prints_upgrade_outcome() -> Result<()> {
    let codex_home = TempDir::new()?;

    let assert = codex_command(codex_home.path())?
        .args(["plugin", "marketplace", "upgrade", "--json"])
        .assert()
        .success();
    let stdout = assert.get_output().stdout.as_slice();
    let actual: serde_json::Value = serde_json::from_slice(stdout)?;

    assert_eq!(
        actual,
        json!({
            "selectedMarketplaces": [],
            "upgradedRoots": [],
            "errors": [],
        })
    );

    Ok(())
}

#[tokio::test]
async fn marketplace_upgrade_keeps_local_only_state_as_a_noop() -> Result<()> {
    let codex_home = TempDir::new()?;
    let local_source = TempDir::new()?;
    let local_source = local_source.path().display().to_string();
    record_user_marketplace(
        codex_home.path(),
        "local-only",
        &MarketplaceConfigUpdate {
            last_updated: "2026-08-09T00:00:00Z",
            last_revision: None,
            source_type: "local",
            source: &local_source,
            ref_name: None,
            sparse_paths: &[],
        },
    )?;

    codex_command(codex_home.path())?
        .args(["plugin", "marketplace", "upgrade"])
        .assert()
        .success()
        .stdout(contains("No configured Git marketplaces to upgrade."));

    Ok(())
}

#[tokio::test]
async fn marketplace_upgrade_no_longer_runs_at_top_level() -> Result<()> {
    let codex_home = TempDir::new()?;

    codex_command(codex_home.path())?
        .args(["marketplace", "upgrade"])
        .assert()
        .failure()
        .stderr(contains("unrecognized subcommand 'upgrade'"));

    Ok(())
}

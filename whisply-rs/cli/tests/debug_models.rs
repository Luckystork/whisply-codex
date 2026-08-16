use std::path::Path;

use anyhow::Result;
use tempfile::TempDir;

fn codex_command(codex_home: &Path) -> Result<assert_cmd::Command> {
    let mut cmd = assert_cmd::Command::new(whisply_utils_cargo_bin::cargo_bin("whisply")?);
    cmd.env("WHISPLY_HOME", codex_home);
    Ok(cmd)
}

#[test]
fn debug_models_rejects_unsigned_bundled_fallback() -> Result<()> {
    let codex_home = TempDir::new()?;
    let mut cmd = codex_command(codex_home.path())?;
    let output = cmd.args(["debug", "models", "--bundled"]).output()?;

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr)?;
    assert!(stderr.contains("--bundled"));

    Ok(())
}

#[test]
fn debug_models_requires_the_managed_signed_catalog() -> Result<()> {
    let codex_home = TempDir::new()?;
    let mut cmd = codex_command(codex_home.path())?;
    let output = cmd.args(["debug", "models"]).output()?;

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr)?;
    assert!(stderr.contains("signed Whisply model catalog") || stderr.contains("managed app"));

    Ok(())
}

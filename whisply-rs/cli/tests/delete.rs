use predicates::prelude::*;

#[test]
fn missing_session_fails_before_delete_confirmation() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let canonical_home = codex_home.path().canonicalize()?;
    let mut cmd = assert_cmd::Command::new(whisply_utils_cargo_bin::cargo_bin("whisply")?);
    cmd.env("WHISPLY_HOME", &canonical_home)
        .env("WHISPLY_HOME", &canonical_home)
        .args(["delete", "123e4567-e89b-12d3-a456-426614174000"]);

    cmd.assert()
        .failure()
        .stderr(predicate::str::contains(
            "No active or archived session found matching",
        ))
        .stderr(predicate::str::contains("cannot confirm").not());
    Ok(())
}

use std::process::Command;

use anyhow::Result;
use tempfile::TempDir;

const LOCAL_PERMISSION_PROFILE: &str = r#"
default_permissions = "local-workspace"

[permissions.local-workspace]
extends = ":workspace"

[permissions.local-workspace.network]
enabled = false
"#;

#[test]
fn sandbox_enforces_local_permission_profile_without_managed_config_io() -> Result<()> {
    let codex_home = TempDir::new()?;
    let config_path = codex_home.path().join("config.toml");
    std::fs::write(&config_path, LOCAL_PERMISSION_PROFILE)?;

    let codex = whisply_utils_cargo_bin::cargo_bin("whisply")?;
    let output = Command::new(&codex)
        .current_dir(codex_home.path())
        .env("WHISPLY_HOME", codex_home.path())
        .env("WHISPLY_HOME", codex_home.path())
        .env_remove("CODEX_ACCESS_TOKEN")
        .env_remove("CODEX_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .args(["sandbox", "--permission-profile", "local-workspace", "--"])
        .arg(&codex)
        .arg("--version")
        .output()?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    let nested_macos_sandbox_unavailable = cfg!(target_os = "macos")
        && output.status.code() == Some(71)
        && stderr.contains("sandbox-exec: sandbox_apply: Operation not permitted");

    assert!(
        output.status.success() || nested_macos_sandbox_unavailable,
        "local sandbox profile was not enforced: status={:?}; stdout={}; stderr={stderr}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
    );
    if !nested_macos_sandbox_unavailable {
        assert!(
            String::from_utf8(output.stdout)?.starts_with("codex"),
            "expected the sandboxed Whisply version command to run"
        );
    }
    assert_eq!(
        std::fs::read_to_string(&config_path)?,
        LOCAL_PERMISSION_PROFILE
    );
    assert!(
        !codex_home
            .path()
            .join("cloud-config-bundle-cache.json")
            .exists(),
        "local sandbox execution must not create a cloud-config cache"
    );

    Ok(())
}

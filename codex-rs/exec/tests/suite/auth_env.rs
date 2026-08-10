#![allow(clippy::unwrap_used)]
use codex_login::CODEX_API_KEY_ENV_VAR;
use core_test_support::responses::ev_completed;
use core_test_support::responses::mount_sse_once_match;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::test_codex_exec::test_codex_exec;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exec_oss_ignores_codex_api_key_env_var() -> anyhow::Result<()> {
    let test = test_codex_exec();
    let server = start_mock_server().await;
    let repo_root = codex_utils_cargo_bin::repo_root()?;

    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| !request.headers.contains_key("Authorization"),
        sse(vec![ev_completed("request_0")]),
    )
    .await;

    test.cmd_with_server(&server)
        .env(CODEX_API_KEY_ENV_VAR, "dummy")
        .arg("--skip-git-repo-check")
        .arg("-C")
        .arg(&repo_root)
        .arg("echo testing codex api key")
        .assert()
        .success();

    Ok(())
}

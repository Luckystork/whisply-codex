#![cfg(not(target_os = "windows"))]
#![allow(clippy::unwrap_used)]

use core_test_support::responses;
use core_test_support::test_codex_exec::test_codex_exec;

async fn run_exec_with_config(config_toml: &str, extra_args: &[&str]) -> anyhow::Result<String> {
    let test = test_codex_exec();
    std::fs::write(test.home_path().join("config.toml"), config_toml)?;

    let server = responses::start_mock_server().await;
    let body = responses::sse(vec![
        responses::ev_response_created("response_1"),
        responses::ev_assistant_message("response_1", "done"),
        responses::ev_completed("response_1"),
    ]);
    responses::mount_sse_once(&server, body).await;

    let mut cmd = test.cmd_with_server(&server);
    let output = cmd
        .arg("--skip-git-repo-check")
        .args(extra_args)
        .arg("check approval mode")
        .output()?;

    assert!(output.status.success(), "exec run failed: {output:?}");

    Ok(String::from_utf8(output.stderr)?)
}

async fn run_exec_with_auto_review_config(extra_args: &[&str]) -> anyhow::Result<String> {
    run_exec_with_config(
        r#"
approval_policy = "on-request"
approvals_reviewer = "auto_review"
"#,
        extra_args,
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exec_preserves_on_request_for_auto_review_config() -> anyhow::Result<()> {
    let stderr = run_exec_with_auto_review_config(&[]).await?;
    assert!(
        stderr.contains("approval: on-request"),
        "stderr missing preserved auto-review approval mode: {stderr}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exec_approve_for_me_flag_sets_approval_mode_and_sandbox() -> anyhow::Result<()> {
    let stderr = run_exec_with_config("", &["--approve-for-me"]).await?;
    assert!(
        stderr.contains("approval: on-request"),
        "stderr missing --approve-for-me approval mode: {stderr}"
    );
    assert!(
        stderr.contains("sandbox: workspace-write"),
        "stderr missing --approve-for-me sandbox mode: {stderr}"
    );

    Ok(())
}

/// WCD-718: `whisply exec` runs where there is nobody to answer a question.
///
/// It can still reach a point where a human decision is the only thing left:
/// `approval_policy = "on-request"` is preserved for automatic review, a
/// command can ask for escalated permissions, and an automatic review that
/// cannot decide hands the question to the person. There is no person here.
///
/// The failure this pins is not a wrong answer, it is no answer -- a headless
/// run that sits forever waiting on a prompt nothing will display, in a
/// script, a hook, or CI, with no output saying why. The run has to end, and
/// the command that needed the decision has to not have run.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exec_declines_a_command_that_needs_a_person_rather_than_waiting_for_one()
-> anyhow::Result<()> {
    let test = test_codex_exec();
    std::fs::write(
        test.home_path().join("config.toml"),
        r#"
approval_policy = "on-request"
approvals_reviewer = "auto_review"
"#,
    )?;

    let marker = test.cwd_path().join("a-person-said-yes");
    let command = format!("touch {}", marker.display());
    let arguments = serde_json::to_string(&serde_json::json!({
        "command": command,
        "sandbox_permissions": "require_escalated",
        "justification": "needs a decision only a person can give",
    }))?;

    // The automatic review asks the model for a verdict and retries; none of
    // these is one, which is the case the requirement is about. A review that
    // reaches no answer must not become a yes, and here there is nobody to
    // turn the question over to.
    let undecided = || {
        responses::sse(vec![
            responses::ev_response_created("review"),
            responses::ev_assistant_message("review", "I am not sure."),
            responses::ev_completed("review"),
        ])
    };
    let server = responses::start_mock_server().await;
    responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![
                responses::ev_response_created("response_1"),
                responses::ev_function_call("call_1", "shell_command", &arguments),
                responses::ev_completed("response_1"),
            ]),
            undecided(),
            undecided(),
            undecided(),
            responses::sse(vec![
                responses::ev_response_created("response_2"),
                responses::ev_assistant_message("response_2", "done"),
                responses::ev_completed("response_2"),
            ]),
        ],
    )
    .await;

    let mut cmd = test.cmd_with_server(&server);
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        tokio::task::spawn_blocking(move || {
            cmd.arg("--skip-git-repo-check")
                .arg("run the command that needs a decision")
                .output()
        }),
    )
    .await
    .map_err(|_| {
        anyhow::anyhow!("exec waited for an approval nobody was there to give instead of finishing")
    })??;

    let output = output?;
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        output.status.success(),
        "exec run failed rather than declining: {output:?}"
    );
    assert!(
        stderr.contains("approval: on-request"),
        "exec did not keep the mode that can ask, so nothing asked: {stderr}"
    );
    assert!(
        !marker.exists(),
        "the command ran without the decision it asked for"
    );
    assert!(
        stderr.contains("touch") && !stderr.contains("succeeded"),
        "the command that needed a decision was not reported as refused: {stderr}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exec_bypass_preserves_never_for_auto_review_config() -> anyhow::Result<()> {
    let stderr =
        run_exec_with_auto_review_config(&["--dangerously-bypass-approvals-and-sandbox"]).await?;
    assert!(
        stderr.contains("approval: never"),
        "stderr missing bypass approval mode: {stderr}"
    );

    Ok(())
}

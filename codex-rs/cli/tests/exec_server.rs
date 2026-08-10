#[cfg(unix)]
use std::io::BufRead as _;
#[cfg(unix)]
use std::io::BufReader as StdBufReader;
#[cfg(unix)]
use std::io::Read as _;
#[cfg(unix)]
use std::io::Write as _;
#[cfg(unix)]
use std::net::TcpStream;
use std::path::Path;
use std::process::Stdio;
#[cfg(unix)]
use std::thread;
use std::time::Duration;
#[cfg(unix)]
use std::time::Instant;

use anyhow::Context;
use anyhow::Result;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;
use tempfile::TempDir;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use wiremock::MockServer;

fn codex_command(codex_home: &Path) -> Result<assert_cmd::Command> {
    let mut cmd = assert_cmd::Command::new(codex_utils_cargo_bin::cargo_bin("whisply")?);
    cmd.env("CODEX_HOME", codex_home)
        .env("WHISPLY_HOME", codex_home);
    Ok(cmd)
}

#[test]
fn strict_config_rejects_unknown_config_fields_for_exec_server() -> Result<()> {
    let codex_home = TempDir::new()?;
    std::fs::write(
        codex_home.path().join("config.toml"),
        r#"
foo = "bar"
"#,
    )?;

    let mut cmd = codex_command(codex_home.path())?;
    cmd.args([
        "exec-server",
        "--strict-config",
        "--listen",
        "ws://127.0.0.1:0",
    ])
    .assert()
    .failure()
    .stderr(contains("unknown configuration field"));

    Ok(())
}

#[test]
fn local_exec_server_ignores_invalid_config_without_strict_config() -> Result<()> {
    let codex_home = TempDir::new()?;
    std::fs::write(codex_home.path().join("config.toml"), "not valid toml = [")?;

    let mut cmd = codex_command(codex_home.path())?;
    cmd.args(["exec-server", "--listen", "stdio"])
        .assert()
        .success()
        .stderr(contains("not valid toml").not());

    Ok(())
}

/// The standalone exec-server accepts an explicit per-connection concurrency limit.
#[test]
fn local_exec_server_accepts_concurrent_requests_flag() -> Result<()> {
    let codex_home = TempDir::new()?;
    let mut cmd = codex_command(codex_home.path())?;
    cmd.args([
        "exec-server",
        "--listen",
        "stdio",
        "--concurrent-requests",
        "2",
    ])
    .assert()
    .success();

    Ok(())
}

#[test]
fn local_exec_server_allows_disabled_parent_lifetime_environment_variable() -> Result<()> {
    let codex_home = TempDir::new()?;
    let mut cmd = codex_command(codex_home.path())?;
    cmd.env(
        codex_exec_server::CODEX_EXEC_SERVER_EXIT_ON_STDIN_CLOSE_ENV_VAR,
        "false",
    )
    .args(["exec-server", "--listen", "stdio"])
    .assert()
    .success();

    Ok(())
}

#[tokio::test]
async fn local_exec_server_does_not_export_telemetry_on_stdio_disconnect() -> Result<()> {
    let collector = MockServer::start().await;
    let codex_home = TempDir::new()?;
    let cwd = url::Url::from_directory_path(std::env::current_dir()?)
        .map_err(|()| anyhow::anyhow!("could not convert cwd to file URL"))?;
    #[cfg(windows)]
    let argv = vec!["ping.exe", "-n", "61", "127.0.0.1"];
    #[cfg(not(windows))]
    let argv = vec!["/bin/sleep", "60"];
    let codex_bin = codex_utils_cargo_bin::cargo_bin("whisply")?;
    let codex_home = codex_home.path().to_path_buf();
    let collector_uri = collector.uri();
    let subprocess = async move {
        let mut command = tokio::process::Command::new(codex_bin);
        command
            .env("CODEX_HOME", &codex_home)
            .env("WHISPLY_HOME", &codex_home)
            .env("HTTP_PROXY", &collector_uri)
            .env("http_proxy", &collector_uri)
            .env("HTTPS_PROXY", &collector_uri)
            .env("https_proxy", &collector_uri)
            .env("ALL_PROXY", &collector_uri)
            .env("all_proxy", &collector_uri)
            .env_remove("NO_PROXY")
            .env_remove("no_proxy")
            .args(["exec-server", "--listen", "stdio"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn()?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("exec-server stdin was not piped"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("exec-server stdout was not piped"))?;
        let mut stdout = BufReader::new(stdout);
        send_json_line(
            &mut stdin,
            &serde_json::json!({
                "id": 1,
                "method": "initialize",
                "params": {"clientName": "managed-no-telemetry-test", "resumeSessionId": null}
            }),
        )
        .await?;
        wait_for_response(&mut stdout, /*expected_id*/ 1).await?;
        send_json_line(
            &mut stdin,
            &serde_json::json!({"method": "initialized", "params": {}}),
        )
        .await?;
        send_json_line(
            &mut stdin,
            &serde_json::json!({
                "id": 2,
                "method": "process/start",
                "params": {
                    "processId": "local-no-telemetry-process",
                    "argv": argv,
                    "cwd": cwd,
                    "env": {},
                    "tty": false,
                    "pipeStdin": false,
                    "arg0": null
                }
            }),
        )
        .await?;
        wait_for_response(&mut stdout, /*expected_id*/ 2).await?;
        drop(stdin);
        let mut remaining_stdout = String::new();
        stdout.read_to_string(&mut remaining_stdout).await?;
        let status = child.wait().await?;
        anyhow::ensure!(
            status.success(),
            "exec-server exited with {status}; remaining stdout: {remaining_stdout}"
        );
        Ok::<(), anyhow::Error>(())
    };
    tokio::time::timeout(Duration::from_secs(30), subprocess)
        .await
        .map_err(|_| anyhow::anyhow!("exec-server subprocess timed out"))??;

    assert!(
        collector
            .received_requests()
            .await
            .unwrap_or_default()
            .is_empty(),
        "local exec-server must not export telemetry through inherited proxy settings"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn local_exec_server_exits_successfully_on_sigterm() -> Result<()> {
    let codex_home = TempDir::new()?;
    let mut child = std::process::Command::new(codex_utils_cargo_bin::cargo_bin("whisply")?)
        .env("CODEX_HOME", codex_home.path())
        .env("WHISPLY_HOME", codex_home.path())
        .args(["exec-server", "--listen", "ws://127.0.0.1:0"])
        .stdout(Stdio::piped())
        .spawn()?;
    let mut listen_url = String::new();
    StdBufReader::new(child.stdout.take().expect("child stdout")).read_line(&mut listen_url)?;
    assert!(listen_url.starts_with("ws://127.0.0.1:"), "{listen_url}");

    let listen_addr = listen_url
        .trim()
        .strip_prefix("ws://")
        .expect("listen URL should use ws://")
        .parse()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut ready = false;
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        if let Ok(mut stream) =
            TcpStream::connect_timeout(&listen_addr, remaining.min(Duration::from_millis(100)))
        {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
            let request =
                format!("GET /readyz HTTP/1.1\r\nHost: {listen_addr}\r\nConnection: close\r\n\r\n");
            let mut response = String::new();
            if stream.write_all(request.as_bytes()).is_ok()
                && stream.read_to_string(&mut response).is_ok()
                && response.starts_with("HTTP/1.1 200")
            {
                ready = true;
                break;
            }
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(ready, "exec-server did not become ready at {listen_url}");

    // SAFETY: `child.id()` is the live process spawned above.
    let result = unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
    assert_eq!(result, 0);
    let status = child.wait()?;
    assert!(status.success(), "{status}");
    Ok(())
}

async fn send_json_line(
    stdin: &mut (impl tokio::io::AsyncWrite + Unpin),
    message: &serde_json::Value,
) -> Result<()> {
    let mut encoded = serde_json::to_vec(message)?;
    encoded.push(b'\n');
    stdin.write_all(&encoded).await?;
    stdin.flush().await?;
    Ok(())
}

async fn wait_for_response(
    stdout: &mut (impl tokio::io::AsyncBufRead + Unpin),
    expected_id: i64,
) -> Result<()> {
    loop {
        let mut line = String::new();
        if stdout.read_line(&mut line).await? == 0 {
            anyhow::bail!("exec-server stdout closed before response {expected_id}");
        }
        let message: serde_json::Value = serde_json::from_str(&line)?;
        if message["id"].as_i64() == Some(expected_id) {
            anyhow::ensure!(
                message.get("error").is_none(),
                "exec-server request {expected_id} failed: {message}"
            );
            return Ok(());
        }
    }
}

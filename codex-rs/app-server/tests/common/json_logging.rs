use std::path::Path;
use std::process::Command;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use serde_json::Value;
use serde_json::json;
use tokio::sync::Notify;

#[derive(Clone, Default)]
pub(crate) struct JsonLogCapture {
    lines: Arc<Mutex<Vec<String>>>,
    updated: Arc<Notify>,
}

impl JsonLogCapture {
    /// Redacts an MCP child stderr line before retaining it for assertions or
    /// forwarding it to the test harness. Child stderr is untrusted diagnostic
    /// input and can contain credentials or private request material.
    pub(crate) fn record_mcp_child_stderr(&self, line: &str) -> String {
        let redacted = redact_mcp_child_stderr(line);
        self.lines
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(redacted.clone());
        self.updated.notify_one();
        redacted
    }

    pub(crate) async fn wait_for_event(&self, event_name: &str) -> Result<Value> {
        let mut events = self.wait_for_events(event_name, /*count*/ 1).await?;
        Ok(events.remove(0))
    }

    pub(crate) async fn wait_for_events(
        &self,
        event_name: &str,
        count: usize,
    ) -> Result<Vec<Value>> {
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let updated = self.updated.notified();
                let events = self
                    .events()?
                    .into_iter()
                    .filter(|event| event["fields"]["event.name"].as_str() == Some(event_name))
                    .collect::<Vec<_>>();
                if events.len() >= count {
                    return Ok(events);
                }
                updated.await;
            }
        })
        .await;
        match result {
            Ok(result) => result,
            Err(_) => {
                let lines = self
                    .lines
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .join("\n");
                anyhow::bail!(
                    "timed out waiting for {count} JSON log event(s) named `{event_name}`; captured stderr:\n{lines}"
                )
            }
        }
    }

    pub(crate) fn events(&self) -> Result<Vec<Value>> {
        let lines = self
            .lines
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        json_log_events(lines.iter().map(String::as_str))
    }
}

const REDACTED_MCP_STDERR_VALUE: &str = "[redacted]";

fn redact_mcp_child_stderr(line: &str) -> String {
    codex_secrets::redact_mcp_stderr(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_child_stderr_redacts_structured_credentials_and_payloads() {
        let line = r#"{
            "level":"WARN",
            "fields":{
                "message":"MCP child exited unexpectedly",
                "headers":{"Authorization":"Bearer unit-test-bearer-token-0123456789","x-trace":"kept"},
                "callback_url":"whisply://auth-callback?code=unit-test-code&state=unit-test-state",
                "payload":{"email":"private@example.test"},
                "environment":{"WHISPLY_PROXY_TOKEN":"unit-test-proxy-token","RUST_LOG":"warn"}
            },
            "target":"mcp.child"
        }"#;

        let redacted = redact_mcp_child_stderr(line);
        for secret in [
            "unit-test-bearer-token-0123456789",
            "unit-test-code",
            "unit-test-state",
            "private@example.test",
            "unit-test-proxy-token",
        ] {
            assert!(!redacted.contains(secret), "leaked {secret}: {redacted}");
        }
        assert!(redacted.contains("MCP child exited unexpectedly"));
        assert!(redacted.contains("x-trace"));
        assert!(redacted.contains("RUST_LOG"));
        assert!(redacted.contains(REDACTED_MCP_STDERR_VALUE));
        assert!(serde_json::from_str::<Value>(&redacted).is_ok());
    }

    #[test]
    fn mcp_child_stderr_redacts_unstructured_headers_and_callback_parameters() {
        let line = "MCP child failed before callback at https://example.test/callback?code=unit-test-code&state=unit-test-state Bearer unit-test-bearer-token-0123456789";

        let redacted = redact_mcp_child_stderr(line);

        assert!(
            redacted
                .starts_with("MCP child failed before callback at https://example.test/callback?")
        );
        for secret in [
            "unit-test-code",
            "unit-test-state",
            "unit-test-bearer-token-0123456789",
        ] {
            assert!(!redacted.contains(secret), "leaked {secret}: {redacted}");
        }
        assert!(redacted.contains(REDACTED_MCP_STDERR_VALUE));
    }
}

pub fn app_server_json_shutdown_event(
    binary: &str,
    args: &[&str],
    codex_home: &Path,
) -> Result<Value> {
    std::fs::write(
        codex_home.join("config.toml"),
        "[features]\nplugins = false\n",
    )?;
    let output = Command::new(codex_utils_cargo_bin::cargo_bin(binary)?)
        .stdin(Stdio::null())
        .env("WHISPLY_HOME", codex_home)
        .env_remove("CODEX_HOME")
        // Do not inherit a debug-only test override that would replace the
        // temporary config written above.
        .env_remove("CODEX_APP_SERVER_TEST_USER_CONFIG_FILE")
        .env(
            "CODEX_APP_SERVER_MANAGED_CONFIG_PATH",
            codex_home.join("managed_config.toml"),
        )
        .env("LOG_FORMAT", "json")
        .env("RUST_LOG", "codex_app_server=info")
        .args(args)
        .output()?;

    let stderr = String::from_utf8(output.stderr)?;
    anyhow::ensure!(output.status.success(), "app-server failed: {stderr}");

    let events = json_log_events(stderr.lines())
        .with_context(|| format!("app-server stderr was not valid JSONL: {stderr}"))?;
    let event = events
        .iter()
        .find(|event| event["fields"]["message"] == "processor task exited")
        .context("missing INFO shutdown event in app-server JSON logs")?;
    Ok(json!({
        "level": event["level"],
        "fields": event["fields"],
        "target": event["target"],
    }))
}

fn json_log_events<'a>(lines: impl IntoIterator<Item = &'a str>) -> Result<Vec<Value>> {
    lines
        .into_iter()
        .filter(|line| !line.is_empty())
        .map(|line| {
            let event = serde_json::from_str::<Value>(line)
                .with_context(|| format!("log line was not JSON: {line}"))?;
            anyhow::ensure!(
                event["level"].is_string()
                    && event["fields"].is_object()
                    && event["target"].is_string(),
                "JSON log event did not include level, fields, and target: {line}"
            );
            let timestamp = event["timestamp"]
                .as_str()
                .with_context(|| format!("JSON log event did not include a timestamp: {line}"))?;
            chrono::DateTime::parse_from_rfc3339(timestamp).with_context(|| {
                format!("JSON log event timestamp was not RFC 3339: {timestamp}")
            })?;
            Ok(event)
        })
        .collect()
}

#[cfg(test)]
mod capture_tests {
    use super::*;

    #[test]
    fn mcp_child_stderr_is_redacted_before_capture() {
        let capture = JsonLogCapture::default();
        let raw = r#"{"level":"WARN","fields":{"message":"MCP child failed","authorization":"Bearer unit-test-bearer-token-0123456789","payload":{"email":"private@example.test"}},"target":"mcp.child"}"#;

        let emitted = capture.record_mcp_child_stderr(raw);
        let captured = capture
            .lines
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .join("\n");

        for secret in ["unit-test-bearer-token-0123456789", "private@example.test"] {
            assert!(!emitted.contains(secret), "emitted {secret}: {emitted}");
            assert!(!captured.contains(secret), "captured {secret}: {captured}");
        }
        assert_eq!(emitted, captured);
        assert!(captured.contains(REDACTED_MCP_STDERR_VALUE));
    }
}

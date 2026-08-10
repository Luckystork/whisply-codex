use regex::Regex;
use serde_json::Value;
use std::sync::LazyLock;

static OPENAI_KEY_REGEX: LazyLock<Regex> = LazyLock::new(|| compile_regex(r"sk-[A-Za-z0-9]{20,}"));
static AWS_ACCESS_KEY_ID_REGEX: LazyLock<Regex> =
    LazyLock::new(|| compile_regex(r"\bAKIA[0-9A-Z]{16}\b"));
static BEARER_TOKEN_REGEX: LazyLock<Regex> =
    LazyLock::new(|| compile_regex(r"(?i:\bBearer)[ \t]+[A-Za-z0-9._~+/-]{16,}=*"));
static SECRET_ASSIGNMENT_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    compile_regex(r#"(?i)\b(api[_-]?key|token|secret|password)\b(\s*[:=]\s*)(["']?)[^\s"']{8,}"#)
});
static MCP_AUTHORIZATION_HEADER_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    compile_regex(r"(?im)(\b(?:proxy-)?authorization\s*[:=]\s*)[^\r\n]*")
});

/// Remove secret and keys from a String. This is done on best effort basis following some
/// well-known REGEX.
pub fn redact_secrets(input: String) -> String {
    let redacted = BEARER_TOKEN_REGEX.replace_all(&input, "Bearer [REDACTED_SECRET]");
    let redacted = OPENAI_KEY_REGEX.replace_all(&redacted, "[REDACTED_SECRET]");
    let redacted = AWS_ACCESS_KEY_ID_REGEX.replace_all(&redacted, "[REDACTED_SECRET]");
    let redacted = SECRET_ASSIGNMENT_REGEX.replace_all(&redacted, "$1$2$3[REDACTED_SECRET]");

    redacted.to_string()
}

const REDACTED_MCP_STDERR_VALUE: &str = "[redacted]";

/// Redacts untrusted MCP child stderr before it is logged or retained.
///
/// MCP stderr is not protocol data. It can contain HTTP diagnostics, copied
/// environment values, callback URLs, and request payloads. Keep the useful
/// surrounding diagnostic context while replacing credential and private
/// material with a fixed marker.
pub fn redact_mcp_stderr(input: &str) -> String {
    match serde_json::from_str::<Value>(input) {
        Ok(mut event) => {
            redact_mcp_json_value(&mut event);
            serde_json::to_string(&event).unwrap_or_else(|_| redact_unstructured_mcp_stderr(input))
        }
        Err(_) => redact_unstructured_mcp_stderr(input),
    }
}

fn redact_mcp_json_value(value: &mut Value) {
    match value {
        Value::Array(values) => {
            for value in values {
                redact_mcp_json_value(value);
            }
        }
        Value::Object(values) => {
            for (key, value) in values {
                if is_sensitive_mcp_stderr_key(key) || is_private_payload_key(key) {
                    *value = Value::String(REDACTED_MCP_STDERR_VALUE.to_string());
                } else if is_environment_key(key) {
                    redact_environment_values(value);
                } else {
                    redact_mcp_json_value(value);
                }
            }
        }
        Value::String(text) => {
            *text = redact_unstructured_mcp_stderr(text);
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn is_sensitive_mcp_stderr_key(key: &str) -> bool {
    let normalized = key
        .bytes()
        .filter(u8::is_ascii_alphanumeric)
        .map(char::from)
        .collect::<String>()
        .to_ascii_lowercase();

    normalized.contains("authorization")
        || normalized.contains("cookie")
        || normalized.contains("secret")
        || normalized.contains("password")
        || normalized.contains("credential")
        || normalized.contains("apikey")
        || normalized.contains("token")
        || normalized.contains("callback")
}

fn is_private_payload_key(key: &str) -> bool {
    let normalized = key
        .bytes()
        .filter(u8::is_ascii_alphanumeric)
        .map(char::from)
        .collect::<String>()
        .to_ascii_lowercase();

    matches!(
        normalized.as_str(),
        "payload"
            | "body"
            | "request"
            | "response"
            | "content"
            | "input"
            | "output"
            | "data"
            | "args"
            | "arguments"
            | "params"
    ) || normalized.contains("privatepayload")
}

fn is_environment_key(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().replace(['_', '-'], "").as_str(),
        "env" | "environment" | "environmentvariables"
    )
}

fn redact_environment_values(value: &mut Value) {
    match value {
        Value::Object(values) => {
            for value in values.values_mut() {
                *value = Value::String(REDACTED_MCP_STDERR_VALUE.to_string());
            }
        }
        _ => *value = Value::String(REDACTED_MCP_STDERR_VALUE.to_string()),
    }
}

fn redact_unstructured_mcp_stderr(input: &str) -> String {
    let mut redacted = redact_secrets(input.to_string());
    redacted = MCP_AUTHORIZATION_HEADER_REGEX
        .replace_all(&redacted, "${1}[redacted]")
        .into_owned();

    for marker in [
        "cookie:",
        "set-cookie:",
        "x-api-key:",
        "api-key:",
        "payload:",
        "payload=",
        "body:",
        "body=",
        "request:",
        "request=",
        "response:",
        "response=",
        "env:",
        "environment:",
        "callback?",
    ] {
        redacted = redact_to_line_end_after_marker(&redacted, marker);
    }

    for parameter in [
        "access_token=",
        "id_token=",
        "refresh_token=",
        "token=",
        "secret=",
        "password=",
        "cookie=",
        "api_key=",
        "api-key=",
        "access_key=",
        "access_key_id=",
        "private_key=",
        "client_secret=",
        "connection_string=",
        "database_url=",
        "service_role_key=",
        "code=",
        "state=",
        "callback=",
        "callback_url=",
        "redirect_uri=",
        "redirect_url=",
    ] {
        redacted = redact_query_value_after_marker(&redacted, parameter);
    }

    redacted
}

fn redact_to_line_end_after_marker(input: &str, marker: &str) -> String {
    redact_after_marker(input, marker, |_| false)
}

fn redact_query_value_after_marker(input: &str, marker: &str) -> String {
    redact_after_marker(input, marker, |byte| {
        matches!(
            byte,
            b'&' | b'#' | b'\'' | b'\"' | b')' | b']' | b'}' | b',' | b' ' | b'\t'
        )
    })
}

fn redact_after_marker(input: &str, marker: &str, terminates_value: impl Fn(u8) -> bool) -> String {
    let mut redacted = String::with_capacity(input.len());
    let mut copied_until = 0;

    while let Some(marker_start) = find_ascii_case_insensitive(input, marker, copied_until) {
        let value_start = marker_start + marker.len();
        let value_end = input.as_bytes()[value_start..]
            .iter()
            .position(|byte| terminates_value(*byte))
            .map_or(input.len(), |offset| value_start + offset);
        redacted.push_str(&input[copied_until..value_start]);
        redacted.push_str(REDACTED_MCP_STDERR_VALUE);
        copied_until = value_end;
    }

    redacted.push_str(&input[copied_until..]);
    redacted
}

fn find_ascii_case_insensitive(input: &str, marker: &str, start: usize) -> Option<usize> {
    let input = input.as_bytes();
    let marker = marker.as_bytes();
    input
        .get(start..)?
        .windows(marker.len())
        .position(|candidate| candidate.eq_ignore_ascii_case(marker))
        .map(|offset| start + offset)
}

fn compile_regex(pattern: &str) -> Regex {
    match Regex::new(pattern) {
        Ok(regex) => regex,
        Err(err) => panic!("invalid regex pattern `{pattern}`: {err}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn redacts_supported_bearer_tokens() {
        let cases = [
            (
                "Bearer abcde+fghijklmnopqrstuvwxyz012345",
                "Bearer [REDACTED_SECRET]",
            ),
            (
                "Bearer abcdefghijklmnop+secret_suffix",
                "Bearer [REDACTED_SECRET]",
            ),
            (
                "Bearer sk-abcdefghijklmnopqrst+secret_suffix",
                "Bearer [REDACTED_SECRET]",
            ),
            (
                "Bearer AKIAABCDEFGHIJKLMNOP/~secret_suffix",
                "Bearer [REDACTED_SECRET]",
            ),
            (
                "Bearer AbcdefghijklMN09._~+/-==; echo done",
                "Bearer [REDACTED_SECRET]; echo done",
            ),
            (
                "authorization: bEaReR\tabcdefghijklmnop",
                "authorization: Bearer [REDACTED_SECRET]",
            ),
            ("Bearer   abcdefghijklmnop", "Bearer [REDACTED_SECRET]"),
        ];

        for (input, expected) in cases {
            assert_eq!(redact_secrets(input.to_string()), expected);
        }
    }

    #[test]
    fn avoids_bearer_false_positives() {
        let cases = [
            "Bearer of good news",
            "Bearer abcdefghijklmno",
            "NotABearer abcdefghijklmnop",
            "Bearerabcdefghijklmnop",
            "Bearer\nabcdefghijklmnop",
            "Bearer\u{a0}abcdefghijklmnop",
            "Bearer abcdefghijklmno\u{212a}",
        ];

        for input in cases {
            assert_eq!(redact_secrets(input.to_string()), input);
        }
    }

    #[test]
    fn redact_mcp_stderr_preserves_context_without_credentials_or_payloads() {
        let input = r#"{
            "level":"WARN",
            "fields":{
                "message":"MCP child exited unexpectedly",
                "headers":{"Authorization":"Bearer unit-test-bearer-token-0123456789","x-trace":"kept"},
                "callback_url":"whisply://auth-callback?code=unit-test-code&state=unit-test-state",
                "payload":{"email":"private@example.test"},
                "environment":{"WHISPLY_PROXY_TOKEN":"unit-test-proxy-token","DATABASE_URL":"postgres://private@example.test/db","RUST_LOG":"warn"}
            },
            "target":"mcp.child"
        }"#;

        let redacted = redact_mcp_stderr(input);
        for secret in [
            "unit-test-bearer-token-0123456789",
            "unit-test-code",
            "unit-test-state",
            "private@example.test",
            "unit-test-proxy-token",
            "postgres://private@example.test/db",
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
    fn redact_mcp_stderr_redacts_unstructured_callback_parameters_and_bearers() {
        let input = "MCP child failed before callback at https://example.test/callback?code=unit-test-code&state=unit-test-state Bearer unit-test-bearer-token-0123456789";

        let redacted = redact_mcp_stderr(input);

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

    #[test]
    fn redact_mcp_stderr_redacts_complete_basic_authorization_header_values() {
        let authorization = "dXNlcm5hbWU6cGFzc3dvcmQ=";
        let proxy_authorization = "cHJveHk6cGFzc3dvcmQ=";
        let input = format!(
            "MCP child rejected Authorization = Basic {authorization}\nMCP proxy rejected Proxy-Authorization=Basic {proxy_authorization}"
        );

        let redacted = redact_mcp_stderr(&input);

        assert!(!redacted.contains(authorization), "leaked {redacted}");
        assert!(
            !redacted.contains(proxy_authorization),
            "leaked {redacted}"
        );
        assert_eq!(
            redacted,
            "MCP child rejected Authorization = [redacted]\nMCP proxy rejected Proxy-Authorization=[redacted]"
        );
    }
}

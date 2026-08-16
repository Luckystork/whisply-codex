use anyhow::Result;
use anyhow::anyhow;
use http::HeaderMap;
use http::HeaderName;
use http::HeaderValue;
use http::header::USER_AGENT;
use std::collections::HashMap;
use std::env;
use std::ffi::OsString;
use whisply_config::host_env_var_is_reserved;
use whisply_config::types::McpServerEnvVar;

const MCP_USER_AGENT: &str = concat!("codex-mcp-client/", env!("CARGO_PKG_VERSION"));

pub(crate) fn create_env_for_mcp_server(
    extra_env: Option<HashMap<OsString, OsString>>,
    env_vars: &[McpServerEnvVar],
) -> Result<HashMap<OsString, OsString>> {
    let additional_env_vars = local_stdio_env_var_names(env_vars)?;
    let env = DEFAULT_ENV_VARS
        .iter()
        .copied()
        .chain(additional_env_vars)
        .filter(|var| !refuse_reserved_host_env(var))
        .filter_map(|var| env::var_os(var).map(|value| (OsString::from(var), value)))
        .chain(extra_env.unwrap_or_default())
        .collect();
    Ok(env)
}

/// The last point before a value leaves the host process for a server.
///
/// Enforced here rather than only where servers are registered because this is
/// the single place every transport and every declaration source passes
/// through. A registration path added later inherits the rule instead of
/// having to remember it.
fn refuse_reserved_host_env(name: &str) -> bool {
    if !host_env_var_is_reserved(name) {
        return false;
    }
    tracing::warn!(
        env_var = name,
        "refused to pass a Whisply-reserved environment variable to an MCP server"
    );
    true
}

pub(crate) fn create_env_overlay_for_remote_mcp_server(
    extra_env: Option<HashMap<OsString, OsString>>,
    env_vars: &[McpServerEnvVar],
) -> HashMap<OsString, OsString> {
    // Remote stdio should inherit PATH/HOME/etc. from the executor side, not
    // from the orchestrator process. Only forward variables explicitly named
    // by the MCP config plus literal env overrides from that config.
    env_vars
        .iter()
        .filter(|var| !var.is_remote_source())
        .filter(|var| !refuse_reserved_host_env(var.name()))
        .filter_map(|var| env::var_os(var.name()).map(|value| (OsString::from(var.name()), value)))
        .chain(extra_env.unwrap_or_default())
        .collect()
}

pub(crate) fn remote_mcp_env_var_names(env_vars: &[McpServerEnvVar]) -> Vec<String> {
    env_vars
        .iter()
        .filter(|var| var.is_remote_source())
        // The executor resolves these on its own side, so the name alone is
        // what would let a reserved value through.
        .filter(|var| !refuse_reserved_host_env(var.name()))
        .map(|var| var.name().to_string())
        .collect()
}

fn local_stdio_env_var_names(env_vars: &[McpServerEnvVar]) -> Result<impl Iterator<Item = &str>> {
    if let Some(remote_var) = env_vars.iter().find(|var| var.is_remote_source()) {
        return Err(anyhow!(
            "env_vars entry `{}` uses source `remote`, which requires remote MCP stdio",
            remote_var.name()
        ));
    }
    Ok(env_vars.iter().map(McpServerEnvVar::name))
}

pub(crate) fn build_default_headers(
    http_headers: Option<HashMap<String, String>>,
    env_http_headers: Option<HashMap<String, String>>,
) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_static(MCP_USER_AGENT));

    if let Some(static_headers) = http_headers {
        for (name, value) in static_headers {
            let header_name = match HeaderName::from_bytes(name.as_bytes()) {
                Ok(name) => name,
                Err(err) => {
                    tracing::warn!("invalid HTTP header name `{name}`: {err}");
                    continue;
                }
            };
            let header_value = match HeaderValue::from_str(value.as_str()) {
                Ok(value) => value,
                Err(err) => {
                    tracing::warn!("invalid HTTP header value for `{name}`: {err}");
                    continue;
                }
            };
            headers.insert(header_name, header_value);
        }
    }

    if let Some(env_headers) = env_http_headers {
        for (name, env_var) in env_headers {
            // The strongest form of this leak: unlike stdio, which hands a
            // value to a local subprocess, a header is sent over the network to
            // whatever URL the server declared.
            if refuse_reserved_host_env(&env_var) {
                continue;
            }
            if let Ok(value) = env::var(&env_var) {
                if value.trim().is_empty() {
                    continue;
                }

                let header_name = match HeaderName::from_bytes(name.as_bytes()) {
                    Ok(name) => name,
                    Err(err) => {
                        tracing::warn!("invalid HTTP header name `{name}`: {err}");
                        continue;
                    }
                };

                let header_value = match HeaderValue::from_str(value.as_str()) {
                    Ok(value) => value,
                    Err(err) => {
                        tracing::warn!(
                            "invalid HTTP header value read from {env_var} for `{name}`: {err}"
                        );
                        continue;
                    }
                };
                headers.insert(header_name, header_value);
            }
        }
    }

    Ok(headers)
}

#[cfg(unix)]
pub(crate) const DEFAULT_ENV_VARS: &[&str] = &[
    "HOME",
    "LOGNAME",
    "PATH",
    "SHELL",
    "USER",
    "__CF_USER_TEXT_ENCODING",
    "LANG",
    "LC_ALL",
    "TERM",
    "TMPDIR",
    "TZ",
];

#[cfg(windows)]
pub(crate) const DEFAULT_ENV_VARS: &[&str] =
    whisply_protocol::shell_environment::WINDOWS_CORE_ENV_VARS;

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    use serial_test::serial;
    use std::ffi::OsStr;

    struct EnvVarGuard {
        key: String,
        original: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &str, value: impl AsRef<OsStr>) -> Self {
            let original = std::env::var_os(key);
            unsafe {
                std::env::set_var(key, value.as_ref());
            }
            Self {
                key: key.to_string(),
                original,
            }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(value) = &self.original {
                unsafe {
                    std::env::set_var(&self.key, value);
                }
            } else {
                unsafe {
                    std::env::remove_var(&self.key);
                }
            }
        }
    }

    #[tokio::test]
    async fn create_env_honors_overrides() {
        let value = "custom".to_string();
        let expected = OsString::from(&value);
        let env = create_env_for_mcp_server(
            Some(HashMap::from([(OsString::from("TZ"), expected.clone())])),
            &[],
        )
        .expect("local MCP env should build");
        assert_eq!(env.get(OsStr::new("TZ")), Some(&expected));
    }

    #[test]
    #[serial(extra_rmcp_env)]
    fn create_env_includes_additional_whitelisted_variables() {
        let custom_var = "EXTRA_RMCP_ENV";
        let value = "from-env";
        let expected = OsString::from(value);
        let _guard = EnvVarGuard::set(custom_var, value);
        let env = create_env_for_mcp_server(/*extra_env*/ None, &[custom_var.into()])
            .expect("local MCP env should build");
        assert_eq!(env.get(OsStr::new(custom_var)), Some(&expected));
    }

    /// Naming a reserved variable in `env_vars` must not be a way to obtain
    /// it. This one is the pointer to the brokered gateway token; the token is
    /// never in the environment, but the handle should not be either.
    #[test]
    #[serial(extra_rmcp_env)]
    fn a_server_cannot_read_a_whisply_authority_handle() {
        let reserved = "WHISPLY_GATEWAY_ENDPOINT_FD";
        let _guard = EnvVarGuard::set(reserved, "41");

        let env = create_env_for_mcp_server(/*extra_env*/ None, &[reserved.into()])
            .expect("local MCP env should build");

        assert_eq!(env.get(OsStr::new(reserved)), None);
    }

    #[test]
    #[serial(extra_rmcp_env)]
    fn refusing_a_reserved_variable_does_not_withhold_the_rest() {
        let reserved = "WHISPLY_NATIVE_BROKER_SOCKET";
        let allowed = "EXTRA_RMCP_ENV";
        let allowed_value = OsString::from("from-env");
        let _reserved_guard = EnvVarGuard::set(reserved, "/tmp/broker.sock");
        let _allowed_guard = EnvVarGuard::set(allowed, "from-env");

        let env =
            create_env_for_mcp_server(/*extra_env*/ None, &[reserved.into(), allowed.into()])
                .expect("local MCP env should build");

        // Refusal is per variable. Failing the whole launch would turn a
        // containment rule into a way to break unrelated servers.
        assert_eq!(env.get(OsStr::new(reserved)), None);
        assert_eq!(env.get(OsStr::new(allowed)), Some(&allowed_value));
    }

    #[test]
    #[serial(extra_rmcp_env)]
    fn the_whole_reserved_namespace_is_refused_not_a_list_of_known_names() {
        // The point of a namespace rule is that a variable introduced later is
        // covered without anyone remembering to add it.
        let unheard_of = "WHISPLY_SOMETHING_ADDED_LATER";
        let _guard = EnvVarGuard::set(unheard_of, "secret");

        let env = create_env_for_mcp_server(/*extra_env*/ None, &[unheard_of.into()])
            .expect("local MCP env should build");

        assert_eq!(env.get(OsStr::new(unheard_of)), None);
    }

    #[test]
    #[serial(extra_rmcp_env)]
    fn the_remote_overlay_refuses_reserved_variables_too() {
        let reserved = "WHISPLY_GATEWAY_AUTH_FD";
        let _guard = EnvVarGuard::set(reserved, "7");

        let overlay =
            create_env_overlay_for_remote_mcp_server(/*extra_env*/ None, &[reserved.into()]);

        assert_eq!(overlay.get(OsStr::new(reserved)), None);
    }

    #[test]
    fn a_reserved_name_is_not_handed_to_the_executor_to_resolve() {
        // The remote path forwards names for the executor to resolve on its
        // side, so the name alone is what would leak the value there.
        let names = remote_mcp_env_var_names(&[
            McpServerEnvVar::Config {
                name: "WHISPLY_PROXY_TOKEN".to_string(),
                source: Some("remote".to_string()),
            },
            McpServerEnvVar::Config {
                name: "PLUGIN_API_KEY".to_string(),
                source: Some("remote".to_string()),
            },
        ]);

        assert_eq!(names, vec!["PLUGIN_API_KEY".to_string()]);
    }

    /// The strongest form of the leak: a header goes over the network to
    /// whatever URL the server declared, so this is exfiltration rather than
    /// merely over-sharing with a local subprocess.
    #[test]
    #[serial(extra_rmcp_env)]
    fn a_reserved_variable_is_never_sent_as_an_http_header() {
        let reserved = "WHISPLY_NATIVE_BROKER_SOCKET";
        let _guard = EnvVarGuard::set(reserved, "/tmp/broker.sock");

        let headers = build_default_headers(
            /*http_headers*/ None,
            Some(HashMap::from([(
                "x-exfiltrated".to_string(),
                reserved.to_string(),
            )])),
        )
        .expect("headers should build");

        assert_eq!(headers.get("x-exfiltrated"), None);
    }

    #[test]
    #[serial(extra_rmcp_env)]
    fn refusing_one_header_still_sends_the_legitimate_ones() {
        let reserved = "WHISPLY_HOME";
        let allowed = "EXTRA_RMCP_ENV";
        let _allowed_guard = EnvVarGuard::set(allowed, "legitimate-value");

        // `build_default_headers` must reject reserved names before reading
        // their values. Do not mutate WHISPLY_HOME just to prove that: it is
        // process-wide storage authority, and parallel OAuth tests correctly
        // exercise it against a real temporary home.

        let headers = build_default_headers(
            /*http_headers*/ None,
            Some(HashMap::from([
                ("x-exfiltrated".to_string(), reserved.to_string()),
                ("x-api-key".to_string(), allowed.to_string()),
            ])),
        )
        .expect("headers should build");

        assert_eq!(headers.get("x-exfiltrated"), None);
        assert_eq!(
            headers.get("x-api-key").map(|value| value.to_str().ok()),
            Some(Some("legitimate-value"))
        );
    }

    #[test]
    #[serial(extra_rmcp_env)]
    fn create_remote_env_overlay_only_forwards_explicit_variables() {
        let default_var = DEFAULT_ENV_VARS[0];
        let custom_var = "EXTRA_REMOTE_RMCP_ENV";
        let custom_value = OsString::from("from-env");
        let _default_guard = EnvVarGuard::set(default_var, "from-default");
        let _custom_guard = EnvVarGuard::set(custom_var, &custom_value);

        let env =
            create_env_overlay_for_remote_mcp_server(/*extra_env*/ None, &[custom_var.into()]);

        assert_eq!(
            env,
            HashMap::from([(OsString::from(custom_var), custom_value)])
        );
    }

    #[test]
    #[serial(extra_rmcp_env)]
    fn create_remote_env_overlay_does_not_copy_remote_source_variables() {
        let remote_var = "REMOTE_ONLY_RMCP_ENV";
        let local_var = "LOCAL_RMCP_ENV";
        let local_value = OsString::from("from-local-env");
        let _remote_guard = EnvVarGuard::set(remote_var, "should-not-be-copied");
        let _local_guard = EnvVarGuard::set(local_var, &local_value);

        let env = create_env_overlay_for_remote_mcp_server(
            /*extra_env*/ None,
            &[
                McpServerEnvVar::Config {
                    name: remote_var.to_string(),
                    source: Some("remote".to_string()),
                },
                McpServerEnvVar::Config {
                    name: local_var.to_string(),
                    source: Some("local".to_string()),
                },
            ],
        );

        assert_eq!(
            env,
            HashMap::from([(OsString::from(local_var), local_value)])
        );
    }

    #[test]
    fn remote_mcp_env_var_names_returns_remote_source_names() {
        let names = remote_mcp_env_var_names(&[
            "LEGACY".into(),
            McpServerEnvVar::Config {
                name: "LOCAL".to_string(),
                source: Some("local".to_string()),
            },
            McpServerEnvVar::Config {
                name: "REMOTE".to_string(),
                source: Some("remote".to_string()),
            },
        ]);

        assert_eq!(names, vec!["REMOTE".to_string()]);
    }

    #[test]
    fn create_local_env_rejects_remote_source_variables() {
        let err = create_env_for_mcp_server(
            /*extra_env*/ None,
            &[McpServerEnvVar::Config {
                name: "REMOTE".to_string(),
                source: Some("remote".to_string()),
            }],
        )
        .expect_err("remote source should require remote stdio");

        assert!(
            err.to_string().contains("requires remote MCP stdio"),
            "unexpected error: {err}"
        );
    }

    #[cfg(unix)]
    #[test]
    #[serial(extra_rmcp_env)]
    fn create_env_preserves_path_when_it_is_not_utf8() {
        use std::os::unix::ffi::OsStrExt;

        let raw_path = std::ffi::OsStr::from_bytes(b"/tmp/codex-\xFF/bin");
        let expected = raw_path.to_os_string();
        let _guard = EnvVarGuard::set("PATH", raw_path);

        let env =
            create_env_for_mcp_server(/*extra_env*/ None, &[]).expect("local MCP env should build");

        assert_eq!(env.get(OsStr::new("PATH")), Some(&expected));
    }
}

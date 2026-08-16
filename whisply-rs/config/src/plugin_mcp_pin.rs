//! Whether a plugin's MCP server still launches what the person approved.
//!
//! Installing a plugin can bring MCP servers with it, and those servers start
//! processes outside the sandbox with the plugin's own command line and
//! environment. The install flow shows that command line before the person
//! agrees to it. Nothing afterwards checked that the definition stayed the
//! same, and for a local plugin source the manifest is re-read on every load —
//! so the reviewed command could be replaced between one session and the next
//! with no second question asked.
//!
//! A launch pin is the hash of what actually runs. Recording it at install and
//! comparing it at load turns a silent substitution into a refusal.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::fingerprint::version_for_toml;
use crate::mcp_types::McpServerConfig;
use crate::mcp_types::McpServerTransportConfig;

/// The parts of a definition that decide what runs and what it can reach: the
/// command line, the environment handed to it, the working directory, and for
/// HTTP the endpoint and its headers.
///
/// Enablement, timeouts, approval modes, and tool allow-lists are the person's
/// own settings and are deliberately excluded. Changing an approval mode is not
/// the plugin swapping the binary underneath it, and a pin that broke on the
/// user's own edits would be re-approved reflexively until it meant nothing.
#[derive(Serialize)]
#[serde(untagged)]
enum LaunchIdentity {
    Stdio {
        transport: &'static str,
        command: String,
        args: Vec<String>,
        env: BTreeMap<String, String>,
        env_vars: Vec<String>,
        cwd: String,
    },
    StreamableHttp {
        transport: &'static str,
        url: String,
        bearer_token_env_var: String,
        http_headers: BTreeMap<String, String>,
        env_http_headers: BTreeMap<String, String>,
    },
}

/// A stable identity for what this server will start. The value is a hash, so
/// recording it never copies a manifest's literal environment values into the
/// person's own config.
pub fn plugin_mcp_launch_hash(config: &McpServerConfig) -> String {
    let identity = match &config.transport {
        McpServerTransportConfig::Stdio {
            command,
            args,
            env,
            env_vars,
            cwd,
        } => LaunchIdentity::Stdio {
            transport: "stdio",
            command: command.clone(),
            args: args.clone(),
            env: env
                .as_ref()
                .map(|env| {
                    env.iter()
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect()
                })
                .unwrap_or_default(),
            // Both the name and where the value is read from change what the
            // process receives, so both are part of the identity.
            env_vars: {
                let mut declared = env_vars
                    .iter()
                    .map(|env_var| match env_var.source() {
                        Some(source) => format!("{}:{source}", env_var.name()),
                        None => env_var.name().to_string(),
                    })
                    .collect::<Vec<_>>();
                declared.sort();
                declared
            },
            cwd: cwd.as_ref().map(ToString::to_string).unwrap_or_default(),
        },
        McpServerTransportConfig::StreamableHttp {
            url,
            bearer_token_env_var,
            http_headers,
            env_http_headers,
        } => LaunchIdentity::StreamableHttp {
            transport: "streamable_http",
            url: url.clone(),
            bearer_token_env_var: bearer_token_env_var.clone().unwrap_or_default(),
            http_headers: http_headers
                .as_ref()
                .map(|headers| {
                    headers
                        .iter()
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect()
                })
                .unwrap_or_default(),
            env_http_headers: env_http_headers
                .as_ref()
                .map(|headers| {
                    headers
                        .iter()
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect()
                })
                .unwrap_or_default(),
        },
    };
    let Ok(value) = toml::Value::try_from(&identity) else {
        unreachable!("plugin MCP launch identity should serialize to TOML");
    };
    version_for_toml(&value)
}

/// What a recorded pin says about the definition in front of us now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginMcpLaunchTrust {
    /// No pin was recorded, which is every server installed before pins
    /// existed. The definition is launched, because refusing here would break
    /// working installations to protect against a change that has not happened.
    Unpinned,
    Trusted,
    /// The definition changed after it was approved. This is the case the pin
    /// exists for.
    Changed,
}

pub fn plugin_mcp_launch_trust(
    config: &McpServerConfig,
    trusted_launch_hash: Option<&str>,
) -> PluginMcpLaunchTrust {
    match trusted_launch_hash {
        None => PluginMcpLaunchTrust::Unpinned,
        Some(trusted) if trusted == plugin_mcp_launch_hash(config) => PluginMcpLaunchTrust::Trusted,
        Some(_) => PluginMcpLaunchTrust::Changed,
    }
}

#[cfg(test)]
#[path = "plugin_mcp_pin_tests.rs"]
mod tests;

use std::collections::HashMap;

use super::*;
use crate::mcp_types::DEFAULT_MCP_SERVER_ENVIRONMENT_ID;
use crate::mcp_types::McpServerEnvVar;

fn stdio(command: &str, args: &[&str]) -> McpServerConfig {
    McpServerConfig {
        transport: McpServerTransportConfig::Stdio {
            command: command.to_string(),
            args: args.iter().map(|arg| (*arg).to_string()).collect(),
            env: None,
            env_vars: Vec::new(),
            cwd: None,
        },
        auth: Default::default(),
        environment_id: DEFAULT_MCP_SERVER_ENVIRONMENT_ID.to_string(),
        enabled: true,
        required: false,
        supports_parallel_tool_calls: false,
        omit_tools_from: None,
        disabled_reason: None,
        startup_timeout_sec: None,
        tool_timeout_sec: None,
        default_tools_approval_mode: None,
        enabled_tools: None,
        disabled_tools: None,
        scopes: None,
        oauth: None,
        oauth_resource: None,
        tools: HashMap::new(),
    }
}

#[test]
fn the_same_definition_hashes_the_same_way() {
    assert_eq!(
        plugin_mcp_launch_hash(&stdio("node", &["server.js"])),
        plugin_mcp_launch_hash(&stdio("node", &["server.js"]))
    );
}

#[test]
fn a_different_command_line_is_a_different_launch() {
    let approved = plugin_mcp_launch_hash(&stdio("node", &["server.js"]));
    for replacement in [
        stdio("bash", &["server.js"]),
        stdio("node", &["other.js"]),
        stdio("node", &["server.js", "--allow-write"]),
        stdio("node", &[]),
    ] {
        assert_ne!(
            approved,
            plugin_mcp_launch_hash(&replacement),
            "a changed command line kept its approval"
        );
    }
}

#[test]
fn the_environment_and_working_directory_are_part_of_what_runs() {
    let approved = plugin_mcp_launch_hash(&stdio("node", &["server.js"]));

    let mut with_env = stdio("node", &["server.js"]);
    if let McpServerTransportConfig::Stdio { env, .. } = &mut with_env.transport {
        *env = Some(HashMap::from([("TOKEN".to_string(), "value".to_string())]));
    }
    assert_ne!(approved, plugin_mcp_launch_hash(&with_env));

    let mut with_other_value = stdio("node", &["server.js"]);
    if let McpServerTransportConfig::Stdio { env, .. } = &mut with_other_value.transport {
        *env = Some(HashMap::from([("TOKEN".to_string(), "other".to_string())]));
    }
    assert_ne!(
        plugin_mcp_launch_hash(&with_env),
        plugin_mcp_launch_hash(&with_other_value),
        "an environment value can redirect the process and must break the pin"
    );

    let mut with_declared = stdio("node", &["server.js"]);
    if let McpServerTransportConfig::Stdio { env_vars, .. } = &mut with_declared.transport {
        *env_vars = vec![McpServerEnvVar::Name("HOME".to_string())];
    }
    assert_ne!(approved, plugin_mcp_launch_hash(&with_declared));

    let mut with_remote_source = stdio("node", &["server.js"]);
    if let McpServerTransportConfig::Stdio { env_vars, .. } = &mut with_remote_source.transport {
        *env_vars = vec![McpServerEnvVar::Config {
            name: "HOME".to_string(),
            source: Some("remote".to_string()),
        }];
    }
    assert_ne!(
        plugin_mcp_launch_hash(&with_declared),
        plugin_mcp_launch_hash(&with_remote_source),
        "where a value is read from is part of what the process receives"
    );
}

#[test]
fn declared_environment_order_is_not_a_change() {
    let mut first = stdio("node", &["server.js"]);
    let mut second = stdio("node", &["server.js"]);
    let names = [
        McpServerEnvVar::Name("A".to_string()),
        McpServerEnvVar::Name("B".to_string()),
    ];
    if let McpServerTransportConfig::Stdio { env_vars, .. } = &mut first.transport {
        *env_vars = names.to_vec();
    }
    if let McpServerTransportConfig::Stdio { env_vars, .. } = &mut second.transport {
        *env_vars = names.iter().rev().cloned().collect();
    }
    assert_eq!(
        plugin_mcp_launch_hash(&first),
        plugin_mcp_launch_hash(&second)
    );
}

#[test]
fn argument_order_is_a_change() {
    assert_ne!(
        plugin_mcp_launch_hash(&stdio("node", &["--a", "--b"])),
        plugin_mcp_launch_hash(&stdio("node", &["--b", "--a"]))
    );
}

/// The person's own settings must not break their own pin, or re-approval
/// becomes reflexive and stops meaning anything.
#[test]
fn user_policy_is_not_part_of_the_launch_identity() {
    let approved = plugin_mcp_launch_hash(&stdio("node", &["server.js"]));

    let mut disabled = stdio("node", &["server.js"]);
    disabled.enabled = false;
    assert_eq!(approved, plugin_mcp_launch_hash(&disabled));

    let mut restricted = stdio("node", &["server.js"]);
    restricted.enabled_tools = Some(vec!["read".to_string()]);
    restricted.disabled_tools = Some(vec!["write".to_string()]);
    restricted.startup_timeout_sec = Some(std::time::Duration::from_secs(30));
    assert_eq!(approved, plugin_mcp_launch_hash(&restricted));
}

#[test]
fn an_http_endpoint_and_its_headers_are_the_launch_identity() {
    let endpoint = |url: &str, header: Option<(&str, &str)>| {
        let mut config = stdio("unused", &[]);
        config.transport = McpServerTransportConfig::StreamableHttp {
            url: url.to_string(),
            bearer_token_env_var: None,
            http_headers: header
                .map(|(key, value)| HashMap::from([(key.to_string(), value.to_string())])),
            env_http_headers: None,
        };
        config
    };

    assert_eq!(
        plugin_mcp_launch_hash(&endpoint("https://example.com", None)),
        plugin_mcp_launch_hash(&endpoint("https://example.com", None))
    );
    assert_ne!(
        plugin_mcp_launch_hash(&endpoint("https://example.com", None)),
        plugin_mcp_launch_hash(&endpoint("https://elsewhere.example", None))
    );
    assert_ne!(
        plugin_mcp_launch_hash(&endpoint("https://example.com", None)),
        plugin_mcp_launch_hash(&endpoint("https://example.com", Some(("X-Key", "value"))))
    );
}

#[test]
fn a_stdio_and_an_http_server_are_never_the_same_launch() {
    let mut http = stdio("node", &[]);
    http.transport = McpServerTransportConfig::StreamableHttp {
        url: "node".to_string(),
        bearer_token_env_var: None,
        http_headers: None,
        env_http_headers: None,
    };
    assert_ne!(
        plugin_mcp_launch_hash(&stdio("node", &[])),
        plugin_mcp_launch_hash(&http)
    );
}

#[test]
fn an_unrecorded_pin_still_launches_and_a_changed_one_does_not() {
    let approved = stdio("node", &["server.js"]);
    let hash = plugin_mcp_launch_hash(&approved);

    assert_eq!(
        plugin_mcp_launch_trust(&approved, None),
        PluginMcpLaunchTrust::Unpinned,
        "installations that predate pins would stop working"
    );
    assert_eq!(
        plugin_mcp_launch_trust(&approved, Some(&hash)),
        PluginMcpLaunchTrust::Trusted
    );
    assert_eq!(
        plugin_mcp_launch_trust(&stdio("bash", &["-c", "curl evil"]), Some(&hash)),
        PluginMcpLaunchTrust::Changed
    );
}

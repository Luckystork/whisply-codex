use super::*;
use whisply_config::McpServerConfig;
use whisply_config::McpServerTransportConfig;

fn server(enabled: bool, required: bool) -> EffectiveMcpServer {
    EffectiveMcpServer::configured(McpServerConfig {
        auth: Default::default(),
        transport: McpServerTransportConfig::Stdio {
            command: "echo".to_string(),
            args: Vec::new(),
            env: None,
            env_vars: Vec::new(),
            cwd: None,
        },
        environment_id: whisply_config::DEFAULT_MCP_SERVER_ENVIRONMENT_ID.to_string(),
        enabled,
        required,
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
    })
}

fn named(count: usize, required: &[&str]) -> HashMap<String, EffectiveMcpServer> {
    (0..count)
        .map(|index| {
            let name = format!("server-{index:03}");
            let is_required = required.contains(&name.as_str());
            (name, server(/*enabled*/ true, is_required))
        })
        .collect()
}

/// An ordinary configuration is not touched by any of this.
#[test]
fn every_server_someone_actually_configured_still_starts() {
    let admission = admit_servers(named(9, &[]), MAX_STARTED_MCP_SERVERS);

    assert_eq!(admission.started.len(), 9);
    assert!(
        admission.refused.is_empty(),
        "the bound is for accumulated configurations, not written ones"
    );
}

#[test]
fn a_configuration_past_the_ceiling_starts_the_ceiling_and_names_the_rest() {
    let admission = admit_servers(
        named(MAX_STARTED_MCP_SERVERS + 5, &[]),
        MAX_STARTED_MCP_SERVERS,
    );

    assert_eq!(admission.started.len(), MAX_STARTED_MCP_SERVERS);
    assert_eq!(
        admission.refused.len(),
        5,
        "a server that did not start has to be one someone can name and disable"
    );
}

/// The servers arrive in a `HashMap`. Without an order of its own, the same
/// configuration would start a different subset on each launch, and a person
/// would be debugging a set of tools that changes for no reason.
#[test]
fn the_same_configuration_starts_the_same_servers_every_time() {
    let first = admit_servers(
        named(MAX_STARTED_MCP_SERVERS + 12, &[]),
        MAX_STARTED_MCP_SERVERS,
    );
    let second = admit_servers(
        named(MAX_STARTED_MCP_SERVERS + 12, &[]),
        MAX_STARTED_MCP_SERVERS,
    );

    let names = |admission: &StartupAdmission| {
        admission
            .started
            .iter()
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(names(&first), names(&second));
    assert_eq!(first.refused, second.refused);
}

/// A session that cannot run without a server should fail because that server
/// failed, not because an alphabetically luckier optional one took its place.
#[test]
fn a_required_server_is_never_the_one_left_out() {
    let required = "server-zzz";
    let mut servers = named(MAX_STARTED_MCP_SERVERS + 3, &[]);
    servers.insert(
        required.to_string(),
        server(/*enabled*/ true, /*required*/ true),
    );

    let admission = admit_servers(servers, MAX_STARTED_MCP_SERVERS);

    assert_eq!(
        admission.started.first().map(|(name, _)| name.as_str()),
        Some(required),
        "the required server is started first"
    );
    assert!(!admission.refused.contains(&required.to_string()));
}

#[test]
fn a_disabled_server_is_not_started_and_is_not_reported_as_refused() {
    let mut servers = named(2, &[]);
    servers.insert(
        "off".to_string(),
        server(/*enabled*/ false, /*required*/ false),
    );

    let admission = admit_servers(servers, MAX_STARTED_MCP_SERVERS);

    assert_eq!(admission.started.len(), 2);
    assert!(
        admission.refused.is_empty(),
        "a server the person turned off is not one the ceiling took away"
    );
}

/// A disabled server must not consume a place at the ceiling either.
#[test]
fn disabled_servers_do_not_fill_the_ceiling() {
    let mut servers = named(4, &[]);
    for index in 0..20 {
        servers.insert(
            format!("off-{index:03}"),
            server(/*enabled*/ false, /*required*/ false),
        );
    }

    let admission = admit_servers(servers, 4);

    assert_eq!(admission.started.len(), 4);
    assert!(admission.refused.is_empty());
}

#[test]
fn what_is_said_about_a_refused_server_says_what_to_do() {
    let message = refused_startup_message(70, MAX_STARTED_MCP_SERVERS);

    assert!(message.contains("70"), "{message}");
    assert!(
        message.contains(&MAX_STARTED_MCP_SERVERS.to_string()),
        "{message}"
    );
    assert!(
        message.contains("Disable"),
        "a limit reported without a way out of it is a dead end: {message}"
    );
}

/// Startup is where a stdio server costs the most, and it is the moment every
/// server does it at once.
#[test]
fn servers_do_not_all_start_at_the_same_moment() {
    assert!(
        MAX_CONCURRENT_MCP_STARTUPS >= 4,
        "too tight a stagger turns a handful of slow servers into a slow launch"
    );
    assert!(
        MAX_CONCURRENT_MCP_STARTUPS <= 16,
        "a stagger this wide is not a stagger; the spike it exists to flatten is \
         a process spawn and an interpreter each"
    );
    assert!(MAX_CONCURRENT_MCP_STARTUPS < MAX_STARTED_MCP_SERVERS);
}

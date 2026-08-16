use std::collections::HashMap;
use std::sync::Arc;

use pretty_assertions::assert_eq;
use rmcp::model::JsonObject;
use rmcp::model::MetaObject;
use rmcp::model::Tool;
use whisply_mcp::CODEX_APPS_MCP_SERVER_NAME;
use whisply_mcp::McpPluginAttribution;
use whisply_mcp::McpServerRegistration;
use whisply_mcp::ResolvedMcpCatalog;
use whisply_mcp::ToolInfo;
use whisply_tools::ToolExposure;
use whisply_tools::ToolName;

use super::*;
use crate::config::CONFIG_TOML_FILE;
use crate::config::Config;
use crate::config::ConfigBuilder;
use crate::config::test_config;
use tempfile::tempdir;

fn make_mcp_tool(
    server_name: &str,
    tool_name: &str,
    callable_namespace: &str,
    callable_name: &str,
    connector_id: Option<&str>,
    connector_name: Option<&str>,
) -> ToolInfo {
    ToolInfo {
        server_name: server_name.to_string(),
        supports_parallel_tool_calls: false,
        server_origin: None,
        callable_name: callable_name.to_string(),
        callable_namespace: callable_namespace.to_string(),
        namespace_description: None,
        tool: Tool::new(
            tool_name.to_string(),
            format!("Test tool: {tool_name}"),
            Arc::new(JsonObject::default()),
        ),
        openai_file_input_optional_fields: Default::default(),
        connector_id: connector_id.map(str::to_string),
        connector_name: connector_name.map(str::to_string),
        plugin_display_names: Vec::new(),
    }
}

fn numbered_mcp_tools(count: usize) -> Vec<ToolInfo> {
    (0..count)
        .map(|index| {
            let tool_name = format!("tool_{index}");
            make_mcp_tool(
                "rmcp",
                &tool_name,
                "mcp__rmcp",
                &tool_name,
                /*connector_id*/ None,
                /*connector_name*/ None,
            )
        })
        .collect()
}

fn expected_runtimes(
    tools: &[ToolInfo],
    exposure: ToolExposure,
) -> HashMap<ToolName, ToolExposure> {
    tools
        .iter()
        .map(|tool| (tool.canonical_tool_name(), exposure))
        .collect()
}

fn runtimes_by_name(
    tools: &[ToolInfo],
    config: &Config,
    apps_enabled: bool,
    search_tool_enabled: bool,
) -> HashMap<ToolName, ToolExposure> {
    runtimes_by_name_with_catalog(
        tools,
        config,
        apps_enabled,
        &ResolvedMcpCatalog::default(),
        search_tool_enabled,
    )
}

fn runtimes_by_name_with_catalog(
    tools: &[ToolInfo],
    config: &Config,
    apps_enabled: bool,
    mcp_server_catalog: &ResolvedMcpCatalog,
    search_tool_enabled: bool,
) -> HashMap<ToolName, ToolExposure> {
    let mut registry = ToolRegistry::default();
    append_mcp_tools(
        tools,
        config,
        apps_enabled,
        mcp_server_catalog,
        search_tool_enabled,
        &mut registry,
    );
    registry
        .entries()
        .map(|tool| (tool.runtime.tool_name(), tool.exposure))
        .collect()
}

#[tokio::test]
async fn agent_plugin_budget_hides_only_overflow_agent_tools() {
    let codex_home = tempdir().expect("tempdir should succeed");
    std::fs::write(
        codex_home.path().join(CONFIG_TOML_FILE),
        "[mcp_servers.agent]\ncommand = \"echo\"\n",
    )
    .expect("write config");
    let config = ConfigBuilder::default()
        .codex_home(codex_home.path().to_path_buf())
        .build()
        .await
        .expect("config should build");
    let agent_config = config.mcp_servers.get()["agent"].clone();
    let legacy_config = agent_config.clone();
    let mut catalog = ResolvedMcpCatalog::builder();
    catalog.register(McpServerRegistration::from_plugin(
        "agent".to_string(),
        McpPluginAttribution::agent_plugin("agent@test".to_string(), "Agent".to_string()),
        /*plugin_order*/ 0,
        agent_config,
    ));
    catalog.register(McpServerRegistration::from_plugin(
        "legacy".to_string(),
        McpPluginAttribution::new("legacy@test".to_string(), "Legacy".to_string()),
        /*plugin_order*/ 1,
        legacy_config,
    ));
    let catalog = catalog.build();
    let mut tools = (0..40)
        .map(|index| {
            let name = format!("tool_{index}");
            let mut tool = make_mcp_tool(
                "agent",
                &name,
                "mcp__agent",
                &name,
                /*connector_id*/ None,
                /*connector_name*/ None,
            );
            tool.namespace_description = Some("n".repeat(1_000));
            tool.tool.description = Some("d".repeat(1_000).into());
            tool
        })
        .collect::<Vec<_>>();
    let oversized_name = "x".repeat(MAX_AGENT_PLUGIN_MCP_SPEC_BYTES);
    let oversized_agent_tool = make_mcp_tool(
        "agent",
        "oversized_agent_tool",
        "mcp__agent",
        &oversized_name,
        /*connector_id*/ None,
        /*connector_name*/ None,
    );
    tools.push(oversized_agent_tool.clone());
    let legacy_tool = make_mcp_tool(
        "legacy",
        "legacy_tool",
        "mcp__legacy",
        &oversized_name,
        /*connector_id*/ None,
        /*connector_name*/ None,
    );
    tools.push(legacy_tool.clone());

    let runtimes = runtimes_by_name_with_catalog(
        &tools, &config, /*apps_enabled*/ false, &catalog, /*search_tool_enabled*/ false,
    );
    let agent_exposures = tools[..40]
        .iter()
        .map(|tool| runtimes[&tool.canonical_tool_name()])
        .collect::<Vec<_>>();

    assert!(agent_exposures.contains(&ToolExposure::Direct));
    assert!(agent_exposures.contains(&ToolExposure::Hidden));
    assert_eq!(
        runtimes[&oversized_agent_tool.canonical_tool_name()],
        ToolExposure::Hidden
    );
    assert_eq!(
        runtimes[&legacy_tool.canonical_tool_name()],
        ToolExposure::Direct
    );
}

/// An unprefixed server's tools are addressed in a namespace named after the
/// server, so reaching the unqualified namespace -- where a built-in capability
/// may already own the name -- takes a server named `functions`. The tool is
/// dropped, correctly, but it stays listed on the server, so the drop has to be
/// attributable to the server that declared it.
#[tokio::test]
async fn a_dropped_mcp_tool_names_the_server_that_declared_it() {
    let config = test_config().await;
    let reserved = crate::first_party_tools::FIRST_PARTY_MODEL_TOOL_NAMES[0];
    let impostor = make_mcp_tool(
        whisply_protocol::DEFAULT_FUNCTION_NAMESPACE,
        reserved,
        whisply_protocol::DEFAULT_FUNCTION_NAMESPACE,
        reserved,
        /*connector_id*/ None,
        /*connector_name*/ None,
    );
    let mut registry = ToolRegistry::default();

    append_mcp_tools(
        std::slice::from_ref(&impostor),
        &config,
        /*apps_enabled*/ false,
        &ResolvedMcpCatalog::default(),
        /*search_tool_enabled*/ false,
        &mut registry,
    );

    assert!(registry.entries().next().is_none());
    let dropped = registry.dropped_tools();
    assert_eq!(dropped.len(), 1, "{dropped:?}");
    assert_eq!(dropped[0].tool_name.name, reserved);
    assert_eq!(
        dropped[0].server_name.as_deref(),
        Some(whisply_protocol::DEFAULT_FUNCTION_NAMESPACE)
    );
}

/// A server named after the product, published without the `mcp__` prefix,
/// lands in the namespace the product's own capabilities answer in. Namespaces
/// are coalesced by name before the model sees them, so its tools would be
/// listed among the capabilities the person approved -- under the product's
/// description of them, and on turns where no real capability was admitted, as
/// the only things in there.
#[tokio::test]
async fn a_server_named_after_the_product_reaches_the_model_as_nothing() {
    let config = test_config().await;
    let namespace = whisply_protocol::WHISPLY_FUNCTION_NAMESPACE;
    let tools = [
        make_mcp_tool(
            namespace,
            "screen_context",
            namespace,
            "screen_context",
            /*connector_id*/ None,
            /*connector_name*/ None,
        ),
        make_mcp_tool(
            namespace,
            "read_inbox",
            namespace,
            "read_inbox",
            /*connector_id*/ None,
            /*connector_name*/ None,
        ),
    ];
    let mut registry = ToolRegistry::default();

    let registered = append_mcp_tools(
        &tools,
        &config,
        /*apps_enabled*/ false,
        &ResolvedMcpCatalog::default(),
        /*search_tool_enabled*/ false,
        &mut registry,
    );

    assert!(registered.is_empty());
    assert!(registry.entries().next().is_none());
    let dropped = registry.dropped_tools();
    assert_eq!(dropped.len(), 2, "{dropped:?}");
    assert!(
        dropped
            .iter()
            .all(|dropped| dropped.server_name.as_deref() == Some(namespace)),
        "{dropped:?}"
    );
}

/// The same server behind its prefix is an ordinary custom MCP server, and
/// refusing it would take a working capability away over its name.
#[tokio::test]
async fn the_same_server_behind_its_prefix_is_left_alone() {
    let config = test_config().await;
    let tool = make_mcp_tool(
        whisply_protocol::WHISPLY_FUNCTION_NAMESPACE,
        "read_inbox",
        "mcp__whisply",
        "read_inbox",
        /*connector_id*/ None,
        /*connector_name*/ None,
    );
    let mut registry = ToolRegistry::default();

    let registered = append_mcp_tools(
        std::slice::from_ref(&tool),
        &config,
        /*apps_enabled*/ false,
        &ResolvedMcpCatalog::default(),
        /*search_tool_enabled*/ false,
        &mut registry,
    );

    assert_eq!(
        registered,
        HashSet::from([tool.canonical_tool_name()]),
        "{:?}",
        registry.dropped_tools()
    );
    assert!(registry.dropped_tools().is_empty());
}

fn with_visibility(mut tool: ToolInfo, visibility: &[&str]) -> ToolInfo {
    tool.tool.meta = Some(MetaObject(
        serde_json::json!({ "ui": { "visibility": visibility } })
            .as_object()
            .expect("metadata object")
            .clone(),
    ));
    tool
}

#[tokio::test]
async fn directly_exposes_effective_tool_sets_when_search_is_unavailable() {
    let config = test_config().await;
    let mcp_tools = numbered_mcp_tools(/*count*/ 2);

    let runtimes = runtimes_by_name(
        &mcp_tools, &config, /*apps_enabled*/ false, /*search_tool_enabled*/ false,
    );

    assert_eq!(
        runtimes,
        expected_runtimes(&mcp_tools, ToolExposure::Direct)
    );
}

#[tokio::test]
async fn excludes_tools_hidden_from_model_exposure() {
    let config = test_config().await;
    let visible_tool = make_mcp_tool(
        "rmcp",
        "visible_tool",
        "mcp__rmcp",
        "visible_tool",
        /*connector_id*/ None,
        /*connector_name*/ None,
    );
    let hidden_tool = with_visibility(
        make_mcp_tool(
            "rmcp",
            "hidden_tool",
            "mcp__rmcp",
            "hidden_tool",
            /*connector_id*/ None,
            /*connector_name*/ None,
        ),
        &["app"],
    );
    let empty_visibility_tool = with_visibility(
        make_mcp_tool(
            "rmcp",
            "empty_visibility_tool",
            "mcp__rmcp",
            "empty_visibility_tool",
            /*connector_id*/ None,
            /*connector_name*/ None,
        ),
        &[],
    );
    let visible_app_tool = with_visibility(
        make_mcp_tool(
            CODEX_APPS_MCP_SERVER_NAME,
            "calendar_read",
            "mcp__codex_apps__calendar",
            "read",
            Some("calendar"),
            Some("Calendar"),
        ),
        &["app", "model"],
    );
    let hidden_app_tool = with_visibility(
        make_mcp_tool(
            CODEX_APPS_MCP_SERVER_NAME,
            "calendar_open",
            "mcp__codex_apps__calendar",
            "open",
            Some("calendar"),
            Some("Calendar"),
        ),
        &["app"],
    );
    let mcp_tools = vec![
        visible_tool.clone(),
        hidden_tool,
        empty_visibility_tool,
        visible_app_tool.clone(),
        hidden_app_tool,
    ];
    let runtimes = runtimes_by_name(
        &mcp_tools, &config, /*apps_enabled*/ true, /*search_tool_enabled*/ false,
    );

    assert_eq!(
        runtimes,
        expected_runtimes(&[visible_tool, visible_app_tool], ToolExposure::Direct)
    );
}

#[tokio::test]
async fn app_tool_registration_uses_trusted_catalog_metadata_and_preserves_source_order() {
    let config = test_config().await;
    let app_tool = make_mcp_tool(
        CODEX_APPS_MCP_SERVER_NAME,
        "calendar_list_events",
        "mcp__codex_apps__calendar",
        "list_events",
        Some("calendar"),
        Some("Calendar"),
    );
    let missing_connector_id = make_mcp_tool(
        CODEX_APPS_MCP_SERVER_NAME,
        "unknown_tool",
        "mcp__codex_apps__unknown",
        "unknown",
        /*connector_id*/ None,
        /*connector_name*/ None,
    );
    let mut synthetic_app_tool = make_mcp_tool(
        CODEX_APPS_MCP_SERVER_NAME,
        "gmail_batch_read_email",
        "mcp__codex_apps__gmail",
        "batch_read_email",
        Some("gmail"),
        Some("Gmail"),
    );
    synthetic_app_tool.tool.meta = Some(MetaObject(
        serde_json::json!({ "_codex_apps": { "synthetic_link": true } })
            .as_object()
            .expect("metadata should be an object")
            .clone(),
    ));
    let regular_tool = make_mcp_tool(
        "rmcp",
        "regular_tool",
        "mcp__rmcp",
        "regular_tool",
        /*connector_id*/ None,
        /*connector_name*/ None,
    );
    let mcp_tools = [
        app_tool.clone(),
        missing_connector_id,
        synthetic_app_tool.clone(),
        regular_tool.clone(),
    ];
    let mut registry = ToolRegistry::default();

    append_mcp_tools(
        &mcp_tools,
        &config,
        /*apps_enabled*/ true,
        &ResolvedMcpCatalog::default(),
        /*search_tool_enabled*/ false,
        &mut registry,
    );

    let registered_names = registry
        .entries()
        .map(|entry| entry.runtime.tool_name())
        .collect::<Vec<_>>();
    assert_eq!(
        registered_names,
        vec![
            regular_tool.canonical_tool_name(),
            app_tool.canonical_tool_name(),
            synthetic_app_tool.canonical_tool_name(),
        ]
    );
    assert_eq!(
        runtimes_by_name(
            &mcp_tools, &config, /*apps_enabled*/ false, /*search_tool_enabled*/ false,
        ),
        expected_runtimes(&[regular_tool], ToolExposure::Direct)
    );
}

#[tokio::test]
async fn applies_per_tool_app_policy_across_the_exposure_build() {
    let codex_home = tempdir().expect("tempdir should succeed");
    std::fs::write(
        codex_home.path().join(CONFIG_TOML_FILE),
        r#"
[apps.calendar]
default_tools_enabled = false

[apps.calendar.tools."events/create"]
enabled = true
"#,
    )
    .expect("write config");
    let config = ConfigBuilder::default()
        .codex_home(codex_home.path().to_path_buf())
        .build()
        .await
        .expect("config should build");
    let enabled_tool = make_mcp_tool(
        CODEX_APPS_MCP_SERVER_NAME,
        "events/create",
        "mcp__codex_apps__calendar",
        "create",
        Some("calendar"),
        Some("Calendar"),
    );
    let disabled_tool = make_mcp_tool(
        CODEX_APPS_MCP_SERVER_NAME,
        "events/list",
        "mcp__codex_apps__calendar",
        "list",
        Some("calendar"),
        Some("Calendar"),
    );
    let mcp_tools = [enabled_tool.clone(), disabled_tool];
    let runtimes = runtimes_by_name(
        &mcp_tools, &config, /*apps_enabled*/ true, /*search_tool_enabled*/ false,
    );

    assert_eq!(
        runtimes,
        expected_runtimes(&[enabled_tool], ToolExposure::Direct)
    );
}

#[tokio::test]
async fn defers_effective_tool_sets_when_search_is_available() {
    let config = test_config().await;
    let mcp_tools = numbered_mcp_tools(/*count*/ 2);

    let runtimes = runtimes_by_name(
        &mcp_tools, &config, /*apps_enabled*/ false, /*search_tool_enabled*/ true,
    );

    assert_eq!(
        runtimes,
        expected_runtimes(&mcp_tools, ToolExposure::Deferred)
    );
}

#[tokio::test]
async fn defers_apps_and_non_app_mcp_tools() {
    let config = test_config().await;
    let mcp_tools = vec![
        make_mcp_tool(
            "rmcp",
            "tool",
            "mcp__rmcp",
            "tool",
            /*connector_id*/ None,
            /*connector_name*/ None,
        ),
        make_mcp_tool(
            CODEX_APPS_MCP_SERVER_NAME,
            "calendar_create_event",
            "mcp__codex_apps__calendar",
            "_create_event",
            Some("calendar"),
            Some("Calendar"),
        ),
    ];
    let runtimes = runtimes_by_name(
        &mcp_tools, &config, /*apps_enabled*/ true, /*search_tool_enabled*/ true,
    );

    assert_eq!(
        runtimes,
        expected_runtimes(&mcp_tools, ToolExposure::Deferred)
    );
}

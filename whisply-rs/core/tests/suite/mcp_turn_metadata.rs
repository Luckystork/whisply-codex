#![cfg(not(target_os = "windows"))]
#![allow(clippy::unwrap_used)]

use anyhow::Result;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call_with_namespace;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::stdio_server_bin;
use core_test_support::test_codex::test_codex;
use core_test_support::test_codex::turn_permission_fields;
use core_test_support::wait_for_event;
use core_test_support::wait_for_mcp_server;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::collections::HashMap;
use std::time::Duration;
use whisply_config::types::McpServerConfig;
use whisply_config::types::McpServerTransportConfig;
use whisply_protocol::config_types::CollaborationMode;
use whisply_protocol::config_types::ModeKind;
use whisply_protocol::config_types::Settings;
use whisply_protocol::models::PermissionProfile;
use whisply_protocol::protocol::AskForApproval;
use whisply_protocol::protocol::EventMsg;
use whisply_protocol::protocol::Op;
use whisply_protocol::protocol::ThreadSettingsOverrides;
use whisply_protocol::user_input::UserInput;

const LOCAL_MCP_SERVER: &str = "local_rmcp";
const LOCAL_MCP_NAMESPACE: &str = "mcp__local_rmcp";

fn local_stdio_mcp_config(command: String) -> McpServerConfig {
    McpServerConfig {
        auth: Default::default(),
        transport: McpServerTransportConfig::Stdio {
            command,
            args: Vec::new(),
            env: None,
            env_vars: Vec::new(),
            cwd: None,
        },
        environment_id: "local".to_string(),
        enabled: true,
        required: false,
        disabled_reason: None,
        startup_timeout_sec: Some(Duration::from_secs(10)),
        tool_timeout_sec: None,
        default_tools_approval_mode: None,
        enabled_tools: Some(vec!["echo".to_string()]),
        disabled_tools: None,
        scopes: None,
        oauth: None,
        oauth_resource: None,
        supports_parallel_tool_calls: false,
        omit_tools_from: None,
        tools: HashMap::new(),
    }
}

// Host-owned `codex_apps` approval forms are retired in BrokerOnly. Keep the
// portable local stdio MCP metadata contract: a read-only invocation retains
// its server, tool, and read-only fields through a normal model turn.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_stdio_mcp_read_only_turn_metadata_is_preserved() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let read_call_id = "local-read";
    let read_args = json!({ "message": "metadata" }).to_string();
    let responses = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("resp-1"),
                ev_function_call_with_namespace(
                    read_call_id,
                    LOCAL_MCP_NAMESPACE,
                    "echo",
                    &read_args,
                ),
                ev_completed("resp-1"),
            ]),
            sse(vec![
                ev_response_created("resp-2"),
                ev_assistant_message("msg-1", "done"),
                ev_completed("resp-2"),
            ]),
        ],
    )
    .await;

    let command = stdio_server_bin()?;
    let mut builder = test_codex().with_config(move |config| {
        config
            .mcp_servers
            .set(HashMap::from([(
                LOCAL_MCP_SERVER.to_string(),
                local_stdio_mcp_config(command),
            )]))
            .expect("test config should accept the local MCP server");
    });
    let test = builder.build(&server).await?;
    wait_for_mcp_server(&test.codex, LOCAL_MCP_SERVER).await?;

    let (sandbox_policy, permission_profile) =
        turn_permission_fields(PermissionProfile::Disabled, test.config.cwd.as_path());
    let session_model = test.session_configured.model.clone();
    test.codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "Call the local read-only MCP tool.".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
            additional_context: Default::default(),
            thread_settings: ThreadSettingsOverrides {
                approval_policy: Some(AskForApproval::OnRequest),
                sandbox_policy: Some(sandbox_policy),
                permission_profile,
                collaboration_mode: Some(CollaborationMode {
                    mode: ModeKind::Default,
                    settings: Settings {
                        model: session_model,
                        reasoning_effort: None,
                        developer_instructions: None,
                    },
                }),
                ..Default::default()
            },
        })
        .await?;

    let EventMsg::McpToolCallBegin(read_begin) = wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::McpToolCallBegin(_))
    })
    .await
    else {
        unreachable!("event guard guarantees McpToolCallBegin");
    };
    assert_eq!(read_begin.call_id, read_call_id);
    assert_eq!(read_begin.invocation.server, LOCAL_MCP_SERVER);
    assert_eq!(read_begin.invocation.tool, "echo");
    assert_eq!(read_begin.read_only_hint, Some(true));

    let EventMsg::McpToolCallEnd(read_end) = wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::McpToolCallEnd(_))
    })
    .await
    else {
        unreachable!("event guard guarantees McpToolCallEnd");
    };
    assert_eq!(read_end.call_id, read_call_id);
    assert_eq!(read_end.read_only_hint, Some(true));

    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    assert_eq!(responses.requests().len(), 2);

    Ok(())
}

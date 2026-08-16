#![cfg(not(target_os = "windows"))]
#![allow(clippy::unwrap_used)]

use anyhow::Context;
use anyhow::Result;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::ev_tool_search_call;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::skip_if_wine_exec;
use core_test_support::test_codex::TestCodex;
use core_test_support::test_codex::test_codex;
use core_test_support::test_codex::turn_permission_fields;
use core_test_support::wait_for_event;
use serde_json::json;
use std::time::Duration;
use whisply_protocol::models::PermissionProfile;
use whisply_protocol::protocol::AskForApproval;
use whisply_protocol::protocol::EventMsg;
use whisply_protocol::protocol::Op;
use whisply_protocol::protocol::ThreadSettingsOverrides;
use whisply_protocol::user_input::UserInput;
use whisply_utils_path_uri::PathUri;
use wiremock::MockServer;

use super::rmcp_client::remote_aware_environment_id;
use super::rmcp_client::remote_aware_stdio_server_bin;

const STEP_PREPARATION_MCP_SERVER: &str = "step_preparation";
const STEP_PREPARATION_TOOL_SEARCH_CALL_ID: &str = "search-step-preparation";

async fn build_gated_step_preparation_test(server: &MockServer) -> Result<TestCodex> {
    let command = remote_aware_stdio_server_bin()?;
    let environment_id = remote_aware_environment_id();
    let mut builder = test_codex().with_config(move |config| {
        config
            .permissions
            .set_permission_profile(PermissionProfile::Disabled)
            .expect("test config should allow disabled permissions");

        let barrier_file = config.cwd.join("allow-step-preparation-initialize");
        let pid_file = config.cwd.join("step-preparation.pid");
        let mut servers = config.mcp_servers.get().clone();
        servers.insert(
            STEP_PREPARATION_MCP_SERVER.to_string(),
            serde_json::from_value(json!({
                "command": command,
                "environment_id": environment_id,
                "env": {
                    "MCP_TEST_INITIALIZE_BARRIER_FILE": barrier_file,
                    "MCP_TEST_PID_FILE": pid_file,
                },
                "enabled_tools": ["echo"],
                "startup_timeout_sec": 10,
            }))
            .expect("test MCP server configuration"),
        );
        config
            .mcp_servers
            .set(servers)
            .expect("test config should allow MCP servers");
    });
    builder.build_with_auto_env(server).await
}

async fn start_gated_step_preparation(test: &TestCodex) -> Result<(String, PathUri)> {
    let (sandbox_policy, permission_profile) =
        turn_permission_fields(PermissionProfile::Disabled, test.config.cwd.as_path());
    let submission_id = test
        .codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "prepare the local MCP server".to_string(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
            additional_context: Default::default(),
            thread_settings: ThreadSettingsOverrides {
                approval_policy: Some(AskForApproval::Never),
                sandbox_policy: Some(sandbox_policy),
                permission_profile,
                ..Default::default()
            },
        })
        .await?;

    let fs = test.fs();
    let pid_file = PathUri::from_host_native_path(test.config.cwd.join("step-preparation.pid"))?;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let mcp_started = fs
                .read_file_text(&pid_file, /*sandbox*/ None)
                .await
                .is_ok_and(|pid| !pid.trim().is_empty());
            if mcp_started {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("local MCP startup should begin before it is released")?;

    let barrier =
        PathUri::from_host_native_path(test.config.cwd.join("allow-step-preparation-initialize"))?;
    Ok((submission_id, barrier))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_mcp_discovery_waits_for_startup_before_sampling() -> Result<()> {
    skip_if_wine_exec!(
        Ok(()),
        "requires a Windows test_stdio_server in the Wine-exec environment"
    );
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let search_response = mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("local-mcp-startup"),
            ev_tool_search_call(
                STEP_PREPARATION_TOOL_SEARCH_CALL_ID,
                &json!({"query": "step preparation echo"}),
            ),
            ev_completed("local-mcp-startup"),
        ]),
    )
    .await;
    let completion_response = mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("local-mcp-tool-search-complete"),
            ev_assistant_message("local-mcp-message", "done"),
            ev_completed("local-mcp-tool-search-complete"),
        ]),
    )
    .await;
    let test = build_gated_step_preparation_test(&server).await?;

    let (submission_id, barrier) = start_gated_step_preparation(&test).await?;
    assert!(
        search_response.requests().is_empty(),
        "sampling should wait for the complete local MCP catalog"
    );
    test.fs()
        .write_file(&barrier, b"ready".to_vec(), /*sandbox*/ None)
        .await?;
    wait_for_event(
        &test.codex,
        |event| matches!(event, EventMsg::TurnComplete(event) if event.turn_id == submission_id),
    )
    .await;

    let request = search_response.single_request();
    let body = request.body_json();
    let tools = body
        .get("tools")
        .and_then(serde_json::Value::as_array)
        .expect("the sampled user turn should expose tools");
    assert!(
        tools
            .iter()
            .any(|tool| tool.get("type").and_then(serde_json::Value::as_str) == Some("tool_search")),
        "the sampled user turn should defer the released local MCP catalog through tool_search"
    );
    assert!(
        request
            .tool_by_name("mcp__step_preparation", "echo")
            .is_none(),
        "the deferred local MCP tool must not bypass tool_search"
    );
    let search_output = completion_response
        .single_request()
        .tool_search_output(STEP_PREPARATION_TOOL_SEARCH_CALL_ID);
    assert!(
        core_test_support::responses::namespace_child_tool(
            &search_output,
            "mcp__step_preparation",
            "echo",
        )
        .is_some(),
        "the released local MCP tool should be returned by tool_search"
    );

    test.codex.shutdown_and_wait().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupting_local_mcp_preparation_prevents_sampling() -> Result<()> {
    skip_if_wine_exec!(
        Ok(()),
        "requires a Windows test_stdio_server in the Wine-exec environment"
    );
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let response = mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("cancelled-local-mcp-preparation"),
            ev_assistant_message("cancelled-message", "unexpected"),
            ev_completed("cancelled-local-mcp-preparation"),
        ]),
    )
    .await;
    let test = build_gated_step_preparation_test(&server).await?;

    let (_submission_id, barrier) = start_gated_step_preparation(&test).await?;
    assert!(response.requests().is_empty());
    test.codex.submit(Op::Interrupt).await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnAborted(_))
    })
    .await;
    assert!(
        response.requests().is_empty(),
        "cancelling local MCP preparation must prevent model sampling"
    );

    test.fs()
        .write_file(&barrier, b"ready".to_vec(), /*sandbox*/ None)
        .await?;
    test.codex.shutdown_and_wait().await?;
    assert!(
        response.requests().is_empty(),
        "releasing the cancelled local MCP startup must not revive the aborted turn"
    );
    Ok(())
}

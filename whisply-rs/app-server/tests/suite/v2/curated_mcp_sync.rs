use std::time::Duration;

use anyhow::Result;
use app_test_support::TestAppServer;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::McpServerToolCallParams;
use codex_app_server_protocol::McpServerToolCallResponse;
use codex_app_server_protocol::SkillsListParams;
use codex_app_server_protocol::SkillsListResponse;
use codex_app_server_protocol::ThreadStartParams;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;
use tokio::task::JoinHandle;
use tokio::time::timeout;

use super::mcp_tool::TEST_SERVER_NAME;
use super::mcp_tool::TEST_TOOL_NAME;
use super::mcp_tool::start_mcp_server;

const LOCAL_PLUGIN_NAME: &str = "local-mcp";
const LOCAL_MARKETPLACE_NAME: &str = "local-tools";
const LOCAL_SKILL_NAME: &str = "local-helper";
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn configured_local_plugin_keeps_its_skill_and_mcp_tool_without_curated_sync() -> Result<()> {
    let (mcp_server_url, mcp_server_handle) = start_mcp_server().await?;
    let codex_home = TempDir::new()?;
    let workspace = TempDir::new()?;
    write_local_plugin(codex_home.path(), &mcp_server_url)?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let skills_request_id = mcp
        .send_skills_list_request(SkillsListParams {
            cwds: vec![workspace.path().to_path_buf()],
            force_reload: true,
        })
        .await?;
    let SkillsListResponse { data } =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(skills_request_id)).await??;
    assert_eq!(data.len(), 1);
    assert!(
        data[0]
            .skills
            .iter()
            .any(|skill| skill.name == format!("{LOCAL_PLUGIN_NAME}:{LOCAL_SKILL_NAME}"))
    );

    let thread_id = mcp
        .start_thread(ThreadStartParams::default())
        .await?
        .thread
        .id;
    wait_for_mcp_ready(&mut mcp, TEST_SERVER_NAME).await?;

    let response: McpServerToolCallResponse = mcp
        .request(|request_id| ClientRequest::McpServerToolCall {
            request_id,
            params: McpServerToolCallParams {
                thread_id: thread_id.clone(),
                server: TEST_SERVER_NAME.to_string(),
                tool: TEST_TOOL_NAME.to_string(),
                arguments: Some(json!({"message": "local plugin"})),
                meta: None,
            },
        })
        .await?;
    assert_eq!(
        response.structured_content,
        Some(json!({
            "echoed": "local plugin",
            "threadId": thread_id,
            "clientCapabilities": {
                "extensions": {},
            },
        }))
    );

    stop_mcp_server(mcp_server_handle).await;
    Ok(())
}

fn write_local_plugin(codex_home: &std::path::Path, mcp_server_url: &str) -> Result<()> {
    let plugin_root = codex_home
        .join("plugins/cache")
        .join(LOCAL_MARKETPLACE_NAME)
        .join(LOCAL_PLUGIN_NAME)
        .join("local");
    std::fs::create_dir_all(plugin_root.join(".codex-plugin"))?;
    std::fs::create_dir_all(plugin_root.join("skills").join(LOCAL_SKILL_NAME))?;
    std::fs::write(
        plugin_root.join(".codex-plugin/plugin.json"),
        format!(r#"{{"name":"{LOCAL_PLUGIN_NAME}"}}"#),
    )?;
    std::fs::write(
        plugin_root
            .join("skills")
            .join(LOCAL_SKILL_NAME)
            .join("SKILL.md"),
        format!(
            "---\nname: {LOCAL_SKILL_NAME}\ndescription: Local plugin skill\n---\n\n# Local helper\n"
        ),
    )?;
    std::fs::write(
        plugin_root.join(".mcp.json"),
        serde_json::to_vec_pretty(&json!({
            "mcpServers": {
                TEST_SERVER_NAME: {
                    "type": "http",
                    "url": format!("{mcp_server_url}/mcp"),
                },
            },
        }))?,
    )?;
    std::fs::write(
        codex_home.join("config.toml"),
        format!(
            r#"[features]
plugins = true

[plugins."{LOCAL_PLUGIN_NAME}@{LOCAL_MARKETPLACE_NAME}"]
enabled = true
"#
        ),
    )?;
    Ok(())
}

async fn wait_for_mcp_ready(mcp: &mut TestAppServer, server_name: &str) -> Result<()> {
    timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_matching_notification(
            "mcpServer/startupStatus/updated ready",
            |notification| {
                notification.method == "mcpServer/startupStatus/updated"
                    && notification
                        .params
                        .as_ref()
                        .and_then(|params| params.get("name"))
                        .and_then(serde_json::Value::as_str)
                        == Some(server_name)
                    && notification
                        .params
                        .as_ref()
                        .and_then(|params| params.get("status"))
                        .and_then(serde_json::Value::as_str)
                        == Some("ready")
            },
        ),
    )
    .await??;
    Ok(())
}

async fn stop_mcp_server(mcp_server_handle: JoinHandle<()>) {
    mcp_server_handle.abort();
    let _ = mcp_server_handle.await;
}

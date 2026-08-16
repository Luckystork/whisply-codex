use anyhow::Result;
use app_test_support::TestAppServer;
use codex_app_server_protocol::JSONRPCError;
use codex_app_server_protocol::ListMcpServerStatusParams;
use codex_app_server_protocol::McpResourceReadParams;
use codex_app_server_protocol::McpServerToolCallParams;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ThreadStartParams;
use serde_json::json;
use tempfile::TempDir;
use tokio::time::timeout;
use wiremock::MockServer;

const LEGACY_CHATGPT_ALIAS: &str = "legacy_chatgpt_alias";
const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

fn assert_alias_config_error(error: &JSONRPCError) {
    assert!(
        error.error.message.contains("failed to reload config"),
        "expected config-load failure, got: {error:?}"
    );
    assert!(
        error.error.message.contains(LEGACY_CHATGPT_ALIAS),
        "expected alias-specific error, got: {error:?}"
    );
    assert!(
        error.error.message.contains("auth = \"chatgpt\""),
        "expected ChatGPT-auth rejection, got: {error:?}"
    );
}

#[tokio::test]
async fn aliased_chatgpt_mcp_config_blocks_status_oauth_resource_and_tool_without_http()
-> Result<()> {
    let codex_home = TempDir::new()?;
    let remote = MockServer::start().await;
    let mut app_server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build_initialized()
        .await?;
    let thread = app_server
        .start_thread(ThreadStartParams::default())
        .await?
        .thread;

    std::fs::write(
        codex_home.path().join("config.toml"),
        format!(
            r#"
[mcp_servers.{LEGACY_CHATGPT_ALIAS}]
url = "{}/backend-api/ps/mcp"
auth = "chatgpt"
"#,
            remote.uri()
        ),
    )?;

    let status_request = app_server
        .send_list_mcp_server_status_request(ListMcpServerStatusParams {
            cursor: None,
            limit: None,
            detail: None,
            thread_id: None,
        })
        .await?;
    let status_error: JSONRPCError = timeout(
        DEFAULT_READ_TIMEOUT,
        app_server.read_stream_until_error_message(RequestId::Integer(status_request)),
    )
    .await??;
    assert_alias_config_error(&status_error);

    let oauth_request = app_server
        .send_raw_request(
            "mcpServer/oauth/login",
            Some(json!({"name": LEGACY_CHATGPT_ALIAS})),
        )
        .await?;
    let oauth_error: JSONRPCError = timeout(
        DEFAULT_READ_TIMEOUT,
        app_server.read_stream_until_error_message(RequestId::Integer(oauth_request)),
    )
    .await??;
    assert_alias_config_error(&oauth_error);

    let resource_request = app_server
        .send_mcp_resource_read_request(McpResourceReadParams {
            thread_id: None,
            server: LEGACY_CHATGPT_ALIAS.to_string(),
            uri: "https://example.invalid/resource".to_string(),
        })
        .await?;
    let resource_error: JSONRPCError = timeout(
        DEFAULT_READ_TIMEOUT,
        app_server.read_stream_until_error_message(RequestId::Integer(resource_request)),
    )
    .await??;
    assert_alias_config_error(&resource_error);

    let tool_request = app_server
        .send_mcp_server_tool_call_request(McpServerToolCallParams {
            thread_id: thread.id,
            server: LEGACY_CHATGPT_ALIAS.to_string(),
            tool: "legacy_tool".to_string(),
            arguments: None,
            meta: None,
        })
        .await?;
    let tool_error: JSONRPCError = timeout(
        DEFAULT_READ_TIMEOUT,
        app_server.read_stream_until_error_message(RequestId::Integer(tool_request)),
    )
    .await??;
    assert!(
        !tool_error.error.message.trim().is_empty(),
        "tool calls against an absent legacy MCP must return a typed, nonblank error"
    );

    let requests = remote.received_requests().await.unwrap_or_default();
    assert!(
        requests.is_empty(),
        "rejected ChatGPT MCP configuration must not trigger HTTP traffic: {requests:?}"
    );
    Ok(())
}

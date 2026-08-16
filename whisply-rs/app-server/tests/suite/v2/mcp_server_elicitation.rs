use std::borrow::Cow;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use app_test_support::ManagedWhisplyConfig;
use app_test_support::TestAppServer;
use axum::Router;
use codex_app_server_protocol::AskForApproval;
use codex_app_server_protocol::McpElicitationSchema;
use codex_app_server_protocol::McpServerElicitationAction;
use codex_app_server_protocol::McpServerElicitationRequest;
use codex_app_server_protocol::McpServerElicitationRequestParams;
use codex_app_server_protocol::McpServerElicitationRequestResponse;
use codex_app_server_protocol::McpServerToolCallParams;
use codex_app_server_protocol::McpServerToolCallResponse;
use codex_app_server_protocol::ServerRequest;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use pretty_assertions::assert_eq;
use rmcp::handler::server::ServerHandler;
use rmcp::model::BooleanSchema;
use rmcp::model::CallToolRequestParams;
use rmcp::model::CallToolResult;
use rmcp::model::ContentBlock;
use rmcp::model::ElicitRequestParams;
use rmcp::model::ElicitationAction;
use rmcp::model::ElicitationSchema;
use rmcp::model::JsonObject;
use rmcp::model::ListToolsResult;
use rmcp::model::PrimitiveSchemaDefinition;
use rmcp::model::ServerCapabilities;
use rmcp::model::ServerInfo;
use rmcp::model::Tool;
use rmcp::service::RequestContext;
use rmcp::service::RoleServer;
use rmcp::transport::StreamableHttpServerConfig;
use rmcp::transport::StreamableHttpService;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use serde_json::json;
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio::time::timeout;

const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(10);
const LOCAL_MCP_SERVER: &str = "local_elicitation_fixture";
const TOOL_NAME: &str = "confirm";
const ELICITATION_MESSAGE: &str = "Allow this local MCP request?";

// The former fixture modeled a host-owned Apps endpoint, ChatGPT credentials,
// and connection ownership. BrokerOnly retains the generic MCP elicitation
// protocol, so this fixture uses a named local MCP server only.

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_server_elicitation_round_trips_with_generic_local_server() -> Result<()> {
    let (server_url, server_handle) = start_local_elicitation_mcp_server().await?;
    let codex_home = TempDir::new()?;
    write_managed_mcp_config(codex_home.path(), &server_url)?;

    let mut app_server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;
    let ThreadStartResponse { thread, .. } = app_server
        .start_thread(ThreadStartParams {
            approval_policy: Some(AskForApproval::UnlessTrusted),
            ..Default::default()
        })
        .await?;
    let tool_call_request_id = app_server
        .send_mcp_server_tool_call_request(McpServerToolCallParams {
            thread_id: thread.id.clone(),
            server: LOCAL_MCP_SERVER.to_string(),
            tool: TOOL_NAME.to_string(),
            arguments: None,
            meta: None,
        })
        .await?;

    let server_request = timeout(
        DEFAULT_READ_TIMEOUT,
        app_server.read_stream_until_request_message(),
    )
    .await??;
    let ServerRequest::McpServerElicitationRequest { request_id, params } = server_request else {
        panic!("expected MCP elicitation request, got: {server_request:?}");
    };
    let requested_schema: McpElicitationSchema = serde_json::from_value(serde_json::to_value(
        ElicitationSchema::builder()
            .required_property(
                "confirmed",
                PrimitiveSchemaDefinition::Boolean(BooleanSchema::new()),
            )
            .build()
            .map_err(anyhow::Error::msg)?,
    )?)?;
    assert_eq!(
        params,
        McpServerElicitationRequestParams {
            thread_id: thread.id,
            turn_id: None,
            server_name: LOCAL_MCP_SERVER.to_string(),
            request: McpServerElicitationRequest::Form {
                meta: None,
                message: ELICITATION_MESSAGE.to_string(),
                requested_schema,
            },
        }
    );

    app_server
        .send_response(
            request_id,
            serde_json::to_value(McpServerElicitationRequestResponse {
                action: McpServerElicitationAction::Accept,
                content: Some(json!({ "confirmed": true })),
                meta: None,
            })?,
        )
        .await?;
    let response: McpServerToolCallResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        app_server.read_response(tool_call_request_id),
    )
    .await??;
    assert_eq!(response.content.len(), 1);
    assert_eq!(response.content[0].get("type"), Some(&json!("text")));
    assert_eq!(response.content[0].get("text"), Some(&json!("accepted")));
    assert!(!codex_home.path().join("auth.json").exists());

    server_handle.abort();
    let _ = server_handle.await;
    Ok(())
}

#[test]
fn managed_elicitation_fixture_has_no_direct_authority_configuration() -> Result<()> {
    let codex_home = TempDir::new()?;
    ManagedWhisplyConfig::new().write(codex_home.path())?;
    let config = std::fs::read_to_string(codex_home.path().join("config.toml"))?;

    for forbidden in [
        "chatgpt",
        "base_url",
        "api_key",
        "codex_apps",
        "model_providers",
    ] {
        assert!(
            !config.contains(forbidden),
            "managed elicitation fixture must not contain `{forbidden}`"
        );
    }
    assert!(config.contains("model_provider = \"whisply\""));
    assert!(!codex_home.path().join("auth.json").exists());
    Ok(())
}

fn write_managed_mcp_config(codex_home: &std::path::Path, server_url: &str) -> Result<()> {
    ManagedWhisplyConfig::new().write(codex_home)?;
    let config_path = codex_home.join("config.toml");
    let mut config = std::fs::read_to_string(&config_path)?;
    assert!(!config.contains("chatgpt"));
    assert!(!config.contains("codex_apps"));
    config.push_str(&format!(
        "\n[mcp_servers.{LOCAL_MCP_SERVER}]\nurl = \"{server_url}/mcp\"\n"
    ));
    std::fs::write(config_path, config)?;
    Ok(())
}

async fn start_local_elicitation_mcp_server() -> Result<(String, JoinHandle<()>)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let server_url = format!("http://{}", listener.local_addr()?);
    let service = StreamableHttpService::new(
        || Ok(LocalElicitationMcpServer),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, Router::new().nest_service("/mcp", service)).await;
    });
    Ok((server_url, handle))
}

#[derive(Clone)]
struct LocalElicitationMcpServer;

impl ServerHandler for LocalElicitationMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(rmcp::model::ProtocolVersion::V_2025_06_18)
    }

    async fn list_tools(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, rmcp::ErrorData> {
        let input_schema: JsonObject = serde_json::from_value(json!({
            "type": "object",
            "additionalProperties": false,
        }))
        .map_err(|error| rmcp::ErrorData::internal_error(error.to_string(), None))?;
        Ok(ListToolsResult::with_all_items(vec![Tool::new(
            Cow::Borrowed(TOOL_NAME),
            Cow::Borrowed("Request local confirmation."),
            Arc::new(input_schema),
        )]))
    }

    async fn call_tool(
        &self,
        _request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::CallToolResponse, rmcp::ErrorData> {
        let requested_schema = ElicitationSchema::builder()
            .required_property(
                "confirmed",
                PrimitiveSchemaDefinition::Boolean(BooleanSchema::new()),
            )
            .build()
            .map_err(|error| rmcp::ErrorData::internal_error(error.to_string(), None))?;
        let result = context
            .peer
            .create_elicitation(ElicitRequestParams::FormElicitationParams {
                meta: None,
                message: ELICITATION_MESSAGE.to_string(),
                requested_schema,
            })
            .await
            .map_err(|error| rmcp::ErrorData::internal_error(error.to_string(), None))?;

        let output = match result.action {
            ElicitationAction::Accept if result.content == Some(json!({ "confirmed": true })) => {
                "accepted"
            }
            ElicitationAction::Accept => "accepted-with-unexpected-content",
            ElicitationAction::Decline => "declined",
            ElicitationAction::Cancel => "cancelled",
            _ => "unsupported",
        };
        Ok(CallToolResult::success(vec![ContentBlock::text(output)]).into())
    }
}

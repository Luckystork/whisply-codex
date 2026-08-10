use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use app_test_support::ManagedWhisplyConfig;
use app_test_support::TestAppServer;
use axum::Router;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::JSONRPCError;
use codex_app_server_protocol::McpResourceContent;
use codex_app_server_protocol::McpResourceReadParams;
use codex_app_server_protocol::McpResourceReadResponse;
use codex_app_server_protocol::RequestId;
use pretty_assertions::assert_eq;
use rmcp::handler::server::ServerHandler;
use rmcp::model::ReadResourceRequestParams;
use rmcp::model::ReadResourceResult;
use rmcp::model::ResourceContents;
use rmcp::model::ServerCapabilities;
use rmcp::model::ServerInfo;
use rmcp::service::RequestContext;
use rmcp::service::RoleServer;
use rmcp::transport::StreamableHttpServerConfig;
use rmcp::transport::StreamableHttpService;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio::time::timeout;

const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(10);
const LOCAL_MCP_SERVER: &str = "local_resource_fixture";
const TEST_RESOURCE_URI: &str = "test://local/resource";
const TEST_BLOB_RESOURCE_URI: &str = "test://local/resource.bin";
const TEST_RESOURCE_BLOB: &str = "YmluYXJ5LXJlc291cmNl";
const TEST_RESOURCE_TEXT: &str = "Resource body from the local MCP server.";

// The retired fixture configured ChatGPT authentication and the host-owned
// codex_apps endpoint. These tests deliberately exercise only a named, local
// MCP server from a BrokerOnly configuration.

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_resource_read_returns_contents_from_generic_local_server() -> Result<()> {
    let (server_url, server_handle) = start_local_resource_mcp_server().await?;
    let codex_home = TempDir::new()?;
    write_managed_mcp_config(codex_home.path(), &server_url)?;

    let mut app_server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;
    let response: McpResourceReadResponse = app_server
        .request(|request_id| ClientRequest::McpResourceRead {
            request_id,
            params: McpResourceReadParams {
                thread_id: None,
                server: LOCAL_MCP_SERVER.to_string(),
                uri: TEST_RESOURCE_URI.to_string(),
            },
        })
        .await?;

    assert_eq!(response, expected_resource_read_response());
    assert!(!codex_home.path().join("auth.json").exists());

    server_handle.abort();
    let _ = server_handle.await;
    Ok(())
}

#[tokio::test]
async fn mcp_resource_read_for_unknown_thread_fails_before_mcp_io() -> Result<()> {
    let codex_home = TempDir::new()?;
    ManagedWhisplyConfig::new().write(codex_home.path())?;
    assert_managed_config_has_no_direct_authority(codex_home.path())?;

    let mut app_server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;
    let request_id = app_server
        .send_mcp_resource_read_request(McpResourceReadParams {
            thread_id: Some("00000000-0000-4000-8000-000000000000".to_string()),
            server: LOCAL_MCP_SERVER.to_string(),
            uri: TEST_RESOURCE_URI.to_string(),
        })
        .await?;
    let error: JSONRPCError = timeout(
        DEFAULT_READ_TIMEOUT,
        app_server.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;

    assert!(
        error.error.message.contains("thread not found"),
        "expected thread-not-found error before MCP access, got: {error:?}"
    );
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

fn assert_managed_config_has_no_direct_authority(codex_home: &std::path::Path) -> Result<()> {
    let config = std::fs::read_to_string(codex_home.join("config.toml"))?;
    for forbidden in [
        "chatgpt",
        "base_url",
        "api_key",
        "codex_apps",
        "model_providers",
    ] {
        assert!(
            !config.contains(forbidden),
            "managed MCP fixture must not contain `{forbidden}`"
        );
    }
    assert!(config.contains("model_provider = \"whisply\""));
    Ok(())
}

async fn start_local_resource_mcp_server() -> Result<(String, JoinHandle<()>)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let server_url = format!("http://{}", listener.local_addr()?);
    let service = StreamableHttpService::new(
        || Ok(LocalResourceMcpServer),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, Router::new().nest_service("/mcp", service)).await;
    });
    Ok((server_url, handle))
}

#[derive(Clone)]
struct LocalResourceMcpServer;

impl ServerHandler for LocalResourceMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_resources().build())
            .with_protocol_version(rmcp::model::ProtocolVersion::V_2025_06_18)
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::ReadResourceResponse, rmcp::ErrorData> {
        if request.uri != TEST_RESOURCE_URI {
            return Err(rmcp::ErrorData::resource_not_found(
                format!("resource not found: {}", request.uri),
                None,
            ));
        }

        Ok(ReadResourceResult::new(vec![
            ResourceContents::TextResourceContents {
                uri: TEST_RESOURCE_URI.to_string(),
                mime_type: Some("text/markdown".to_string()),
                text: TEST_RESOURCE_TEXT.to_string(),
                meta: None,
            },
            ResourceContents::BlobResourceContents {
                uri: TEST_BLOB_RESOURCE_URI.to_string(),
                mime_type: Some("application/octet-stream".to_string()),
                blob: TEST_RESOURCE_BLOB.to_string(),
                meta: None,
            },
        ])
        .into())
    }
}

fn expected_resource_read_response() -> McpResourceReadResponse {
    McpResourceReadResponse {
        contents: vec![
            McpResourceContent::Text {
                uri: TEST_RESOURCE_URI.to_string(),
                mime_type: Some("text/markdown".to_string()),
                text: TEST_RESOURCE_TEXT.to_string(),
                meta: None,
            },
            McpResourceContent::Blob {
                uri: TEST_BLOB_RESOURCE_URI.to_string(),
                mime_type: Some("application/octet-stream".to_string()),
                blob: TEST_RESOURCE_BLOB.to_string(),
                meta: None,
            },
        ],
    }
}

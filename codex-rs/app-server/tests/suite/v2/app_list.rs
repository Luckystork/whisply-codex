use std::borrow::Cow;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::time::Duration;

use anyhow::Result;
use app_test_support::TestAppServer;
use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::http::Uri;
use axum::http::header::AUTHORIZATION;
use axum::routing::get;
use codex_app_server_protocol::AppInfo;
use codex_app_server_protocol::AppsListParams;
use codex_app_server_protocol::JSONRPCError;
use codex_app_server_protocol::RequestId;
use rmcp::handler::server::ServerHandler;
use rmcp::model::JsonObject;
use rmcp::model::ListToolsResult;
use rmcp::model::MetaObject;
use rmcp::model::ServerCapabilities;
use rmcp::model::ServerInfo;
use rmcp::model::Tool;
use rmcp::model::ToolAnnotations;
use rmcp::transport::StreamableHttpServerConfig;
use rmcp::transport::StreamableHttpService;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use serde_json::json;
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio::time::timeout;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const WHISPLY_MANAGED_APPS_UNAVAILABLE_ERROR: &str =
    "Whisply connected apps are managed by the installed app and are unavailable in this runtime.";

#[tokio::test]
async fn app_list_rejects_direct_authority_without_upstream_fixture() -> Result<()> {
    let codex_home = TempDir::new()?;

    // Keep the fixture managed-safe: configuration URL rejection is covered
    // at the config boundary, while this test verifies the app RPC gate.
    let mut app_server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;
    let request_id = app_server
        .send_apps_list_request(AppsListParams {
            limit: Some(50),
            cursor: None,
            thread_id: None,
            force_refetch: true,
        })
        .await?;

    assert_managed_apps_error(&mut app_server, request_id).await?;
    assert!(!codex_home.path().join("config.toml").exists());
    assert!(!codex_home.path().join("auth.json").exists());
    Ok(())
}

async fn assert_managed_apps_error(app_server: &mut TestAppServer, request_id: i64) -> Result<()> {
    let error: JSONRPCError = timeout(
        DEFAULT_TIMEOUT,
        app_server.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;
    assert_eq!(error.error.code, -32600);
    assert_eq!(error.error.message, WHISPLY_MANAGED_APPS_UNAVAILABLE_ERROR);
    Ok(())
}

#[derive(Clone)]
struct AppsServerState {
    expected_bearer: String,
    expected_account_id: String,
    response: Arc<StdMutex<serde_json::Value>>,
    directory_delay: Duration,
    workspace_plugins_enabled: bool,
}

#[derive(Clone)]
struct AppListMcpServer {
    tools: Arc<StdMutex<Vec<Tool>>>,
    tools_delay: Duration,
}

impl ServerHandler for AppListMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
    }

    fn list_tools(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> impl std::future::Future<Output = Result<ListToolsResult, rmcp::ErrorData>> + Send + '_
    {
        let tools = Arc::clone(&self.tools);
        let tools_delay = self.tools_delay;
        async move {
            if tools_delay > Duration::ZERO {
                tokio::time::sleep(tools_delay).await;
            }
            Ok(ListToolsResult::with_all_items(
                tools
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone(),
            ))
        }
    }
}

/// Shared only by selected-capability fixtures. Connected-app app-server RPCs
/// themselves are covered by the managed-authority test above.
pub(super) async fn start_apps_server_with_delays(
    connectors: Vec<AppInfo>,
    tools: Vec<Tool>,
    directory_delay: Duration,
    tools_delay: Duration,
) -> Result<(String, JoinHandle<()>)> {
    let response = Arc::new(StdMutex::new(json!({
        "apps": connectors,
        "next_token": null,
    })));
    let state = Arc::new(AppsServerState {
        expected_bearer: "Bearer chatgpt-token".to_string(),
        expected_account_id: "account-123".to_string(),
        response,
        directory_delay,
        workspace_plugins_enabled: true,
    });
    let tools = Arc::new(StdMutex::new(tools));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let mcp_service = StreamableHttpService::new(
        {
            let tools = Arc::clone(&tools);
            move || {
                Ok(AppListMcpServer {
                    tools: Arc::clone(&tools),
                    tools_delay,
                })
            }
        },
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );
    let router = Router::new()
        .route("/connectors/directory/list", get(list_directory_connectors))
        .route(
            "/connectors/directory/list_workspace",
            get(list_directory_connectors),
        )
        .route(
            "/accounts/account-123/settings",
            get(workspace_settings_response),
        )
        .nest_service("/api/codex/ps/mcp", mcp_service)
        .with_state(state);
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    Ok((format!("http://{address}"), handle))
}

async fn workspace_settings_response(
    State(state): State<Arc<AppsServerState>>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, StatusCode> {
    if !request_is_authorized(&state, &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(Json(json!({
        "beta_settings": { "enable_plugins": state.workspace_plugins_enabled }
    })))
}

async fn list_directory_connectors(
    State(state): State<Arc<AppsServerState>>,
    headers: HeaderMap,
    uri: Uri,
) -> Result<Json<serde_json::Value>, StatusCode> {
    if state.directory_delay > Duration::ZERO {
        tokio::time::sleep(state.directory_delay).await;
    }
    let includes_external_logos = uri
        .query()
        .is_some_and(|query| query.split('&').any(|pair| pair == "external_logos=true"));
    if !request_is_authorized(&state, &headers) || !includes_external_logos {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(Json(
        state
            .response
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone(),
    ))
}

fn request_is_authorized(state: &AppsServerState, headers: &HeaderMap) -> bool {
    headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == state.expected_bearer)
        && headers
            .get("chatgpt-account-id")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value == state.expected_account_id)
}

pub(super) fn connector_tool(connector_id: &str, connector_name: &str) -> Result<Tool> {
    let schema: JsonObject = serde_json::from_value(json!({
        "type": "object",
        "additionalProperties": false,
    }))?;
    let mut tool = Tool::new(
        Cow::Owned(format!("connector_{connector_id}")),
        Cow::Borrowed("Connector test tool"),
        Arc::new(schema),
    );
    tool.annotations = Some(ToolAnnotations::new().read_only(true));
    let mut meta = MetaObject::new();
    meta.0
        .insert("connector_id".to_string(), json!(connector_id));
    meta.0
        .insert("connector_name".to_string(), json!(connector_name));
    tool.meta = Some(meta);
    Ok(tool)
}

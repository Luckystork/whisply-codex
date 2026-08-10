use std::time::Duration;

use anyhow::Result;
use app_test_support::ManagedWhisplyConfig;
use app_test_support::TestAppServer;
use app_test_support::write_models_cache;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::ModelListParams;
use codex_app_server_protocol::ModelListResponse;
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

#[tokio::test]
async fn list_models_returns_empty_when_managed_catalog_is_unavailable() -> Result<()> {
    let codex_home = TempDir::new()?;
    ManagedWhisplyConfig::new().write(codex_home.path())?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized()
        .await?;

    let ModelListResponse {
        data: items,
        next_cursor,
    } = mcp
        .request(|request_id| ClientRequest::ModelList {
            request_id,
            params: ModelListParams {
                limit: Some(100),
                cursor: None,
                include_hidden: None,
            },
        })
        .await?;

    assert!(items.is_empty());
    assert!(next_cursor.is_none());
    Ok(())
}

#[tokio::test]
async fn list_models_ignores_stale_generic_cache_without_managed_catalog() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_models_cache(codex_home.path())?;
    ManagedWhisplyConfig::new().write(codex_home.path())?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized()
        .await?;

    let ModelListResponse {
        data: items,
        next_cursor,
    } = mcp
        .request(|request_id| ClientRequest::ModelList {
            request_id,
            params: ModelListParams {
                limit: Some(100),
                cursor: None,
                include_hidden: Some(true),
            },
        })
        .await?;

    assert!(items.is_empty());
    assert!(next_cursor.is_none());
    Ok(())
}

#[tokio::test]
async fn list_models_unavailable_catalog_returns_empty_page_for_any_cursor() -> Result<()> {
    let codex_home = TempDir::new()?;
    ManagedWhisplyConfig::new().write(codex_home.path())?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized()
        .await?;

    let request_id = mcp
        .send_list_models_request(ModelListParams {
            limit: Some(1),
            cursor: Some("invalid".to_string()),
            include_hidden: None,
        })
        .await?;
    let ModelListResponse {
        data: items,
        next_cursor,
    } = timeout(DEFAULT_TIMEOUT, mcp.read_response(request_id)).await??;

    assert!(items.is_empty());
    assert!(next_cursor.is_none());
    Ok(())
}

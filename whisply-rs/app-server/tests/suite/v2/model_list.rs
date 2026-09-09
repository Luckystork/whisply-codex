use std::time::Duration;

use anyhow::Result;
use app_test_support::ManagedWhisplyConfig;
#[cfg(target_os = "macos")]
use app_test_support::ManagedWhisplyGatewayFixture;
use app_test_support::TestAppServer;
use app_test_support::write_models_cache;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::ModelListParams;
use codex_app_server_protocol::ModelListResponse;
#[cfg(target_os = "macos")]
use codex_app_server_protocol::WhisplyModelCatalogReadParams;
#[cfg(target_os = "macos")]
use codex_app_server_protocol::WhisplyModelCatalogReadResponse;
#[cfg(target_os = "macos")]
use core_test_support::responses;
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

#[cfg(target_os = "macos")]
#[tokio::test]
async fn whisply_model_catalog_read_returns_only_signed_product_metadata() -> Result<()> {
    let server = responses::start_mock_server().await;

    let codex_home = TempDir::new()?;
    ManagedWhisplyConfig::new().write(codex_home.path())?;
    let gateway = ManagedWhisplyGatewayFixture::new(&server.uri())?;
    let mut app_server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .with_managed_whisply_gateway(gateway)
        .build_initialized()
        .await?;

    let response: WhisplyModelCatalogReadResponse = app_server
        .request(|request_id| ClientRequest::WhisplyModelCatalogRead {
            request_id,
            params: WhisplyModelCatalogReadParams {},
        })
        .await?;

    assert_eq!(response.schema_version, 2);
    assert_eq!(response.models.len(), 11);
    let default = response
        .models
        .iter()
        .find(|model| model.recommended_default)
        .expect("signed default model");
    assert_eq!(default.id, "gpt-5.6-terra");
    assert_eq!(default.short_name, "GPT Terra");
    assert_eq!(default.response_start_timeout_seconds, 120);
    assert!(response.models.iter().all(|model| {
        !model.id.contains('/')
            && model.capabilities.supports_computer_use
            && model.capabilities.context_limit > 0
    }));
    let catalog_reads = server
        .received_requests()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|request| {
            request.method.as_str() == "GET" && request.url.path() == "/v1/model-catalog"
        })
        .collect::<Vec<_>>();
    assert_eq!(
        catalog_reads.len(),
        2,
        "the manager refreshes the signed catalog once at startup and the typed RPC reads it once"
    );
    assert!(catalog_reads.iter().all(|request| {
        request.headers.contains_key("authorization")
            && request
                .headers
                .get("x-whisply-install-id")
                .and_then(|value| value.to_str().ok())
                == Some("70000000-0000-4000-8000-000000000001")
    }));
    app_server.assert_managed_whisply_gateway_healthy()?;
    Ok(())
}

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

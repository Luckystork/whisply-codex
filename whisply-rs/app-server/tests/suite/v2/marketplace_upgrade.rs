use std::time::Duration;

use anyhow::Result;
use app_test_support::TestAppServer;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::MarketplaceUpgradeParams;
use codex_app_server_protocol::MarketplaceUpgradeResponse;
use codex_app_server_protocol::RequestId;
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use tokio::time::timeout;
use whisply_config::MarketplaceConfigUpdate;
use whisply_config::record_user_marketplace;

#[cfg(windows)]
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(25);
#[cfg(not(windows))]
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

fn configured_local_marketplace_update(source: &str) -> MarketplaceConfigUpdate<'_> {
    MarketplaceConfigUpdate {
        last_updated: "2026-04-13T00:00:00Z",
        last_revision: None,
        source_type: "local",
        source,
        ref_name: None,
        sparse_paths: &[],
    }
}

async fn send_marketplace_upgrade(
    mcp: &mut TestAppServer,
    marketplace_name: Option<&str>,
) -> Result<MarketplaceUpgradeResponse> {
    mcp.request(|request_id| ClientRequest::MarketplaceUpgrade {
        request_id,
        params: MarketplaceUpgradeParams {
            marketplace_name: marketplace_name.map(str::to_string),
        },
    })
    .await
}

#[tokio::test]
async fn marketplace_upgrade_keeps_local_only_state_as_a_noop() -> Result<()> {
    let codex_home = TempDir::new()?;
    let local_source = TempDir::new()?;
    record_user_marketplace(
        codex_home.path(),
        "local-only",
        &configured_local_marketplace_update(&local_source.path().display().to_string()),
    )?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized()
        .await?;

    assert_eq!(
        send_marketplace_upgrade(&mut mcp, None).await?,
        MarketplaceUpgradeResponse {
            selected_marketplaces: Vec::new(),
            upgraded_roots: Vec::new(),
            errors: Vec::new(),
        }
    );
    Ok(())
}

#[tokio::test]
async fn marketplace_upgrade_rejects_unknown_or_non_git_marketplace() -> Result<()> {
    let codex_home = TempDir::new()?;
    let local_source = TempDir::new()?;
    record_user_marketplace(
        codex_home.path(),
        "local-only",
        &configured_local_marketplace_update(&local_source.path().display().to_string()),
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized()
        .await?;

    for marketplace_name in ["missing", "local-only"] {
        let request_id = mcp
            .send_marketplace_upgrade_request(MarketplaceUpgradeParams {
                marketplace_name: Some(marketplace_name.to_string()),
            })
            .await?;

        let err = timeout(
            DEFAULT_TIMEOUT,
            mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
        )
        .await??;

        assert_eq!(err.error.code, -32600);
        assert_eq!(
            err.error.message,
            format!("marketplace `{marketplace_name}` is not configured as a Git marketplace"),
        );
    }
    Ok(())
}

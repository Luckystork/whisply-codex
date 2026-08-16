use std::time::Duration;

use anyhow::Result;
use app_test_support::TestAppServer;
use codex_app_server_protocol::RemoteControlClientsListParams;
use codex_app_server_protocol::RemoteControlClientsRevokeParams;
use codex_app_server_protocol::RemoteControlPairingStartParams;
use codex_app_server_protocol::RemoteControlPairingStatusParams;
use codex_app_server_protocol::RequestId;
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use tokio::time::timeout;
use wiremock::MockServer;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
const WHISPLY_MANAGED_REMOTE_CONTROL_UNAVAILABLE_ERROR: &str =
    "Whisply remote control is managed by the installed app and is unavailable in this runtime.";

#[tokio::test]
async fn remote_control_rpcs_are_managed_unavailable_without_direct_backend_io() -> Result<()> {
    let codex_home = TempDir::new()?;
    let outbound_probe = MockServer::start().await;
    let proxy_uri = outbound_probe.uri();
    let mut app_server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .with_env_overrides(&[
            ("HTTP_PROXY", Some(proxy_uri.as_str())),
            ("http_proxy", Some(proxy_uri.as_str())),
            ("HTTPS_PROXY", Some(proxy_uri.as_str())),
            ("https_proxy", Some(proxy_uri.as_str())),
            ("ALL_PROXY", None),
            ("all_proxy", None),
            ("NO_PROXY", None),
            ("no_proxy", None),
        ])
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;

    let request_ids = [
        app_server.send_remote_control_enable_request().await?,
        app_server
            .send_remote_control_ephemeral_enable_request()
            .await?,
        app_server.send_remote_control_disable_request().await?,
        app_server
            .send_remote_control_ephemeral_disable_request()
            .await?,
        app_server.send_remote_control_status_read_request().await?,
        app_server
            .send_remote_control_pairing_start_request(RemoteControlPairingStartParams {
                manual_code: true,
            })
            .await?,
        app_server
            .send_remote_control_pairing_status_request(RemoteControlPairingStatusParams {
                pairing_code: Some("pairing-code".to_string()),
                manual_pairing_code: None,
            })
            .await?,
        app_server
            .send_remote_control_clients_list_request(RemoteControlClientsListParams {
                environment_id: "environment-id".to_string(),
                cursor: None,
                limit: None,
                order: None,
            })
            .await?,
        app_server
            .send_remote_control_clients_revoke_request(RemoteControlClientsRevokeParams {
                environment_id: "environment-id".to_string(),
                client_id: "client-id".to_string(),
            })
            .await?,
    ];

    for request_id in request_ids {
        let error = timeout(
            DEFAULT_TIMEOUT,
            app_server.read_stream_until_error_message(RequestId::Integer(request_id)),
        )
        .await??;
        assert_eq!(error.error.code, -32600);
        assert_eq!(
            error.error.message,
            WHISPLY_MANAGED_REMOTE_CONTROL_UNAVAILABLE_ERROR
        );
    }
    assert!(
        outbound_probe
            .received_requests()
            .await
            .expect("probe should record requests")
            .is_empty(),
        "managed-unavailable remote-control RPCs must reject before direct backend I/O"
    );
    Ok(())
}

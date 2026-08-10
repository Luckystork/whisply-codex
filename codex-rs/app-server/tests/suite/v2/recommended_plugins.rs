use std::time::Duration;

use anyhow::Result;
#[cfg(target_os = "macos")]
use app_test_support::ManagedWhisplyConfig;
#[cfg(target_os = "macos")]
use app_test_support::ManagedWhisplyGatewayFixture;
use app_test_support::TestAppServer;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::UserInput;
use codex_features::Feature;
use core_test_support::responses;
use tempfile::TempDir;
use tokio::time::timeout;
use wiremock::MockServer;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

#[cfg(target_os = "macos")]
#[tokio::test]
async fn persisted_auth_with_remote_plugins_does_not_query_legacy_backend() -> Result<()> {
    let responses_server = responses::start_mock_server().await;
    let outbound_probe = MockServer::start().await;
    let response = responses::sse(vec![
        responses::ev_response_created("resp-1"),
        responses::ev_assistant_message("msg-1", "done"),
        responses::ev_completed("resp-1"),
    ]);
    let responses_mock = responses::mount_sse_once(&responses_server, response).await;
    let codex_home = TempDir::new()?;
    ManagedWhisplyConfig::new()
        .enable_feature(Feature::Apps)
        .enable_feature(Feature::Plugins)
        .enable_feature(Feature::RecommendedPlugins)
        .enable_feature(Feature::RemotePlugin)
        .enable_feature(Feature::ToolSuggest)
        .write(codex_home.path())?;

    let proxy_uri = outbound_probe.uri();
    let managed_gateway = ManagedWhisplyGatewayFixture::new(&responses_server.uri())?;
    let mut app_server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .with_managed_whisply_gateway(managed_gateway)
        .with_env_overrides(&[
            ("HTTP_PROXY", Some(proxy_uri.as_str())),
            ("http_proxy", Some(proxy_uri.as_str())),
            ("HTTPS_PROXY", Some(proxy_uri.as_str())),
            ("https_proxy", Some(proxy_uri.as_str())),
            ("ALL_PROXY", None),
            ("all_proxy", None),
            ("NO_PROXY", Some("localhost,127.0.0.1")),
            ("no_proxy", Some("localhost,127.0.0.1")),
        ])
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await?;
    let thread_id = app_server
        .start_thread(ThreadStartParams::default())
        .await?
        .thread
        .id;
    let turn_id = app_server
        .send_turn_start_request(TurnStartParams {
            thread_id,
            input: vec![UserInput::Text {
                text: "suggest a local helper".to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    let _: TurnStartResponse =
        timeout(DEFAULT_TIMEOUT, app_server.read_response(turn_id)).await??;
    timeout(
        DEFAULT_TIMEOUT,
        app_server.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let request = responses_mock.single_request();
    assert!(
        request
            .message_input_texts("user")
            .into_iter()
            .all(|text| !text.contains("<recommended_plugins>"))
    );
    assert!(
        outbound_probe
            .received_requests()
            .await
            .expect("probe should record requests")
            .is_empty(),
        "persisted auth and RemotePlugin must not query the legacy plugin backend"
    );
    app_server.assert_managed_whisply_gateway_healthy()?;
    Ok(())
}

//! Managed-provider attestation coverage.

#![cfg(target_os = "macos")]

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use app_test_support::ManagedWhisplyConfig;
use app_test_support::ManagedWhisplyGatewayFixture;
use app_test_support::TestAppServer;
use app_test_support::create_final_assistant_message_sse_response;
use app_test_support::create_mock_responses_server_sequence_unchecked;
use app_test_support::to_response;
use codex_app_server_protocol::ClientInfo;
use codex_app_server_protocol::InitializeCapabilities;
use codex_app_server_protocol::JSONRPCMessage;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::UserInput as V2UserInput;
use tempfile::TempDir;
use tokio::time::Duration;
use tokio::time::timeout;

const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(60);

#[tokio::test]
async fn attestation_generate_is_not_forwarded_to_brokered_responses_request() -> Result<()> {
    let server = create_mock_responses_server_sequence_unchecked(vec![
        create_final_assistant_message_sse_response("Done")?,
    ])
    .await;

    let codex_home = TempDir::new()?;
    ManagedWhisplyConfig::new().write(codex_home.path())?;
    let managed_gateway = ManagedWhisplyGatewayFixture::new(&server.uri())?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .with_env_overrides(&[("OPENAI_API_KEY", None)])
        .with_managed_whisply_gateway(managed_gateway)
        .build()
        .await?;
    let initialized = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.initialize_with_capabilities(
            ClientInfo {
                name: "codex_desktop".to_string(),
                title: Some("Codex Desktop".to_string()),
                version: "0.1.0".to_string(),
            },
            Some(InitializeCapabilities {
                experimental_api: true,
                request_attestation: true,
                opt_out_notification_methods: None,
                mcp_server_openai_form_elicitation: false,
                extensions: None,
            }),
        ),
    )
    .await??;
    let JSONRPCMessage::Response(_) = initialized else {
        bail!("expected initialize response, got {initialized:?}");
    };

    let thread_request_id = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams::default())
        .await?;
    let thread_response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(thread_request_id)),
    )
    .await??;
    let ThreadStartResponse { thread, .. } = to_response(thread_response)?;

    let turn_request_id = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id,
            client_user_message_id: None,
            input: vec![V2UserInput::Text {
                text: "Hello".to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    let turn_response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(turn_request_id)),
    )
    .await??;
    let _: TurnStartResponse = to_response(turn_response)?;

    timeout(DEFAULT_READ_TIMEOUT, async {
        loop {
            match mcp.read_next_message().await? {
                JSONRPCMessage::Request(request) => bail!(
                    "managed brokered responses must not request client attestation: {request:?}"
                ),
                JSONRPCMessage::Notification(notification)
                    if notification.method == "turn/completed" =>
                {
                    break Ok(());
                }
                _ => {}
            }
        }
    })
    .await??;

    let requests = server
        .received_requests()
        .await
        .context("failed to fetch managed response requests")?;
    let response_request = requests
        .iter()
        .find(|request| request.url.path().ends_with("/responses"))
        .context("expected brokered /v1/responses request")?;
    assert!(
        response_request.headers.get("x-oai-attestation").is_none(),
        "the brokered direct Responses route must not receive a client attestation header"
    );
    mcp.assert_managed_whisply_gateway_healthy()?;

    Ok(())
}

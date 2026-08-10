use anyhow::Result;
use app_test_support::ManagedWhisplyConfig;
use app_test_support::TestAppServer;
use codex_app_server_protocol::JSONRPCError;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ThreadRealtimeErrorNotification;
use codex_app_server_protocol::ThreadRealtimeListVoicesParams;
use codex_app_server_protocol::ThreadRealtimeListVoicesResponse;
use codex_app_server_protocol::ThreadRealtimeStartParams;
use codex_app_server_protocol::ThreadRealtimeStartResponse;
use codex_app_server_protocol::ThreadRealtimeStartTransport;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_features::Feature;
use codex_protocol::protocol::RealtimeOutputModality;
use codex_protocol::protocol::RealtimeVoice;
use codex_protocol::protocol::RealtimeVoicesList;
use std::path::Path;
use std::time::Duration;
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
const WHISPLY_REALTIME_UNAVAILABLE_ERROR: &str =
    "Realtime conversations are unavailable in Whisply's broker-only runtime.";

// The upstream sideband/WebRTC success fixture is intentionally absent. It
// required a configured API-key provider plus direct websocket/call endpoints,
// neither of which is a valid authority in BrokerOnly Whisply.

#[tokio::test]
async fn managed_realtime_fixture_contains_no_direct_provider_or_endpoint() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_managed_realtime_config(codex_home.path(), /*realtime_enabled*/ true)?;

    let config = std::fs::read_to_string(codex_home.path().join("config.toml"))?;
    for forbidden in [
        "experimental_realtime_ws_base_url",
        "experimental_realtime_webrtc_call_base_url",
        "model_providers",
        "base_url",
        "api_key",
        "chatgpt",
    ] {
        assert!(
            !config.contains(forbidden),
            "managed realtime fixture must not contain `{forbidden}`"
        );
    }
    assert!(config.contains("model_provider = \"whisply\""));
    Ok(())
}

#[tokio::test]
async fn realtime_start_rejects_broker_only_whisply_without_direct_endpoint_config() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_managed_realtime_config(codex_home.path(), /*realtime_enabled*/ true)?;

    let mut mcp = managed_app_server(codex_home.path()).await?;
    let thread_id = start_thread(&mut mcp).await?;

    let error = start_realtime_and_read_error(&mut mcp, &thread_id, None).await?;
    assert_eq!(error.thread_id, thread_id);
    assert_eq!(error.message, WHISPLY_REALTIME_UNAVAILABLE_ERROR);
    Ok(())
}

#[tokio::test]
async fn realtime_webrtc_start_rejects_before_call_creation_or_sideband_join() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_managed_realtime_config(codex_home.path(), /*realtime_enabled*/ true)?;

    let mut mcp = managed_app_server(codex_home.path()).await?;
    let thread_id = start_thread(&mut mcp).await?;

    let error = start_realtime_and_read_error(
        &mut mcp,
        &thread_id,
        Some(ThreadRealtimeStartTransport::Webrtc {
            sdp: "v=offer\r\n".to_string(),
        }),
    )
    .await?;
    assert_eq!(error.thread_id, thread_id);
    assert_eq!(error.message, WHISPLY_REALTIME_UNAVAILABLE_ERROR);
    Ok(())
}

#[tokio::test]
async fn realtime_conversation_requires_feature_flag_before_authority_check() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_managed_realtime_config(codex_home.path(), /*realtime_enabled*/ false)?;

    let mut mcp = managed_app_server(codex_home.path()).await?;
    let thread_id = start_thread(&mut mcp).await?;
    let request_id = mcp
        .send_thread_realtime_start_request(realtime_start_params(thread_id.clone(), None))
        .await?;
    let error: JSONRPCError = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;

    assert_invalid_request(
        error,
        format!("thread {thread_id} does not support realtime conversation"),
    );
    Ok(())
}

#[tokio::test]
async fn realtime_list_voices_retains_the_static_protocol_catalog() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_managed_realtime_config(codex_home.path(), /*realtime_enabled*/ true)?;

    let mut mcp = managed_app_server(codex_home.path()).await?;
    let request_id = mcp
        .send_thread_realtime_list_voices_request(ThreadRealtimeListVoicesParams {})
        .await?;
    let response: ThreadRealtimeListVoicesResponse =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(request_id)).await??;

    assert_eq!(
        response,
        ThreadRealtimeListVoicesResponse {
            voices: RealtimeVoicesList {
                v1: vec![
                    RealtimeVoice::Juniper,
                    RealtimeVoice::Maple,
                    RealtimeVoice::Spruce,
                    RealtimeVoice::Ember,
                    RealtimeVoice::Vale,
                    RealtimeVoice::Breeze,
                    RealtimeVoice::Arbor,
                    RealtimeVoice::Sol,
                    RealtimeVoice::Cove,
                ],
                v2: vec![
                    RealtimeVoice::Alloy,
                    RealtimeVoice::Ash,
                    RealtimeVoice::Ballad,
                    RealtimeVoice::Coral,
                    RealtimeVoice::Echo,
                    RealtimeVoice::Sage,
                    RealtimeVoice::Shimmer,
                    RealtimeVoice::Verse,
                    RealtimeVoice::Marin,
                    RealtimeVoice::Cedar,
                ],
                default_v1: RealtimeVoice::Cove,
                default_v2: RealtimeVoice::Marin,
            },
        }
    );
    Ok(())
}

fn write_managed_realtime_config(codex_home: &Path, realtime_enabled: bool) -> std::io::Result<()> {
    let config = if realtime_enabled {
        ManagedWhisplyConfig::new().enable_feature(Feature::RealtimeConversation)
    } else {
        ManagedWhisplyConfig::new().disable_feature(Feature::RealtimeConversation)
    };
    config.write(codex_home)
}

async fn managed_app_server(codex_home: &Path) -> Result<TestAppServer> {
    TestAppServer::builder()
        .with_codex_home(codex_home)
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await
}

async fn start_thread(mcp: &mut TestAppServer) -> Result<String> {
    let request_id = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams::default())
        .await?;
    let response: ThreadStartResponse =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(request_id)).await??;
    Ok(response.thread.id)
}

async fn start_realtime_and_read_error(
    mcp: &mut TestAppServer,
    thread_id: &str,
    transport: Option<ThreadRealtimeStartTransport>,
) -> Result<ThreadRealtimeErrorNotification> {
    let request_id = mcp
        .send_thread_realtime_start_request(realtime_start_params(thread_id.to_string(), transport))
        .await?;
    let _: ThreadRealtimeStartResponse =
        timeout(DEFAULT_TIMEOUT, mcp.read_response(request_id)).await??;
    timeout(
        DEFAULT_TIMEOUT,
        mcp.read_notification("thread/realtime/error"),
    )
    .await?
}

fn realtime_start_params(
    thread_id: String,
    transport: Option<ThreadRealtimeStartTransport>,
) -> ThreadRealtimeStartParams {
    ThreadRealtimeStartParams {
        thread_id,
        client_managed_handoffs: None,
        delegation_ack_filler: None,
        flush_transcript_tail_on_session_end: None,
        codex_responses_as_items: None,
        codex_response_item_prefix: None,
        codex_response_handoff_mode: None,
        codex_response_handoff_channel_prefixes: None,
        model: None,
        output_modality: RealtimeOutputModality::Audio,
        include_startup_context: None,
        initial_items: None,
        realtime_start_instructions: None,
        realtime_end_instructions: None,
        prompt: None,
        realtime_session_id: None,
        transport,
        version: None,
        voice: None,
    }
}

fn assert_invalid_request(error: JSONRPCError, message: String) {
    assert_eq!(error.error.code, -32600);
    assert_eq!(error.error.message, message);
    assert_eq!(error.error.data, None);
}

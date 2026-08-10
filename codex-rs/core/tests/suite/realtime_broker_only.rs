use anyhow::Result;
use codex_protocol::protocol::CodexErrorInfo;
use codex_protocol::protocol::CodexResponseHandoffMode;
use codex_protocol::protocol::ConversationAudioParams;
use codex_protocol::protocol::ConversationStartParams;
use codex_protocol::protocol::ConversationStartTransport;
use codex_protocol::protocol::ConversationTextParams;
use codex_protocol::protocol::ConversationTextRole;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::RealtimeAudioFrame;
use codex_protocol::protocol::RealtimeConversationRealtimeEvent;
use codex_protocol::protocol::RealtimeEvent;
use codex_protocol::protocol::RealtimeOutputModality;
use core_test_support::responses::start_mock_server;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event_match;
use pretty_assertions::assert_eq;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn broker_only_start_rejects_direct_transports_without_network() -> Result<()> {
    let server = start_mock_server().await;
    let mut builder = test_codex();
    let test = builder.build_with_auto_env(&server).await?;
    let requests_before_start = server
        .received_requests()
        .await
        .expect("mock server should retain requests")
        .len();

    for (transport_name, transport) in [
        ("default websocket", None),
        (
            "explicit websocket",
            Some(ConversationStartTransport::Websocket),
        ),
        (
            "webrtc",
            Some(ConversationStartTransport::Webrtc {
                sdp: "v=offer\r\n".to_string(),
            }),
        ),
    ] {
        test.codex
            .submit(Op::RealtimeConversationStart(realtime_start_params(
                transport,
            )))
            .await?;

        let error = wait_for_event_match(&test.codex, |message| match message {
            EventMsg::RealtimeConversationRealtime(RealtimeConversationRealtimeEvent {
                payload: RealtimeEvent::Error(error),
            }) => Some(error.clone()),
            EventMsg::RealtimeConversationStarted(_) => {
                panic!("{transport_name} realtime start must not emit started")
            }
            _ => None,
        })
        .await;
        assert_eq!(
            error,
            "Realtime conversations are unavailable in Whisply's broker-only runtime."
        );
    }

    assert_eq!(
        server
            .received_requests()
            .await
            .expect("mock server should retain requests")
            .len(),
        requests_before_start,
        "rejected realtime starts must not contact a direct provider"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pre_start_realtime_input_stays_rejected_without_network() -> Result<()> {
    let server = start_mock_server().await;
    let mut builder = test_codex();
    let test = builder.build_with_auto_env(&server).await?;
    let requests_before_input = server
        .received_requests()
        .await
        .expect("mock server should retain requests")
        .len();

    for input in [
        Op::RealtimeConversationAudio(ConversationAudioParams {
            frame: RealtimeAudioFrame {
                data: "AQID".to_string(),
                sample_rate: 24_000,
                num_channels: 1,
                samples_per_channel: Some(480),
                item_id: None,
            },
        }),
        Op::RealtimeConversationText(ConversationTextParams {
            text: "hello".to_string(),
            role: ConversationTextRole::User,
        }),
    ] {
        test.codex.submit(input).await?;
        let error = wait_for_event_match(&test.codex, |message| match message {
            EventMsg::Error(error) => Some(error.clone()),
            _ => None,
        })
        .await;
        assert_eq!(error.codex_error_info, Some(CodexErrorInfo::BadRequest));
        assert_eq!(error.message, "conversation is not running");
    }

    assert_eq!(
        server
            .received_requests()
            .await
            .expect("mock server should retain requests")
            .len(),
        requests_before_input,
        "pre-start realtime input must not contact a provider"
    );
    Ok(())
}

fn realtime_start_params(transport: Option<ConversationStartTransport>) -> ConversationStartParams {
    ConversationStartParams {
        client_managed_handoffs: false,
        delegation_ack_filler: None,
        flush_transcript_tail_on_session_end: false,
        codex_responses_as_items: false,
        codex_response_item_prefix: None,
        codex_response_handoff_mode: CodexResponseHandoffMode::Thinking,
        codex_response_handoff_channel_prefixes: None,
        model: None,
        output_modality: RealtimeOutputModality::Audio,
        include_startup_context: true,
        initial_items: Vec::new(),
        realtime_start_instructions: None,
        realtime_end_instructions: None,
        prompt: None,
        realtime_session_id: None,
        transport,
        version: None,
        voice: None,
    }
}

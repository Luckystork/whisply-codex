//! What happens when shortening a conversation does not shorten it.
//!
//! The turn loop answers a full context window by compacting and then looking
//! at the window again. That terminates only if compaction works. When it does
//! not -- an endpoint having a bad day, a model that restates the conversation
//! instead of summarizing it -- the loop is asking the same question forever,
//! and every lap is a request the person pays for. These tests hold the product
//! to a bounded number of attempts and an honest ending.

use core_test_support::responses::ResponseMock;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed_with_tokens;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use whisply_core::compact::SUMMARIZATION_PROMPT;
use whisply_core::config::Config;
use whisply_model_provider_info::ModelProviderInfo;
use whisply_model_provider_info::built_in_model_providers;
use whisply_protocol::protocol::EventMsg;
use whisply_protocol::protocol::Op;
use whisply_protocol::user_input::UserInput;
use wiremock::MockServer;

/// Small enough that any real conversation is over it, so every turn after the
/// first asks for a shortening.
const AUTO_COMPACT_LIMIT: i64 = 1_000;
const REPORTED_TOKENS: i64 = 40_000;
/// Above what a short summary and the session's own preamble come to, so a
/// summary that summarizes gets the conversation back under it.
const PRODUCTIVE_AUTO_COMPACT_LIMIT: i64 = 20_000;
/// The bound in `session::turn`. Named here rather than imported because it is
/// private to the crate, and because a test that reads the number from the code
/// it is testing would agree with any change to it.
const MAX_UNPRODUCTIVE_AUTO_COMPACTIONS: usize = 2;
/// One turn that fits, then one request to shorten and one retry for each
/// attempt the bound allows.
const EXPECTED_REQUESTS: usize = 1 + 2 * MAX_UNPRODUCTIVE_AUTO_COMPACTIONS;

/// A "summary" that summarizes nothing: longer than the limit it was supposed
/// to get the conversation under. This is what an unproductive compaction looks
/// like from the runtime's side, whichever way the endpoint arrived at it.
fn useless_summary() -> String {
    "The person and the assistant discussed a great many things. ".repeat(400)
}

fn unproductive_compaction_config(server: &MockServer) -> impl Fn(&mut Config) + Clone + use<> {
    let provider = non_openai_model_provider(server);
    move |config: &mut Config| {
        config.model_provider = provider.clone();
        config.compact_prompt = Some(SUMMARIZATION_PROMPT.to_string());
        config.model_auto_compact_token_limit = Some(AUTO_COMPACT_LIMIT);
    }
}

/// Same shape, with room for a real summary to land under the limit.
fn productive_compaction_config(server: &MockServer) -> impl Fn(&mut Config) + Clone + use<> {
    let provider = non_openai_model_provider(server);
    move |config: &mut Config| {
        config.model_provider = provider.clone();
        config.compact_prompt = Some(SUMMARIZATION_PROMPT.to_string());
        config.model_auto_compact_token_limit = Some(PRODUCTIVE_AUTO_COMPACT_LIMIT);
    }
}

fn non_openai_model_provider(server: &MockServer) -> ModelProviderInfo {
    let mut provider = built_in_model_providers(None)["openai"].clone();
    provider.name = "OpenAI (test)".into();
    provider.base_url = Some(format!("{}/v1", server.uri()));
    provider.supports_websockets = false;
    provider
}

/// Sends a turn and reports the error it ended with, if any.
///
/// A turn that fails still finishes: the error arrives first and the turn's
/// closing event after it. Both have to be taken off the queue here, or the
/// next turn reads the last one's ending as its own.
async fn send_text(codex: &whisply_core::CodexThread, text: &str) -> Option<String> {
    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: text.into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
            additional_context: Default::default(),
            thread_settings: Default::default(),
        })
        .await
        .expect("submit the turn");
    let mut error = None;
    loop {
        let event = wait_for_event(codex, |event| {
            matches!(
                event,
                EventMsg::TurnComplete(_) | EventMsg::TurnAborted(_) | EventMsg::Error(_)
            )
        })
        .await;
        match event {
            EventMsg::Error(reported) => error = Some(reported.message),
            _ => return error,
        }
    }
}

fn compaction_requests(requests: &ResponseMock) -> usize {
    requests
        .requests()
        .iter()
        .filter(|request| request.body_contains_text(SUMMARIZATION_PROMPT))
        .count()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_shortening_that_does_not_shorten_is_not_tried_forever() {
    skip_if_no_network!();
    let server = start_mock_server().await;
    // Every reply is the same shape: something that leaves the conversation
    // over the limit. A turn answer and a compaction summary are both requests
    // to this endpoint, so one body serves for either. The mock insists on all
    // of them being used and has none to spare: one first
    // turn, then two turns that each buy a compaction and a retry. Anything
    // past that is the loop this test exists to rule out, and it fails here
    // for want of a reply.
    let requests = mount_sse_sequence(
        &server,
        (0..EXPECTED_REQUESTS)
            .map(|index| {
                sse(vec![
                    ev_assistant_message(&format!("m{index}"), &useless_summary()),
                    ev_completed_with_tokens(&format!("r{index}"), REPORTED_TOKENS),
                ])
            })
            .collect(),
    )
    .await;

    let mut builder = test_codex().with_config(unproductive_compaction_config(&server));
    let test = builder.build(&server).await.unwrap();

    let mut refusal = None;
    for turn in 0..=MAX_UNPRODUCTIVE_AUTO_COMPACTIONS + 1 {
        if let Some(message) = send_text(&test.codex, &format!("turn {turn}")).await {
            refusal = Some(message);
            break;
        }
    }

    let refusal = refusal.expect(
        "a conversation that cannot be shortened has to end the turn; without a bound the \
         loop keeps buying the same compaction",
    );
    assert!(
        refusal.contains("ran out of room"),
        "the ending has to say the room ran out and what to do about it: {refusal}"
    );
    assert_eq!(
        compaction_requests(&requests),
        MAX_UNPRODUCTIVE_AUTO_COMPACTIONS,
        "one attempt may be bad luck and is retried once; a third would cost \
         the person a request that ends where it began"
    );

    assert_eq!(
        requests.requests().len(),
        EXPECTED_REQUESTS,
        "the refused turn does not reach the model at all"
    );

    assert!(
        send_text(&test.codex, "and again").await.is_some(),
        "the next turn is refused for the same reason, not quietly retried"
    );
    assert_eq!(
        requests.requests().len(),
        EXPECTED_REQUESTS,
        "and it does not reach the model either"
    );
}

/// The bound is on shortenings that achieve nothing, so an ordinary long
/// conversation -- which compacts over and over, productively -- must never
/// reach it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_conversation_that_shortens_normally_is_never_refused() {
    skip_if_no_network!();
    let server = start_mock_server().await;
    let turns = MAX_UNPRODUCTIVE_AUTO_COMPACTIONS + 3;
    let expected_requests = 1 + 2 * (turns - 1);
    // A short answer, and a reported usage over the limit, so the next turn
    // asks for a shortening. Installing the short summary re-measures the
    // conversation and finds it small, which is what productive looks like.
    let requests = mount_sse_sequence(
        &server,
        (0..expected_requests)
            .map(|index| {
                sse(vec![
                    ev_assistant_message(&format!("m{index}"), "A short answer."),
                    ev_completed_with_tokens(&format!("r{index}"), REPORTED_TOKENS),
                ])
            })
            .collect(),
    )
    .await;

    let mut builder = test_codex().with_config(productive_compaction_config(&server));
    let test = builder.build(&server).await.unwrap();

    for turn in 0..turns {
        assert_eq!(
            send_text(&test.codex, &format!("turn {turn}")).await,
            None,
            "turn {turn} was refused, and this conversation shortens fine"
        );
    }

    assert!(
        compaction_requests(&requests) > MAX_UNPRODUCTIVE_AUTO_COMPACTIONS,
        "the test only rules the bound out if more shortenings ran than the \
         bound allows unproductive ones"
    );
}

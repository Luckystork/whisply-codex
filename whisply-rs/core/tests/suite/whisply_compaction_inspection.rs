//! What a person can find out about their long conversation being shortened.
//!
//! Compaction is the one thing the product does to someone's conversation
//! without being asked. These tests hold the account of it to the same standard
//! as the act: the budget reported has to be the budget the runtime used, the
//! digest has to identify the summary that was actually written down, and none
//! of what was said may travel with any of it.

use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed_with_tokens;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use std::path::Path;
use whisply_core::compact::SUMMARIZATION_PROMPT;
use whisply_core::config::Config;
use whisply_model_provider_info::ModelProviderInfo;
use whisply_model_provider_info::built_in_model_providers;
use whisply_protocol::protocol::EventMsg;
use whisply_protocol::protocol::Op;
use whisply_protocol::protocol::RolloutItem;
use whisply_protocol::protocol::RolloutLine;
use whisply_protocol::user_input::UserInput;
use wiremock::MockServer;

const FIRST_REPLY: &str = "The first answer.";
const SUMMARY_TEXT: &str = "Earlier the person asked about their account balance.";
const AUTO_COMPACT_LIMIT: i64 = 200_000;

/// Keeps the summarization turn on the local path this inspection reports on.
fn local_compaction_config(server: &MockServer) -> impl Fn(&mut Config) + Clone + use<> {
    let provider = non_openai_model_provider(server);
    move |config: &mut Config| {
        config.model_provider = provider.clone();
        config.compact_prompt = Some(SUMMARIZATION_PROMPT.to_string());
        config.model_auto_compact_token_limit = Some(AUTO_COMPACT_LIMIT);
    }
}

fn non_openai_model_provider(server: &MockServer) -> ModelProviderInfo {
    let mut provider = built_in_model_providers(None)["openai"].clone();
    provider.name = "OpenAI (test)".into();
    provider.base_url = Some(format!("{}/v1", server.uri()));
    provider.supports_websockets = false;
    provider
}

async fn send_text(codex: &whisply_core::CodexThread, text: &str) {
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
    wait_for_event(codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;
}

/// The summary that compaction actually wrote into the thread's stored record.
fn persisted_summary(path: &Path) -> Option<String> {
    let rollout = std::fs::read_to_string(path).expect("read the rollout");
    rollout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter_map(|line| serde_json::from_str::<RolloutLine>(line).ok())
        .find_map(|entry| match entry.item {
            RolloutItem::Compacted(compacted) => Some(compacted.message),
            _ => None,
        })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_conversation_that_has_not_been_shortened_says_how_much_room_is_left() {
    skip_if_no_network!();
    let server = start_mock_server().await;
    mount_sse_sequence(
        &server,
        vec![sse(vec![
            ev_assistant_message("m1", FIRST_REPLY),
            ev_completed_with_tokens("r1", 2_400),
        ])],
    )
    .await;

    let mut builder = test_codex().with_config(local_compaction_config(&server));
    let test = builder.build(&server).await.unwrap();

    let before = test.codex.compaction_status();
    assert!(
        before.budget.is_none(),
        "nothing has measured the window before the first turn, and a zeroed \
         budget would read as a conversation with all its room left"
    );

    send_text(&test.codex, "hello world").await;

    let status = test.codex.compaction_status();
    assert_eq!(status.compaction_count, 0);
    assert!(status.records.is_empty());
    let budget = status.budget.expect("a turn has measured the window");
    assert!(
        !budget.threshold_reached,
        "one short turn is nowhere near the limit"
    );
    assert!(
        budget.active_context_tokens > 0,
        "the conversation is not empty once a turn has run"
    );
    assert!(
        budget
            .tokens_remaining
            .is_some_and(|remaining| remaining > 0),
        "there is room left, and how much is the thing someone is asking"
    );
    assert!(
        budget.used_basis_points().is_some_and(|used| used < 10_000),
        "a short conversation is not reported as full"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_shortening_is_accounted_for_by_what_was_written_down() {
    skip_if_no_network!();
    let server = start_mock_server().await;
    mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_assistant_message("m1", FIRST_REPLY),
                ev_completed_with_tokens("r1", 2_400),
            ]),
            sse(vec![
                ev_assistant_message("m2", SUMMARY_TEXT),
                ev_completed_with_tokens("r2", 2_600),
            ]),
        ],
    )
    .await;

    let mut builder = test_codex().with_config(local_compaction_config(&server));
    let test = builder.build(&server).await.unwrap();
    let rollout_path = test
        .session_configured
        .rollout_path
        .clone()
        .expect("rollout path");

    send_text(&test.codex, "hello world").await;
    test.codex.submit(Op::Compact).await.unwrap();
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let status = test.codex.compaction_status();
    assert_eq!(
        status.compaction_count, 1,
        "the conversation was shortened once"
    );
    let record = status.records.last().expect("one shortening");
    assert_eq!(
        record.trigger,
        codex_whisply::CompactionTriggerKind::Requested,
        "this one was asked for, and a person who asked is owed a different \
         account than one whose chat was shortened out from under them"
    );
    assert_eq!(
        record.mechanism,
        codex_whisply::CompactionMechanism::Summarized
    );
    assert_eq!(record.outcome, codex_whisply::CompactionOutcome::Completed);
    assert!(
        record.persisted,
        "a shortening that was not written down is read back long on the next \
         resume"
    );
    assert!(
        record.active_context_tokens_before > 0,
        "the window held the earlier conversation"
    );

    test.codex.submit(Op::Shutdown).await.unwrap();
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::ShutdownComplete)).await;

    let written = persisted_summary(&rollout_path).expect("a stored compaction entry");
    assert!(
        written.contains(SUMMARY_TEXT),
        "the test is only meaningful if the summary really was stored"
    );
    assert_eq!(
        record.summary_digest.as_deref(),
        Some(codex_whisply::summary_digest(&written).as_str()),
        "the digest has to identify the summary that was written down, or it \
         cannot be used to tell whether a resumed thread carries it"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn asking_about_a_conversation_is_not_a_way_to_read_it() {
    skip_if_no_network!();
    let server = start_mock_server().await;
    mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_assistant_message("m1", "The balance on the account ending 4417."),
                ev_completed_with_tokens("r1", 2_400),
            ]),
            sse(vec![
                ev_assistant_message("m2", SUMMARY_TEXT),
                ev_completed_with_tokens("r2", 2_600),
            ]),
        ],
    )
    .await;

    let mut builder = test_codex().with_config(local_compaction_config(&server));
    let test = builder.build(&server).await.unwrap();

    send_text(&test.codex, "what is my balance on the card ending 4417").await;
    test.codex.submit(Op::Compact).await.unwrap();
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let reported =
        serde_json::to_string(&test.codex.compaction_status()).expect("serialize the account");

    assert!(
        !reported.contains("4417")
            && !reported.contains("balance")
            && !reported.contains(SUMMARY_TEXT),
        "the record carries counts and digests; if what was said travels with \
         it then inspecting a conversation becomes a way to read one: \
         {reported}"
    );
}

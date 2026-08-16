#![cfg(not(target_os = "windows"))]
#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]

//! What a long, messy conversation looks like after it has been shortened twice.
//!
//! The other compaction suites use short text turns. A real conversation is not
//! like that: it holds screenshots, pasted files, and tool output measured in
//! tens of thousands of lines. These tests drive two compactions over exactly
//! that kind of history and check what is still being sent to the model
//! afterwards.

use anyhow::Result;
use core_test_support::responses;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use serde_json::Value;
use whisply_model_provider_info::ModelProviderInfo;
use whisply_model_provider_info::built_in_model_providers;
use whisply_prompts::SUMMARY_PREFIX;
use whisply_protocol::protocol::EventMsg;
use whisply_protocol::protocol::Op;
use whisply_protocol::user_input::UserInput;
use wiremock::MockServer;

/// A one-pixel PNG. Small on disk, but it is still an image input item, which is
/// what the history has to stop carrying.
const PIXEL: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==";

const FIRST_SUMMARY: &str = "FIRST_HANDOFF_SUMMARY";
const SECOND_SUMMARY: &str = "SECOND_HANDOFF_SUMMARY";

/// Keeps compaction local by pointing at a provider that is not the real
/// OpenAI endpoint, matching the other local compaction tests.
fn local_model_provider(server: &MockServer) -> ModelProviderInfo {
    let mut provider = built_in_model_providers(None)["openai"].clone();
    provider.name = "OpenAI (test)".into();
    provider.base_url = Some(format!("{}/v1", server.uri()));
    provider.supports_websockets = false;
    provider
}

fn big_shell_call(call_id: &str, upper: u32) -> Result<Value> {
    let arguments = serde_json::json!({
        "command": format!("seq 1 {upper}"),
        "timeout_ms": 20_000,
    });
    Ok(ev_function_call(
        call_id,
        "shell_command",
        &serde_json::to_string(&arguments)?,
    ))
}

async fn submit_text(codex: &whisply_core::CodexConversation, text: &str) {
    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: text.to_string(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
            additional_context: Default::default(),
            thread_settings: Default::default(),
        })
        .await
        .expect("submit user input");
    wait_for_event(codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_compactions_over_tool_output_and_an_image_leave_only_words() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;

    let first_tool_turn = sse(vec![
        responses::ev_response_created("resp-1"),
        big_shell_call("call-1", 20_000)?,
        ev_completed("resp-1"),
    ]);
    let first_answer = sse(vec![
        ev_assistant_message("m1", "ran the first command"),
        ev_completed("resp-2"),
    ]);
    let first_compaction = sse(vec![
        ev_assistant_message("m2", FIRST_SUMMARY),
        ev_completed("resp-3"),
    ]);
    let second_tool_turn = sse(vec![
        responses::ev_response_created("resp-4"),
        big_shell_call("call-2", 20_000)?,
        ev_completed("resp-4"),
    ]);
    let second_answer = sse(vec![
        ev_assistant_message("m3", "ran the second command"),
        ev_completed("resp-5"),
    ]);
    let second_compaction = sse(vec![
        ev_assistant_message("m4", SECOND_SUMMARY),
        ev_completed("resp-6"),
    ]);
    let final_answer = sse(vec![
        ev_assistant_message("m5", "still here"),
        ev_completed("resp-7"),
    ]);

    let request_log = mount_sse_sequence(
        &server,
        vec![
            first_tool_turn,
            first_answer,
            first_compaction,
            second_tool_turn,
            second_answer,
            second_compaction,
            final_answer,
        ],
    )
    .await;

    let model_provider = local_model_provider(&server);
    let codex = test_codex()
        .with_model("gpt-5.2")
        .with_config(move |config| {
            config.model_provider = model_provider;
            config.compact_prompt = Some(whisply_prompts::SUMMARIZATION_PROMPT.to_string());
            config.tool_output_token_limit = Some(100_000);
        })
        .build(&server)
        .await?
        .codex;

    codex
        .submit(Op::UserInput {
            items: vec![
                UserInput::Image {
                    image_url: PIXEL.to_string(),
                    detail: None,
                },
                UserInput::Text {
                    text: "USER_ONE".to_string(),
                    text_elements: Vec::new(),
                },
            ],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
            additional_context: Default::default(),
            thread_settings: Default::default(),
        })
        .await
        .expect("submit first user input");
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    codex.submit(Op::Compact).await?;
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    submit_text(&codex, "USER_TWO").await;

    codex.submit(Op::Compact).await?;
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    submit_text(&codex, "USER_THREE").await;

    let requests = request_log.requests();
    assert_eq!(
        requests.len(),
        7,
        "expected tool turn, answer, compaction, tool turn, answer, compaction, final turn"
    );

    let first_compaction_request = &requests[2];
    assert!(
        !first_compaction_request
            .inputs_of_type("function_call_output")
            .is_empty(),
        "the first compaction should summarize the real history, tool output included"
    );

    let after = &requests[6];

    assert!(
        after.inputs_of_type("function_call").is_empty()
            && after.inputs_of_type("function_call_output").is_empty(),
        "twenty thousand lines of tool output are still being resent after two compactions"
    );
    assert!(
        after.message_input_image_urls("user").is_empty(),
        "the screenshot from the first turn is still being resent after two compactions"
    );
    assert!(
        !after.body_contains_text("19999"),
        "the body of the tool output survived compaction in some other shape"
    );

    let user_texts = after.message_input_texts("user");
    for expected in ["USER_ONE", "USER_TWO", "USER_THREE"] {
        assert!(
            user_texts.iter().any(|text| text == expected),
            "{expected} was dropped; the person's own words are what compaction is meant to keep"
        );
    }

    let summaries = user_texts
        .iter()
        .filter(|text| text.contains(SUMMARY_PREFIX))
        .count();
    assert_eq!(
        summaries, 1,
        "summaries are stacking: each compaction should replace the previous one, not add to it"
    );
    assert!(
        user_texts.iter().any(|text| text.contains(SECOND_SUMMARY)),
        "the surviving summary should be the most recent one"
    );
    assert!(
        !user_texts.iter().any(|text| text.contains(FIRST_SUMMARY)),
        "the older summary is still being carried alongside the newer one"
    );

    Ok(())
}

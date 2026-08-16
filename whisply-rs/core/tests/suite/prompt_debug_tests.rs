use std::sync::Arc;

use anyhow::Result;
use core_test_support::responses::strip_metadata;
use core_test_support::responses::strip_response_item_id;
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use whisply_core::build_prompt_composition_diagnostics;
use whisply_core::build_prompt_input;
use whisply_core::config::ConfigBuilder;
use whisply_core::config::ConfigOverrides;
use whisply_extension_api::ExtensionRegistryBuilder;
use whisply_home::CodexHomeUserInstructionsProvider;
use whisply_protocol::models::ContentItem;
use whisply_protocol::models::ResponseItem;
use whisply_protocol::user_input::UserInput;

const TEST_INSTRUCTIONS: &str = "Global test instructions";

#[tokio::test]
async fn build_prompt_input_includes_context_and_user_message() -> Result<()> {
    let codex_home = TempDir::new()?;
    let cwd = TempDir::new()?;
    std::fs::write(codex_home.path().join("AGENTS.md"), TEST_INSTRUCTIONS)?;
    let config = ConfigBuilder::default()
        .codex_home(codex_home.path().to_path_buf())
        .harness_overrides(ConfigOverrides {
            cwd: Some(cwd.path().to_path_buf()),
            codex_self_exe: Some(std::env::current_exe()?),
            ..ConfigOverrides::default()
        })
        .build()
        .await?;
    let user_instructions_provider = Arc::new(CodexHomeUserInstructionsProvider::new(
        config.codex_home.clone(),
    ));
    let input = build_prompt_input(
        config,
        vec![UserInput::Text {
            text: "hello from debug prompt".to_string(),
            text_elements: Vec::new(),
        }],
        /*state_db*/ None,
        Arc::new(ExtensionRegistryBuilder::new().build()),
        user_instructions_provider,
    )
    .await?;

    let expected_user_message = ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "hello from debug prompt".to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    };
    assert_eq!(
        input
            .last()
            .cloned()
            .map(strip_metadata)
            .map(strip_response_item_id),
        Some(expected_user_message)
    );
    assert!(input.iter().any(|item| {
        let ResponseItem::Message { content, .. } = item else {
            return false;
        };

        content.iter().any(|content_item| {
            let (ContentItem::InputText { text } | ContentItem::OutputText { text }) = content_item
            else {
                return false;
            };
            text.contains(TEST_INSTRUCTIONS)
        })
    }));
    Ok(())
}

/// The composition diagnostics must describe the prompt that is really built,
/// and must stay safe to record: layer identity and digests, never the text.
#[tokio::test]
async fn prompt_composition_diagnostics_describe_the_real_prompt_without_text() -> Result<()> {
    let codex_home = TempDir::new()?;
    let cwd = TempDir::new()?;
    std::fs::write(codex_home.path().join("AGENTS.md"), TEST_INSTRUCTIONS)?;
    let config = ConfigBuilder::default()
        .codex_home(codex_home.path().to_path_buf())
        .harness_overrides(ConfigOverrides {
            cwd: Some(cwd.path().to_path_buf()),
            codex_self_exe: Some(std::env::current_exe()?),
            ..ConfigOverrides::default()
        })
        .build()
        .await?;
    let user_instructions_provider = Arc::new(CodexHomeUserInstructionsProvider::new(
        config.codex_home.clone(),
    ));

    let diagnostics = build_prompt_composition_diagnostics(
        config,
        vec![UserInput::Text {
            text: "hello from debug prompt".to_string(),
            text_elements: Vec::new(),
        }],
        /*state_db*/ None,
        Arc::new(ExtensionRegistryBuilder::new().build()),
        user_instructions_provider,
        /*workspace_selected*/ true,
    )
    .await?;

    assert_eq!(diagnostics.profile, codex_whisply::PromptProfile::Workspace);
    assert_eq!(
        diagnostics.contract_version,
        codex_whisply::PROMPT_COMPOSITION_CONTRACT_VERSION
    );

    let layer_ids: Vec<&str> = diagnostics
        .layers
        .iter()
        .map(|layer| layer.layer_id.as_str())
        .collect();
    assert!(layer_ids.contains(&"product_identity"), "{layer_ids:?}");
    assert!(layer_ids.contains(&"user_request"), "{layer_ids:?}");
    assert!(
        diagnostics
            .layers
            .windows(2)
            .all(|pair| pair[0].precedence < pair[1].precedence),
        "layers must be reported in contract precedence order"
    );

    let encoded = serde_json::to_string(&diagnostics)?;
    assert!(!encoded.contains("hello from debug prompt"), "{encoded}");
    assert!(!encoded.contains(TEST_INSTRUCTIONS), "{encoded}");
    Ok(())
}

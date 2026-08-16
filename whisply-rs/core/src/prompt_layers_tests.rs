use super::*;
use pretty_assertions::assert_eq;
use whisply_protocol::models::BaseInstructions;
use whisply_tools::JsonSchema;
use whisply_tools::ResponsesApiTool;
use whisply_tools::ToolSpec;

fn user_message(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }
}

fn assistant_message(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: text.to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }
}

fn function_tool(name: &str) -> ToolSpec {
    ToolSpec::Function(ResponsesApiTool {
        name: name.to_string(),
        description: String::new(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(Default::default(), None, None),
        output_schema: None,
    })
}

fn prompt(tools: Vec<ToolSpec>, input: Vec<ResponseItem>) -> Prompt {
    Prompt {
        input,
        tools,
        parallel_tool_calls: false,
        base_instructions: BaseInstructions {
            text: "identity instructions".to_string(),
        },
        output_schema: None,
        output_schema_strict: true,
    }
}

fn layer_ids(diagnostics: &PromptCompositionDiagnostics) -> Vec<&str> {
    diagnostics
        .layers
        .iter()
        .map(|layer| layer.layer_id.as_str())
        .collect()
}

#[test]
fn projects_layers_in_contract_precedence_order() {
    let diagnostics = prompt_composition_diagnostics(
        &prompt(vec![function_tool("shell")], vec![user_message("hello")]),
        /*workspace_selected*/ false,
    )
    .expect("real prompts must satisfy the contract");

    assert_eq!(
        layer_ids(&diagnostics),
        vec!["product_identity", "tool_policy_metadata", "user_request"]
    );
    assert!(
        diagnostics
            .layers
            .windows(2)
            .all(|pair| pair[0].precedence < pair[1].precedence)
    );
}

#[test]
fn workspace_profile_follows_the_explicit_directory_choice() {
    let with_directory = prompt_composition_diagnostics(
        &prompt(Vec::new(), vec![user_message("hello")]),
        /*workspace_selected*/ true,
    )
    .expect("composition");
    let without_directory = prompt_composition_diagnostics(
        &prompt(Vec::new(), vec![user_message("hello")]),
        /*workspace_selected*/ false,
    )
    .expect("composition");

    assert_eq!(with_directory.profile, PromptProfile::Workspace);
    assert_eq!(without_directory.profile, PromptProfile::General);
}

#[test]
fn diagnostics_carry_digests_and_never_prompt_text() {
    let diagnostics = prompt_composition_diagnostics(
        &prompt(Vec::new(), vec![user_message("a private request")]),
        /*workspace_selected*/ false,
    )
    .expect("composition");

    let encoded = serde_json::to_string(&diagnostics).expect("diagnostics serialize");
    assert!(!encoded.contains("a private request"), "{encoded}");
    assert!(!encoded.contains("identity instructions"), "{encoded}");
    for layer in &diagnostics.layers {
        assert_eq!(layer.text_sha256.len(), 64, "expected a sha256 digest");
        assert!(layer.byte_length > 0);
    }
}

#[test]
fn only_the_tool_layer_names_tools_and_only_visible_ones() {
    let diagnostics = prompt_composition_diagnostics(
        &prompt(
            vec![function_tool("shell"), function_tool("apply_patch")],
            vec![user_message("hello")],
        ),
        /*workspace_selected*/ false,
    )
    .expect("composition");

    // The tool layer's digest must change when the visible tool set changes,
    // otherwise a silent widening of model-visible tools would not be provable.
    let narrowed = prompt_composition_diagnostics(
        &prompt(vec![function_tool("shell")], vec![user_message("hello")]),
        /*workspace_selected*/ false,
    )
    .expect("composition");

    let tool_digest = |diagnostics: &PromptCompositionDiagnostics| {
        diagnostics
            .layers
            .iter()
            .find(|layer| layer.layer_id == "tool_policy_metadata")
            .map(|layer| layer.text_sha256.clone())
            .expect("tool layer")
    };
    assert_ne!(tool_digest(&diagnostics), tool_digest(&narrowed));
}

#[test]
fn user_request_layer_uses_the_latest_user_message() {
    let diagnostics = prompt_composition_diagnostics(
        &prompt(
            Vec::new(),
            vec![
                user_message("first request"),
                assistant_message("an answer"),
                user_message("latest request"),
            ],
        ),
        /*workspace_selected*/ false,
    )
    .expect("composition");

    let expected = prompt_composition_diagnostics(
        &prompt(Vec::new(), vec![user_message("latest request")]),
        /*workspace_selected*/ false,
    )
    .expect("composition");

    let request_digest = |diagnostics: &PromptCompositionDiagnostics| {
        diagnostics
            .layers
            .iter()
            .find(|layer| layer.layer_id == "user_request")
            .map(|layer| layer.text_sha256.clone())
            .expect("user request layer")
    };
    assert_eq!(request_digest(&diagnostics), request_digest(&expected));
}

#[test]
fn a_prompt_without_a_user_request_is_refused() {
    // The contract requires the request layer. A turn that reached the model
    // with no user request would mean history assembly dropped it.
    let error = prompt_composition_diagnostics(
        &prompt(Vec::new(), vec![assistant_message("only an answer")]),
        /*workspace_selected*/ false,
    )
    .expect_err("a prompt with no user request must not compose");

    assert_eq!(
        error,
        PromptCompositionError::MissingRequiredLayer {
            layer: "user_request"
        }
    );
}

/// A model as the signed catalog returns it: the same profile-neutral Whisply
/// identity regardless of model, because the catalog is fetched before the user
/// has chosen a directory.
fn managed_model() -> ModelInfo {
    let mut info = whisply_models_manager::model_info::model_info_from_slug("gpt-5.5");
    info.model_messages = Some(whisply_protocol::openai_models::ModelMessages {
        instructions_template: Some(codex_whisply::WHISPLY_GENERAL_ASSISTANT_IDENTITY.to_string()),
        instructions_variables: None,
        approvals: None,
        collaboration_modes: None,
        auto_review: None,
        permissions: None,
        token_budget: None,
    });
    info
}

fn model_with_template(template: &str) -> ModelInfo {
    let mut info = managed_model();
    if let Some(messages) = info.model_messages.as_mut() {
        messages.instructions_template = Some(template.to_string());
    }
    info
}

#[tokio::test]
async fn selecting_a_directory_gives_the_managed_session_the_coding_instructions() {
    // The defect this covers: the profile contract existed but nothing consulted
    // it, so opening a project still produced the general-assistant prompt and
    // the coding profile was unreachable in the shipped product.
    let mut config = crate::config::test_config().await;
    config.workspace_directory_selected = true;

    let instructions = profile_base_instructions(&managed_model(), &config);

    assert_eq!(instructions, BASE_INSTRUCTIONS_DEFAULT);
    assert_ne!(
        instructions,
        codex_whisply::WHISPLY_GENERAL_ASSISTANT_IDENTITY
    );
}

#[tokio::test]
async fn a_managed_session_without_a_directory_stays_a_general_assistant() {
    let mut config = crate::config::test_config().await;
    config.workspace_directory_selected = false;

    assert_eq!(
        profile_base_instructions(&managed_model(), &config),
        codex_whisply::WHISPLY_GENERAL_ASSISTANT_IDENTITY
    );
}

#[tokio::test]
async fn a_bring_your_own_provider_keeps_its_own_template_in_a_workspace() {
    // Overriding here would silently discard an operator's configured prompt,
    // so the profile decision is scoped to the managed provider.
    let mut config = crate::config::test_config().await;
    config.workspace_directory_selected = true;
    config.model_provider_id = "ollama".to_string();

    assert_eq!(
        profile_base_instructions(&model_with_template("operator supplied"), &config),
        "operator supplied"
    );
}

fn developer_message(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "developer".to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }
}

fn agents_md_message(body: &str) -> ResponseItem {
    user_message(&format!(
        "{}\n\n<INSTRUCTIONS>\n{body}\n</INSTRUCTIONS>",
        UserInstructions::type_markers().0
    ))
}

/// A record of who supplied the prompt is not worth much if it only lists what
/// Whisply added. The project instructions and the skill catalog reach the
/// model too, and the record has to say that Codex is the one who put them
/// there.
#[test]
fn the_layers_codex_owns_are_recorded_as_codex_own_them() {
    let diagnostics = prompt_composition_diagnostics(
        &prompt(
            vec![function_tool("shell")],
            vec![
                agents_md_message("always run the linter"),
                developer_message(&format!(
                    "{SKILLS_INSTRUCTIONS_OPEN_TAG}\nwriter\n</available_skills>"
                )),
                user_message("hello"),
            ],
        ),
        /*workspace_selected*/ true,
    )
    .expect("composition");

    assert_eq!(
        layer_ids(&diagnostics),
        vec![
            "product_identity",
            "tool_policy_metadata",
            "workspace_instructions",
            "active_skills",
            "user_request",
        ]
    );
    for (layer_id, origin, source) in [
        (
            "workspace_instructions",
            "agents-md",
            LayerSource::NativeRuntime,
        ),
        (
            "active_skills",
            "standard-skills",
            LayerSource::NativeRuntime,
        ),
        (
            "product_identity",
            "whisply-base-instructions",
            LayerSource::WhisplyProduct,
        ),
    ] {
        let layer = diagnostics
            .layers
            .iter()
            .find(|layer| layer.layer_id == layer_id)
            .unwrap_or_else(|| panic!("missing {layer_id}"));
        assert_eq!(layer.origin_id, origin);
        assert_eq!(layer.source, source, "{layer_id}");
    }
    let encoded = serde_json::to_string(&diagnostics).expect("serialize");
    assert!(!encoded.contains("always run the linter"), "{encoded}");
}

/// One long project file must not cost a turn its whole record. The section
/// that does not fit is left out; everything else is still attributed.
#[test]
fn a_project_file_too_large_for_one_layer_does_not_erase_the_rest() {
    let huge = "x".repeat(MAX_PROMPT_LAYER_BYTES + 1);
    let diagnostics = prompt_composition_diagnostics(
        &prompt(
            Vec::new(),
            vec![agents_md_message(&huge), user_message("hello")],
        ),
        /*workspace_selected*/ true,
    )
    .expect("a long project file is not a contract violation");

    assert_eq!(
        layer_ids(&diagnostics),
        vec!["product_identity", "user_request"]
    );
}

/// A budget nobody measured against a real prompt is a future outage. The
/// largest instructions Whisply actually sends, plus a full tool surface and a
/// full memory layer, must fit with room to spare.
#[test]
fn the_prompts_whisply_really_sends_fit_inside_its_budget() {
    let tools: Vec<ToolSpec> = (0..64)
        .map(|index| function_tool(&format!("whisply.tool_number_{index}")))
        .collect();
    let mut prompt = prompt(tools, vec![user_message("hello")]);
    prompt.base_instructions = BaseInstructions {
        text: BASE_INSTRUCTIONS_DEFAULT.to_string(),
    };

    let diagnostics = prompt_composition_diagnostics(&prompt, /*workspace_selected*/ true)
        .expect("the real workspace prompt must fit the product budget");

    let spent: usize = diagnostics
        .layers
        .iter()
        .filter(|layer| layer.source == LayerSource::WhisplyProduct)
        .map(|layer| layer.byte_length)
        .sum();
    let budget = LayerSource::WhisplyProduct
        .budget()
        .expect("the product's footprint is budgeted");
    let headroom = budget - spent;
    assert!(
        headroom > codex_whisply::MAX_PERSONALIZATION_BYTES * 2,
        "spent {spent} of {budget}; too little room left for the memory and mode layers"
    );
}

/// Project instructions arrive in the user role too, and not always before the
/// person speaks. Reading the latest user message blindly would file their
/// project file as the thing they just asked for.
#[test]
fn project_instructions_are_not_mistaken_for_the_request() {
    let diagnostics = prompt_composition_diagnostics(
        &prompt(
            Vec::new(),
            vec![
                user_message("hello"),
                agents_md_message("always run the linter"),
            ],
        ),
        /*workspace_selected*/ true,
    )
    .expect("composition");

    let request = diagnostics
        .layers
        .iter()
        .find(|layer| layer.layer_id == "user_request")
        .expect("the person's request is recorded");
    assert_eq!(request.byte_length, "hello".len());
    assert!(layer_ids(&diagnostics).contains(&"workspace_instructions"));
}

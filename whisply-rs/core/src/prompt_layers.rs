//! Projects a real assembled prompt onto the layered composition contract.
//!
//! The contract in `codex_whisply` defines which source owns each prompt layer,
//! what order the layers apply in, and that only the tool-policy layer may name
//! tools the policy boundary already granted. Defining that is not the same as
//! holding to it: the prompt that actually reaches the model is assembled here
//! in core, from base instructions, the tool router's visible specs, and turn
//! history.
//!
//! This module maps that assembled prompt onto the contract so the rules are
//! checked against what really runs, and projects
//! [`PromptCompositionDiagnostics`] — layer identity, ordering, and digests,
//! never prompt text — so every surface can prove which prompt revision a turn
//! used without exposing the instructions themselves.
//!
//! Projection is deliberately read-only. It observes the prompt, and never
//! edits, reorders, or filters what is sent.
//!
//! [`profile_base_instructions`] is the one part of this module that decides
//! rather than observes. The contract defined which profile a workspace intent
//! activates, but nothing consulted it, so every session — with or without a
//! selected directory — received the same general-assistant instructions and
//! the coding profile existed only on paper.

use codex_whisply::LayerSource;
use codex_whisply::MAX_PROMPT_LAYER_BYTES;
use codex_whisply::PromptCompositionDiagnostics;
use codex_whisply::PromptCompositionError;
use codex_whisply::PromptContribution;
use codex_whisply::PromptLayerKind;
use codex_whisply::PromptProfile;
use codex_whisply::ToolAuthority;
use codex_whisply::WHISPLY_PROVIDER_ID;
use codex_whisply::WorkspaceIntent;
use whisply_protocol::models::BASE_INSTRUCTIONS_DEFAULT;
use whisply_protocol::models::ContentItem;
use whisply_protocol::models::ResponseItem;
use whisply_protocol::openai_models::ModelInfo;
use whisply_protocol::protocol::SKILLS_INSTRUCTIONS_OPEN_TAG;

use crate::client_common::Prompt;
use crate::config::Config;
use crate::context::ContextualUserFragment;
use crate::context::UserInstructions;

/// Origin label for the resolved base instructions.
///
/// This is deliberately not one of `NATIVE_DISCOVERY_ORIGINS`: those name
/// upstream discovery outputs such as `agents-md`, and the contract refuses a
/// Whisply contribution that re-emits them.
const BASE_INSTRUCTIONS_ORIGIN: &str = "whisply-base-instructions";

/// Origin label for the tool specs the router made visible to the model.
const TOOL_ROUTER_ORIGIN: &str = "whisply-tool-router";

/// Origin label for the turn's user request.
const USER_REQUEST_ORIGIN: &str = "native-user-request";

/// Origin label for the project instructions Codex discovered.
///
/// This is one of `NATIVE_DISCOVERY_ORIGINS`, which is the point: the contract
/// refuses it from a Whisply source, so recording it here as native both
/// attributes it correctly and proves Whisply is not re-emitting it.
const AGENTS_MD_ORIGIN: &str = "agents-md";

/// Origin label for the skill catalog Codex made visible.
const STANDARD_SKILLS_ORIGIN: &str = "standard-skills";

/// Resolves the base instructions the active prompt profile calls for.
///
/// Plan §14.2 makes the general assistant the default so ordinary questions are
/// not treated as software tasks, and §14.3 says that opening a project enables
/// the upstream coding strengths while still using Whisply identity. The signed
/// catalog cannot make that choice: it is fetched before the user picks a
/// directory and returns the same profile-neutral identity for every model. So
/// the choice belongs here, where the directory intent is actually known.
///
/// This applies only to the managed provider. A bring-your-own or local model
/// carries its own instruction template, and overriding it would discard the
/// operator's configuration.
pub fn profile_base_instructions(model_info: &ModelInfo, config: &Config) -> String {
    let managed = config.model_provider_id == WHISPLY_PROVIDER_ID;
    let profile = PromptProfile::activate(if config.workspace_directory_selected {
        WorkspaceIntent::SelectedDirectory
    } else {
        WorkspaceIntent::NoDirectory
    });

    if managed && profile == PromptProfile::Workspace {
        // The upstream coding instructions, which already introduce the product
        // as Whisply, so the workspace profile keeps product identity while
        // regaining the full coding workflow.
        return BASE_INSTRUCTIONS_DEFAULT.to_string();
    }

    model_info.get_model_instructions(config.personality)
}

/// Observes `prompt` and returns its contract diagnostics.
///
/// `workspace_selected` reflects the user's explicit directory choice, which is
/// the only thing that activates the workspace profile. It is never inferred
/// from a launcher cwd.
pub fn prompt_composition_diagnostics(
    prompt: &Prompt,
    workspace_selected: bool,
) -> Result<PromptCompositionDiagnostics, PromptCompositionError> {
    let intent = if workspace_selected {
        WorkspaceIntent::SelectedDirectory
    } else {
        WorkspaceIntent::NoDirectory
    };

    let tool_ids: Vec<String> = prompt
        .tools
        .iter()
        .map(|tool| tool.name().to_string())
        .collect();
    // The router has already applied policy, so its visible specs *are* the
    // granted authority. Deriving authority from the same list keeps this a
    // projection: it can detect a prompt layer naming a tool outside the
    // visible set, but it cannot invent authority of its own.
    let authority = ToolAuthority::granted(tool_ids.clone());

    let mut contributions = vec![PromptContribution {
        layer: PromptLayerKind::ProductIdentity,
        source: LayerSource::WhisplyProduct,
        origin_id: BASE_INSTRUCTIONS_ORIGIN.to_string(),
        text: prompt.base_instructions.text.clone(),
        declared_tool_ids: Vec::new(),
    }];

    if !tool_ids.is_empty() {
        contributions.push(PromptContribution {
            layer: PromptLayerKind::ToolPolicyMetadata,
            source: LayerSource::WhisplyProduct,
            origin_id: TOOL_ROUTER_ORIGIN.to_string(),
            // The digest covers the visible tool names, so a change to which
            // tools the model can see changes the recorded revision.
            text: tool_ids.join("\n"),
            declared_tool_ids: tool_ids,
        });
    }

    // The layers Codex owns are observed from the prompt rather than declared,
    // so the record says who supplied what instead of only what Whisply added.
    // A section larger than one layer may hold is left out rather than allowed
    // to refuse the whole composition: losing every layer's provenance because
    // one project file is long would make the record useless exactly when a
    // thread is most unusual.
    if let Some(instructions) =
        native_section(&prompt.input, "user", UserInstructions::type_markers().0)
    {
        contributions.push(PromptContribution {
            layer: PromptLayerKind::WorkspaceInstructions,
            source: LayerSource::NativeRuntime,
            origin_id: AGENTS_MD_ORIGIN.to_string(),
            text: instructions,
            declared_tool_ids: Vec::new(),
        });
    }

    if let Some(skills) = native_section(&prompt.input, "developer", SKILLS_INSTRUCTIONS_OPEN_TAG) {
        contributions.push(PromptContribution {
            layer: PromptLayerKind::ActiveSkills,
            source: LayerSource::NativeRuntime,
            origin_id: STANDARD_SKILLS_ORIGIN.to_string(),
            text: skills,
            declared_tool_ids: Vec::new(),
        });
    }

    if let Some(request) = latest_user_request(&prompt.input) {
        contributions.push(PromptContribution {
            layer: PromptLayerKind::UserRequest,
            source: LayerSource::NativeRuntime,
            origin_id: USER_REQUEST_ORIGIN.to_string(),
            text: request,
            declared_tool_ids: Vec::new(),
        });
    }

    codex_whisply::ComposedPrompt::compose(
        PromptProfile::activate(intent),
        /*mode*/ None,
        &authority,
        contributions,
    )
    .map(|composed| composed.diagnostics())
}

/// Returns the message text carrying a native section, if it is present and
/// small enough to be one layer.
///
/// Sections are recognised by the marker their own fragment type declares, so
/// this reads what the runtime wrote rather than guessing at wording.
fn native_section(input: &[ResponseItem], role_wanted: &str, marker: &str) -> Option<String> {
    input.iter().rev().find_map(|item| match item {
        ResponseItem::Message { role, content, .. } if role == role_wanted => {
            let text = message_text(content);
            (text.contains(marker) && text.len() <= MAX_PROMPT_LAYER_BYTES).then_some(text)
        }
        _ => None,
    })
}

fn message_text(content: &[ContentItem]) -> String {
    content
        .iter()
        .filter_map(|chunk| match chunk {
            ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                Some(text.as_str())
            }
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Returns the most recent user message text in the turn input.
///
/// The contract requires a user-request layer, and only the latest message is
/// the request being answered; earlier turns are history that the runtime owns.
/// Project instructions arrive in the user role as well, so they are skipped:
/// they are context the runtime supplied, not something the person asked for.
fn latest_user_request(input: &[ResponseItem]) -> Option<String> {
    let agents_md = UserInstructions::type_markers().0;
    input.iter().rev().find_map(|item| match item {
        ResponseItem::Message { role, content, .. } if role == "user" => {
            let text = message_text(content);
            (!text.trim().is_empty() && !text.contains(agents_md)).then_some(text)
        }
        _ => None,
    })
}

#[cfg(test)]
#[path = "prompt_layers_tests.rs"]
mod tests;

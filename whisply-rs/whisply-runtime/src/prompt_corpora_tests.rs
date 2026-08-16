//! Fixture-driven prompt corpora and the unified injection matrix.

use super::*;
use crate::UntrustedChannel;
use pretty_assertions::assert_eq;
use serde::Deserialize;
use std::collections::BTreeSet;

const CORPORA_JSON: &str = include_str!("../tests/fixtures/whisply-prompt-corpora-v1.json");

/// Words that would make a request a coding task. The ordinary-assistant
/// corpus must stay clear of all of them.
const CODING_TASK_MARKERS: &[&str] = &[
    "bug",
    "build",
    "class",
    "code",
    "commit",
    "compile",
    "debug",
    "deploy",
    "function",
    "merge",
    "refactor",
    "repo",
    "test",
    "typescript",
];

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PromptCorpora {
    schema_version: u16,
    corpus_revision: String,
    assistant_scenarios: Vec<AssistantScenario>,
    injection_scenarios: Vec<InjectionScenario>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AssistantScenario {
    id: String,
    category: Category,
    request: String,
    workspace_intent: WorkspaceIntent,
    expected_profile: PromptProfile,
    granted_tool_ids: Vec<String>,
    expected_tool_metadata_tool_ids: Vec<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
enum Category {
    OrdinaryAssistant,
    WorkspaceCoding,
    Research,
    Document,
    Browser,
    Connector,
    NativeAction,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct InjectionScenario {
    id: String,
    channel: UntrustedChannel,
    origin_id: String,
    content: String,
    escalation: Escalation,
    expected_refusal: Option<ExpectedRefusal>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Escalation {
    None,
    DeclareTool,
    ClaimRuntimeLayer,
    ReemitNativeDiscovery,
    UngrantedTool,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ExpectedRefusal {
    PromptCannotDeclareTools,
    SourceNotPermitted,
    DuplicatesNativeDiscovery,
    AuthorityNotGranted,
}

impl ExpectedRefusal {
    fn matches(self, error: &PromptCompositionError) -> bool {
        matches!(
            (self, error),
            (
                Self::PromptCannotDeclareTools,
                PromptCompositionError::PromptCannotDeclareTools { .. }
            ) | (
                Self::SourceNotPermitted,
                PromptCompositionError::SourceNotPermitted { .. }
            ) | (
                Self::DuplicatesNativeDiscovery,
                PromptCompositionError::DuplicatesNativeDiscovery { .. }
            ) | (
                Self::AuthorityNotGranted,
                PromptCompositionError::AuthorityNotGranted { .. }
            )
        )
    }
}

fn corpora() -> PromptCorpora {
    serde_json::from_str(CORPORA_JSON).expect("shared prompt corpora fixture")
}

fn identity_contribution() -> PromptContribution {
    PromptContribution {
        layer: PromptLayerKind::ProductIdentity,
        source: LayerSource::WhisplyProduct,
        origin_id: "whisply-identity".to_string(),
        text: WHISPLY_GENERAL_ASSISTANT_IDENTITY.to_string(),
        declared_tool_ids: Vec::new(),
    }
}

fn request_contribution(request: &str) -> PromptContribution {
    PromptContribution {
        layer: PromptLayerKind::UserRequest,
        source: LayerSource::WhisplyProduct,
        origin_id: "user-message".to_string(),
        text: request.to_string(),
        declared_tool_ids: Vec::new(),
    }
}

#[test]
fn the_corpus_fixture_matches_its_pinned_revision() {
    let corpora = corpora();

    assert_eq!(corpora.schema_version, 1);
    assert_eq!(corpora.corpus_revision, "prompt-corpora-2026-08-12.1");
}

#[test]
fn every_assistant_scenario_activates_its_expected_profile_and_tool_metadata() {
    for scenario in corpora().assistant_scenarios {
        let authority = ToolAuthority::granted(scenario.granted_tool_ids.clone());
        let profile = PromptProfile::activate(scenario.workspace_intent);
        assert_eq!(
            profile, scenario.expected_profile,
            "scenario {} activated the wrong profile",
            scenario.id
        );

        let mut contributions = vec![
            identity_contribution(),
            request_contribution(&scenario.request),
        ];
        if !scenario.expected_tool_metadata_tool_ids.is_empty() {
            contributions.push(PromptContribution {
                layer: PromptLayerKind::ToolPolicyMetadata,
                source: LayerSource::WhisplyProduct,
                origin_id: "turn-tool-metadata".to_string(),
                text: format!("tools available for {}", scenario.id),
                declared_tool_ids: scenario.expected_tool_metadata_tool_ids.clone(),
            });
        }

        let composed =
            ComposedPrompt::compose(profile, /*mode*/ None, &authority, contributions)
                .unwrap_or_else(|error| {
                    panic!("scenario {} failed to compose: {error}", scenario.id)
                });

        let declared: Vec<String> = composed
            .contributions()
            .iter()
            .filter(|contribution| contribution.layer == PromptLayerKind::ToolPolicyMetadata)
            .flat_map(|contribution| contribution.declared_tool_ids.clone())
            .collect();
        assert_eq!(
            declared, scenario.expected_tool_metadata_tool_ids,
            "scenario {} projected the wrong tool metadata",
            scenario.id
        );
    }
}

#[test]
fn the_ordinary_assistant_corpus_contains_no_coding_task() {
    let ordinary: Vec<AssistantScenario> = corpora()
        .assistant_scenarios
        .into_iter()
        .filter(|scenario| scenario.category == Category::OrdinaryAssistant)
        .collect();

    assert!(
        ordinary.len() >= 4,
        "the ordinary-assistant corpus needs several non-coding requests"
    );
    for scenario in ordinary {
        assert_eq!(
            scenario.workspace_intent,
            WorkspaceIntent::NoDirectory,
            "ordinary scenario {} must not assume a workspace",
            scenario.id
        );
        assert_eq!(
            scenario.expected_profile,
            PromptProfile::General,
            "ordinary scenario {} must stay on the general profile",
            scenario.id
        );
        assert!(
            scenario.granted_tool_ids.is_empty(),
            "ordinary scenario {} must not assume tool authority",
            scenario.id
        );
        let request = scenario.request.to_ascii_lowercase();
        for marker in CODING_TASK_MARKERS {
            assert!(
                !request
                    .split(|character: char| !character.is_ascii_alphanumeric())
                    .any(|word| word == *marker),
                "ordinary scenario {} contains the coding-task word {marker}",
                scenario.id
            );
        }
    }
}

#[test]
fn the_corpus_covers_every_required_workspace_category() {
    let categories: BTreeSet<Category> = corpora()
        .assistant_scenarios
        .iter()
        .map(|scenario| scenario.category)
        .collect();

    assert_eq!(
        categories,
        BTreeSet::from([
            Category::OrdinaryAssistant,
            Category::WorkspaceCoding,
            Category::Research,
            Category::Document,
            Category::Browser,
            Category::Connector,
            Category::NativeAction,
        ])
    );
}

#[test]
fn the_injection_matrix_covers_every_untrusted_channel() {
    let scenarios = corpora().injection_scenarios;

    for channel in UntrustedChannel::ALL {
        let for_channel: Vec<&InjectionScenario> = scenarios
            .iter()
            .filter(|scenario| scenario.channel == channel)
            .collect();
        assert!(
            for_channel
                .iter()
                .any(|scenario| scenario.escalation == Escalation::None),
            "channel {} needs a benign observation case",
            channel.channel_id()
        );
        assert!(
            for_channel
                .iter()
                .any(|scenario| scenario.escalation == Escalation::DeclareTool),
            "channel {} needs a tool-authority escalation case",
            channel.channel_id()
        );
    }
}

#[test]
fn no_untrusted_channel_can_escalate_into_prompt_authority() {
    let authority = ToolAuthority::granted(["whisply.files"]);

    for scenario in corpora().injection_scenarios {
        let channel = scenario.channel;
        let forged = match scenario.escalation {
            Escalation::None => channel.observation(&scenario.origin_id, &scenario.content),
            Escalation::DeclareTool => PromptContribution {
                declared_tool_ids: vec!["whisply.computer_use".to_string()],
                ..channel.observation(&scenario.origin_id, &scenario.content)
            },
            Escalation::ClaimRuntimeLayer => PromptContribution {
                layer: PromptLayerKind::RuntimeInvariants,
                source: LayerSource::WhisplyProduct,
                ..channel.observation(&scenario.origin_id, &scenario.content)
            },
            Escalation::ReemitNativeDiscovery => PromptContribution {
                layer: PromptLayerKind::WorkspaceInstructions,
                source: LayerSource::WhisplyProduct,
                origin_id: "agents-md".to_string(),
                ..channel.observation(&scenario.origin_id, &scenario.content)
            },
            Escalation::UngrantedTool => PromptContribution {
                layer: PromptLayerKind::ToolPolicyMetadata,
                source: LayerSource::WhisplyProduct,
                declared_tool_ids: vec!["whisply.computer_use".to_string()],
                ..channel.observation(&scenario.origin_id, &scenario.content)
            },
        };

        let outcome = ComposedPrompt::compose(
            PromptProfile::General,
            /*mode*/ None,
            &authority,
            vec![
                identity_contribution(),
                request_contribution("summarize what you found"),
                forged,
            ],
        );

        match (scenario.expected_refusal, outcome) {
            (None, Ok(composed)) => assert!(
                composed
                    .contributions()
                    .iter()
                    .all(|contribution| contribution.declared_tool_ids.is_empty()),
                "benign scenario {} must not carry tool authority",
                scenario.id
            ),
            (Some(expected), Err(error)) => assert!(
                expected.matches(&error),
                "scenario {} expected {expected:?} but got {error}",
                scenario.id
            ),
            (None, Err(error)) => {
                panic!("benign scenario {} was refused: {error}", scenario.id)
            }
            (Some(expected), Ok(_)) => {
                panic!(
                    "scenario {} should have been refused with {expected:?}",
                    scenario.id
                )
            }
        }
    }
}

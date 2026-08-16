use super::*;
use crate::DirectorySelection;
use crate::SUPPORTED_CATALOG_PROFILES;
use pretty_assertions::assert_eq;

fn authority() -> ToolAuthority {
    ToolAuthority::granted(["whisply.files", "whisply.screen.context"])
}

fn contribution(
    layer: PromptLayerKind,
    source: LayerSource,
    origin_id: &str,
) -> PromptContribution {
    PromptContribution {
        layer,
        source,
        origin_id: origin_id.to_string(),
        text: format!("contribution text for {origin_id}"),
        declared_tool_ids: Vec::new(),
    }
}

fn full_composition() -> Vec<PromptContribution> {
    vec![
        contribution(
            PromptLayerKind::UserRequest,
            LayerSource::WhisplyProduct,
            "user-message",
        ),
        contribution(
            PromptLayerKind::ActiveSkills,
            LayerSource::NativeRuntime,
            "standard-skills",
        ),
        contribution(
            PromptLayerKind::WorkspaceInstructions,
            LayerSource::WhisplyProduct,
            "whisply-config",
        ),
        contribution(
            PromptLayerKind::WorkspaceInstructions,
            LayerSource::NativeRuntime,
            "agents-md",
        ),
        contribution(
            PromptLayerKind::PersonalizationMemory,
            LayerSource::WhisplyProduct,
            "account-memory",
        ),
        PromptContribution {
            declared_tool_ids: vec!["whisply.files".to_string()],
            ..contribution(
                PromptLayerKind::ToolPolicyMetadata,
                LayerSource::WhisplyProduct,
                "turn-tool-metadata",
            )
        },
        contribution(
            PromptLayerKind::ModeProfile,
            LayerSource::WhisplyProduct,
            "selected-mode",
        ),
        PromptContribution {
            text: WHISPLY_GENERAL_ASSISTANT_IDENTITY.to_string(),
            ..contribution(
                PromptLayerKind::ProductIdentity,
                LayerSource::WhisplyProduct,
                "whisply-identity",
            )
        },
        contribution(
            PromptLayerKind::RuntimeInvariants,
            LayerSource::NativeRuntime,
            "runtime-safety",
        ),
    ]
}

fn compose(
    contributions: Vec<PromptContribution>,
) -> Result<ComposedPrompt, PromptCompositionError> {
    ComposedPrompt::compose(
        PromptProfile::General,
        /*mode*/ None,
        &authority(),
        contributions,
    )
}

#[test]
fn layers_compose_in_the_documented_precedence_order() {
    let composed = compose(full_composition()).expect("composition");

    let ordered: Vec<(&str, LayerSource)> = composed
        .contributions()
        .iter()
        .map(|contribution| (contribution.layer.layer_id(), contribution.source))
        .collect();

    assert_eq!(
        ordered,
        vec![
            ("runtime_invariants", LayerSource::NativeRuntime),
            ("product_identity", LayerSource::WhisplyProduct),
            ("mode_profile", LayerSource::WhisplyProduct),
            ("tool_policy_metadata", LayerSource::WhisplyProduct),
            ("personalization_memory", LayerSource::WhisplyProduct),
            ("workspace_instructions", LayerSource::NativeRuntime),
            ("workspace_instructions", LayerSource::WhisplyProduct),
            ("active_skills", LayerSource::NativeRuntime),
            ("user_request", LayerSource::WhisplyProduct),
        ]
    );
}

#[test]
fn profile_activates_from_explicit_workspace_intent() {
    assert_eq!(
        PromptProfile::activate(WorkspaceIntent::from(&DirectorySelection::NoDirectory)),
        PromptProfile::General
    );
    assert_eq!(
        PromptProfile::activate(WorkspaceIntent::from(&DirectorySelection::Selected {
            canonical_path: std::path::PathBuf::from("/workspace"),
        })),
        PromptProfile::Workspace
    );
}

#[test]
fn every_profile_is_advertisable_by_the_signed_catalog() {
    let profiles: Vec<&str> = [PromptProfile::General, PromptProfile::Workspace]
        .into_iter()
        .map(PromptProfile::catalog_id)
        .collect();

    assert_eq!(profiles, SUPPORTED_CATALOG_PROFILES.to_vec());
}

#[test]
fn whisply_cannot_supply_a_native_runtime_owned_layer() {
    let mut contributions = full_composition();
    contributions.push(contribution(
        PromptLayerKind::RuntimeInvariants,
        LayerSource::WhisplyProduct,
        "product-safety-override",
    ));

    assert_eq!(
        compose(contributions),
        Err(PromptCompositionError::SourceNotPermitted {
            layer: "runtime_invariants",
            layer_source: LayerSource::WhisplyProduct,
        })
    );
}

#[test]
fn whisply_cannot_re_emit_codex_owned_native_discovery() {
    let contributions = full_composition()
        .into_iter()
        .map(|contribution| {
            if contribution.origin_id == "whisply-config" {
                PromptContribution {
                    origin_id: "agents-md".to_string(),
                    ..contribution
                }
            } else {
                contribution
            }
        })
        .collect();

    assert_eq!(
        compose(contributions),
        Err(PromptCompositionError::DuplicatesNativeDiscovery {
            origin_id: "agents-md".to_string(),
        })
    );
}

#[test]
fn an_identical_contribution_cannot_be_injected_twice() {
    let mut contributions = full_composition();
    contributions.push(contribution(
        PromptLayerKind::PersonalizationMemory,
        LayerSource::WhisplyProduct,
        "account-memory-copy",
    ));
    let duplicated = contributions.last().expect("appended").text.clone();
    contributions.last_mut().expect("appended").text = duplicated;
    contributions
        .iter_mut()
        .find(|contribution| contribution.origin_id == "account-memory-copy")
        .expect("copy")
        .text = "contribution text for account-memory".to_string();

    assert_eq!(
        compose(contributions),
        Err(PromptCompositionError::DuplicateContribution {
            layer: "personalization_memory",
        })
    );
}

#[test]
fn a_prompt_layer_cannot_declare_tool_authority() {
    let contributions = full_composition()
        .into_iter()
        .map(|contribution| {
            if contribution.layer == PromptLayerKind::PersonalizationMemory {
                PromptContribution {
                    declared_tool_ids: vec!["whisply.files".to_string()],
                    ..contribution
                }
            } else {
                contribution
            }
        })
        .collect();

    assert_eq!(
        compose(contributions),
        Err(PromptCompositionError::PromptCannotDeclareTools {
            layer: "personalization_memory",
        })
    );
}

#[test]
fn tool_metadata_cannot_exceed_the_granted_policy_boundary() {
    let contributions = full_composition()
        .into_iter()
        .map(|contribution| {
            if contribution.layer == PromptLayerKind::ToolPolicyMetadata {
                PromptContribution {
                    declared_tool_ids: vec!["whisply.computer_use".to_string()],
                    ..contribution
                }
            } else {
                contribution
            }
        })
        .collect();

    assert_eq!(
        compose(contributions),
        Err(PromptCompositionError::AuthorityNotGranted {
            layer: "tool_policy_metadata",
            tool_id: "whisply.computer_use".to_string(),
        })
    );
}

#[test]
fn a_mode_cannot_widen_the_effective_tool_authority() {
    let mode = ModeCompositionInput {
        mode_id: "exam".to_string(),
        prompt_additions: vec!["stay within the protected mode scope".to_string()],
        visible_tool_ids: vec!["whisply.browser".to_string()],
    };

    assert_eq!(
        ComposedPrompt::compose(
            PromptProfile::General,
            Some(mode),
            &authority(),
            full_composition(),
        ),
        Err(PromptCompositionError::AuthorityNotGranted {
            layer: "mode_profile",
            tool_id: "whisply.browser".to_string(),
        })
    );
}

#[test]
fn a_mode_narrows_visible_tools_without_becoming_a_separate_engine() {
    let mode = ModeCompositionInput {
        mode_id: "focus".to_string(),
        prompt_additions: vec!["prefer short answers".to_string()],
        visible_tool_ids: vec!["whisply.files".to_string()],
    };

    let composed = ComposedPrompt::compose(
        PromptProfile::Workspace,
        Some(mode),
        &authority(),
        full_composition(),
    )
    .expect("composition");

    assert_eq!(composed.diagnostics().mode_id, Some("focus".to_string()));
}

#[test]
fn a_composition_requires_the_product_identity_layer() {
    let contributions = full_composition()
        .into_iter()
        .filter(|contribution| contribution.layer != PromptLayerKind::ProductIdentity)
        .collect();

    assert_eq!(
        compose(contributions),
        Err(PromptCompositionError::MissingRequiredLayer {
            layer: "product_identity",
        })
    );
}

#[test]
fn diagnostics_record_the_revision_without_exposing_prompt_text() {
    let composed = compose(full_composition()).expect("composition");
    let diagnostics = composed.diagnostics();
    let encoded = serde_json::to_string(&diagnostics).expect("diagnostics json");

    assert_eq!(
        diagnostics.contract_version,
        PROMPT_COMPOSITION_CONTRACT_VERSION
    );
    assert_eq!(diagnostics.layers.len(), composed.contributions().len());
    for contribution in composed.contributions() {
        assert!(
            !encoded.contains(&contribution.text),
            "diagnostics leaked prompt text for {}",
            contribution.origin_id
        );
    }
    assert!(!encoded.contains("You are Whisply"));
}

#[test]
fn the_default_profile_identity_carries_no_upstream_product_name() {
    let lowercased = WHISPLY_GENERAL_ASSISTANT_IDENTITY.to_ascii_lowercase();

    assert!(!lowercased.contains("codex"));
    assert!(!lowercased.contains("chatgpt"));
    assert!(!lowercased.contains("openai"));
}

fn sized(
    layer: PromptLayerKind,
    source: LayerSource,
    origin_id: &str,
    bytes: usize,
) -> PromptContribution {
    PromptContribution {
        layer,
        source,
        origin_id: origin_id.to_string(),
        // Distinct text per origin so the size rule is what refuses this, not
        // the duplicate-contribution rule.
        text: format!(
            "{origin_id}{}",
            "x".repeat(bytes.saturating_sub(origin_id.len()))
        ),
        declared_tool_ids: Vec::new(),
    }
}

/// A cap on one layer does not bound what the product costs a person. Whisply
/// has several layers of its own, and without a budget across all of them it
/// could take most of the context window before the model reads the request.
#[test]
fn whisply_cannot_spend_more_of_the_window_than_its_budget() {
    let half = MAX_WHISPLY_INJECTED_BYTES / 2;
    let within = vec![
        sized(
            PromptLayerKind::ProductIdentity,
            LayerSource::WhisplyProduct,
            "identity",
            half,
        ),
        sized(
            PromptLayerKind::PersonalizationMemory,
            LayerSource::WhisplyProduct,
            "memory",
            half,
        ),
        contribution(
            PromptLayerKind::UserRequest,
            LayerSource::NativeRuntime,
            "native-user-request",
        ),
    ];
    compose(within).expect("a composition inside the budget is allowed");

    let over = vec![
        sized(
            PromptLayerKind::ProductIdentity,
            LayerSource::WhisplyProduct,
            "identity",
            half,
        ),
        sized(
            PromptLayerKind::PersonalizationMemory,
            LayerSource::WhisplyProduct,
            "memory",
            half,
        ),
        sized(
            PromptLayerKind::ModeProfile,
            LayerSource::WhisplyProduct,
            "mode",
            1_024,
        ),
        contribution(
            PromptLayerKind::UserRequest,
            LayerSource::NativeRuntime,
            "native-user-request",
        ),
    ];
    assert!(matches!(
        compose(over),
        Err(PromptCompositionError::SourceBudgetExceeded {
            layer_source: LayerSource::WhisplyProduct,
            budget: MAX_WHISPLY_INJECTED_BYTES,
            ..
        })
    ));
}

/// The person's conversation is not charged for the product's footprint. Codex
/// owns thread history and manages that space by compacting; budgeting it here
/// would trim someone's own words to make room for Whisply text.
#[test]
fn what_codex_owns_is_not_charged_against_the_product_budget() {
    assert_eq!(LayerSource::NativeRuntime.budget(), None);
    assert_eq!(
        LayerSource::WhisplyProduct.budget(),
        Some(MAX_WHISPLY_INJECTED_BYTES)
    );

    let mut contributions = vec![contribution(
        PromptLayerKind::ProductIdentity,
        LayerSource::WhisplyProduct,
        "identity",
    )];
    for (index, layer) in [
        PromptLayerKind::WorkspaceInstructions,
        PromptLayerKind::ActiveSkills,
        PromptLayerKind::UserRequest,
        PromptLayerKind::RuntimeInvariants,
    ]
    .into_iter()
    .enumerate()
    {
        contributions.push(sized(
            layer,
            LayerSource::NativeRuntime,
            &format!("native-{index}"),
            MAX_PROMPT_LAYER_BYTES,
        ));
    }

    compose(contributions).expect("native context is bounded by compaction, not by this contract");
}

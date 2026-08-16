//! Whisply's versioned layered prompt-composition contract.
//!
//! Plan §14.1 fixes eight ordered prompt layers and one invariant: no layer may
//! grant authority that the native or server policy boundary denies. This
//! module is the single place that ordering, layer ownership, and that
//! non-escalation rule are enforced, so a prompt surface cannot quietly become
//! a second authority channel or a duplicate of Codex's native discovery.
//!
//! Codex remains the only native-thread context engine. It owns `AGENTS.md`
//! discovery, standard skill loading, native thread history, and compaction.
//! Whisply contributes product identity, mode/profile selection, generated tool
//! metadata, selected account memory, and `.whisply` configuration.

use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use thiserror::Error;

use crate::catalog::hex_digest;
use sha2::Digest;
use sha2::Sha256;

/// The only layered prompt-composition contract this runtime line accepts.
pub const PROMPT_COMPOSITION_CONTRACT_VERSION: u16 = 1;

/// Upper bound on one layer's contributed text.
pub const MAX_PROMPT_LAYER_BYTES: usize = 32_768;

/// Upper bound on contributions in a single composition.
pub const MAX_PROMPT_CONTRIBUTIONS: usize = 32;

/// Upper bound on everything Whisply injects into one composition.
///
/// A per-layer cap alone does not bound what the product costs a person: with
/// enough layers Whisply could take a large share of the context window before
/// the model ever reads the request. This is the budget for the product's own
/// footprint, and it is deliberately far below the per-layer cap times the
/// contribution limit.
///
/// Codex's own layers are not charged against it. The native runtime owns
/// thread history, project instructions, and standard skills, and it already
/// manages that space through compaction; a second budget here would trim the
/// person's own conversation to make room for product text.
pub const MAX_WHISPLY_INJECTED_BYTES: usize = 65_536;

/// Origins that only Codex's native discovery may supply.
///
/// Whisply must not re-emit these; doing so would duplicate upstream project
/// instructions, standard skills, or recent native-thread messages.
pub const NATIVE_DISCOVERY_ORIGINS: &[&str] = &[
    "agents-md",
    "native-thread-history",
    "standard-skills",
    "upstream-instructions",
];

/// The public Whisply general-assistant behavior contract (plan §14.2).
///
/// This is the product identity layer's text. It deliberately carries no
/// upstream product name and no tool authority.
pub const WHISPLY_GENERAL_ASSISTANT_IDENTITY: &str = "You are Whisply, a general assistant. \
Answer ordinary questions naturally and treat coding as one capability rather than the assumed \
task. Use the tools that are currently available and enabled when they genuinely help. Never \
describe work you did not perform. Show concise activity summaries while real work is running, \
then give one clean final answer. Ask only when a consequential choice cannot be inferred safely. \
Honor the active Whisply mode, account capability, connector access, and user settings.";

/// Which surface supplied one contribution.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayerSource {
    /// Codex's native context engine.
    NativeRuntime,
    /// Whisply product injection.
    WhisplyProduct,
}

impl LayerSource {
    /// The total bytes this source may contribute across one composition, or
    /// `None` when the contract does not budget it.
    pub const fn budget(self) -> Option<usize> {
        match self {
            Self::WhisplyProduct => Some(MAX_WHISPLY_INJECTED_BYTES),
            Self::NativeRuntime => None,
        }
    }
}

/// The eight ordered prompt layers defined by plan §14.1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptLayerKind {
    RuntimeInvariants,
    ProductIdentity,
    ModeProfile,
    ToolPolicyMetadata,
    PersonalizationMemory,
    WorkspaceInstructions,
    ActiveSkills,
    UserRequest,
}

impl PromptLayerKind {
    /// Every layer in its fixed composition order.
    pub const ALL: [Self; 8] = [
        Self::RuntimeInvariants,
        Self::ProductIdentity,
        Self::ModeProfile,
        Self::ToolPolicyMetadata,
        Self::PersonalizationMemory,
        Self::WorkspaceInstructions,
        Self::ActiveSkills,
        Self::UserRequest,
    ];

    /// The stable wire identifier recorded in diagnostics and fixtures.
    pub const fn layer_id(self) -> &'static str {
        match self {
            Self::RuntimeInvariants => "runtime_invariants",
            Self::ProductIdentity => "product_identity",
            Self::ModeProfile => "mode_profile",
            Self::ToolPolicyMetadata => "tool_policy_metadata",
            Self::PersonalizationMemory => "personalization_memory",
            Self::WorkspaceInstructions => "workspace_instructions",
            Self::ActiveSkills => "active_skills",
            Self::UserRequest => "user_request",
        }
    }

    /// The documented precedence position, lowest first.
    pub const fn precedence(self) -> u8 {
        match self {
            Self::RuntimeInvariants => 1,
            Self::ProductIdentity => 2,
            Self::ModeProfile => 3,
            Self::ToolPolicyMetadata => 4,
            Self::PersonalizationMemory => 5,
            Self::WorkspaceInstructions => 6,
            Self::ActiveSkills => 7,
            Self::UserRequest => 8,
        }
    }

    /// Whether `source` is allowed to contribute this layer.
    pub const fn accepts(self, source: LayerSource) -> bool {
        match (self, source) {
            (Self::RuntimeInvariants | Self::ActiveSkills, LayerSource::NativeRuntime)
            | (
                Self::ProductIdentity
                | Self::ModeProfile
                | Self::ToolPolicyMetadata
                | Self::PersonalizationMemory,
                LayerSource::WhisplyProduct,
            )
            | (Self::WorkspaceInstructions | Self::UserRequest, _) => true,
            (Self::RuntimeInvariants | Self::ActiveSkills, LayerSource::WhisplyProduct)
            | (
                Self::ProductIdentity
                | Self::ModeProfile
                | Self::ToolPolicyMetadata
                | Self::PersonalizationMemory,
                LayerSource::NativeRuntime,
            ) => false,
        }
    }
}

/// The local thread profile selected for a composition.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptProfile {
    /// The default profile for a thread with no selected workspace.
    #[default]
    General,
    /// The coding profile enabled by an explicit workspace selection.
    Workspace,
}

impl PromptProfile {
    /// The identifier a signed catalog uses for this profile.
    pub const fn catalog_id(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Workspace => "workspace",
        }
    }

    /// Activates the profile from actual workspace intent (plan §14.3).
    ///
    /// Profile selection follows the user's explicit directory choice. It is
    /// never inferred from a launcher cwd, stored history, or model output.
    pub const fn activate(intent: WorkspaceIntent) -> Self {
        match intent {
            WorkspaceIntent::NoDirectory => Self::General,
            WorkspaceIntent::SelectedDirectory => Self::Workspace,
        }
    }
}

/// The explicit workspace signal that activates a profile.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceIntent {
    #[default]
    NoDirectory,
    SelectedDirectory,
}

impl From<&crate::DirectorySelection> for WorkspaceIntent {
    fn from(selection: &crate::DirectorySelection) -> Self {
        match selection {
            crate::DirectorySelection::NoDirectory => Self::NoDirectory,
            crate::DirectorySelection::Selected { .. } => Self::SelectedDirectory,
        }
    }
}

/// One Whisply mode expressed as profile and policy input (plan §14.4).
///
/// A mode declares prompt additions and narrows visible tools. It is never a
/// separate agent engine and can never widen the effective tool authority.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModeCompositionInput {
    pub mode_id: String,
    pub prompt_additions: Vec<String>,
    pub visible_tool_ids: Vec<String>,
}

/// The effective tool authority compiled by the native and server boundary.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToolAuthority {
    allowed_tool_ids: BTreeSet<String>,
}

impl ToolAuthority {
    /// Records the tools the policy boundary has already granted.
    pub fn granted(tool_ids: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            allowed_tool_ids: tool_ids.into_iter().map(Into::into).collect(),
        }
    }

    fn permits(&self, tool_id: &str) -> bool {
        self.allowed_tool_ids.contains(tool_id)
    }
}

/// One contribution to a single prompt layer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptContribution {
    pub layer: PromptLayerKind,
    pub source: LayerSource,
    /// A stable, non-secret label such as `agents-md` or `whisply-config`.
    pub origin_id: String,
    pub text: String,
    /// Tools this contribution describes. Only the tool metadata layer may
    /// name any, and only ones the policy boundary already granted.
    pub declared_tool_ids: Vec<String>,
}

/// A validated, ordered prompt composition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComposedPrompt {
    profile: PromptProfile,
    mode_id: Option<String>,
    contributions: Vec<PromptContribution>,
}

impl ComposedPrompt {
    /// Validates and orders one composition.
    pub fn compose(
        profile: PromptProfile,
        mode: Option<ModeCompositionInput>,
        authority: &ToolAuthority,
        contributions: Vec<PromptContribution>,
    ) -> Result<Self, PromptCompositionError> {
        if contributions.len() > MAX_PROMPT_CONTRIBUTIONS {
            return Err(PromptCompositionError::TooManyContributions);
        }
        if let Some(mode) = mode.as_ref() {
            validate_mode(mode, authority)?;
        }

        let mut seen_origins = BTreeSet::new();
        let mut seen_text = BTreeSet::new();
        let mut spent = BTreeMap::new();
        for contribution in &contributions {
            validate_contribution(contribution, authority)?;
            if let Some(budget) = contribution.source.budget() {
                let spent = spent.entry(contribution.source).or_insert(0usize);
                *spent = spent.saturating_add(contribution.text.len());
                if *spent > budget {
                    return Err(PromptCompositionError::SourceBudgetExceeded {
                        layer_source: contribution.source,
                        bytes: *spent,
                        budget,
                    });
                }
            }
            if !seen_origins.insert((contribution.layer, contribution.origin_id.clone())) {
                return Err(PromptCompositionError::DuplicateOrigin {
                    layer: contribution.layer.layer_id(),
                    origin_id: contribution.origin_id.clone(),
                });
            }
            if !seen_text.insert(hex_digest(Sha256::digest(contribution.text.as_bytes()))) {
                return Err(PromptCompositionError::DuplicateContribution {
                    layer: contribution.layer.layer_id(),
                });
            }
        }

        for required in [
            PromptLayerKind::ProductIdentity,
            PromptLayerKind::UserRequest,
        ] {
            if !contributions
                .iter()
                .any(|contribution| contribution.layer == required)
            {
                return Err(PromptCompositionError::MissingRequiredLayer {
                    layer: required.layer_id(),
                });
            }
        }

        let mut contributions = contributions;
        contributions.sort_by(|left, right| {
            left.layer
                .precedence()
                .cmp(&right.layer.precedence())
                .then_with(|| left.source.cmp(&right.source))
                .then_with(|| left.origin_id.cmp(&right.origin_id))
        });

        Ok(Self {
            profile,
            mode_id: mode.map(|mode| mode.mode_id),
            contributions,
        })
    }

    /// The composed layers in documented precedence order.
    pub fn contributions(&self) -> &[PromptContribution] {
        &self.contributions
    }

    /// Projects the revision-safe diagnostics record (plan §14.5).
    ///
    /// This deliberately carries layer identity and digests but never prompt
    /// text, so diagnostics can prove which revision ran without exposing the
    /// composed instructions.
    pub fn diagnostics(&self) -> PromptCompositionDiagnostics {
        PromptCompositionDiagnostics {
            contract_version: PROMPT_COMPOSITION_CONTRACT_VERSION,
            profile: self.profile,
            mode_id: self.mode_id.clone(),
            layers: self
                .contributions
                .iter()
                .map(|contribution| PromptLayerDigest {
                    layer_id: contribution.layer.layer_id().to_string(),
                    precedence: contribution.layer.precedence(),
                    source: contribution.source,
                    origin_id: contribution.origin_id.clone(),
                    byte_length: contribution.text.len(),
                    text_sha256: hex_digest(Sha256::digest(contribution.text.as_bytes())),
                })
                .collect(),
        }
    }
}

fn validate_mode(
    mode: &ModeCompositionInput,
    authority: &ToolAuthority,
) -> Result<(), PromptCompositionError> {
    if mode.mode_id.trim().is_empty() {
        return Err(PromptCompositionError::InvalidMode);
    }
    if let Some(tool_id) = mode
        .visible_tool_ids
        .iter()
        .find(|tool_id| !authority.permits(tool_id))
    {
        return Err(PromptCompositionError::AuthorityNotGranted {
            layer: PromptLayerKind::ModeProfile.layer_id(),
            tool_id: tool_id.clone(),
        });
    }
    Ok(())
}

fn validate_contribution(
    contribution: &PromptContribution,
    authority: &ToolAuthority,
) -> Result<(), PromptCompositionError> {
    let PromptContribution {
        layer,
        source,
        origin_id,
        text,
        declared_tool_ids,
    } = contribution;

    if origin_id.trim().is_empty() || text.trim().is_empty() {
        return Err(PromptCompositionError::EmptyContribution {
            layer: layer.layer_id(),
        });
    }
    if text.len() > MAX_PROMPT_LAYER_BYTES {
        return Err(PromptCompositionError::ContributionTooLarge {
            layer: layer.layer_id(),
        });
    }
    if !layer.accepts(*source) {
        return Err(PromptCompositionError::SourceNotPermitted {
            layer: layer.layer_id(),
            layer_source: *source,
        });
    }
    if *source == LayerSource::WhisplyProduct
        && NATIVE_DISCOVERY_ORIGINS.contains(&origin_id.as_str())
    {
        return Err(PromptCompositionError::DuplicatesNativeDiscovery {
            origin_id: origin_id.clone(),
        });
    }
    if !declared_tool_ids.is_empty() && *layer != PromptLayerKind::ToolPolicyMetadata {
        return Err(PromptCompositionError::PromptCannotDeclareTools {
            layer: layer.layer_id(),
        });
    }
    if let Some(tool_id) = declared_tool_ids
        .iter()
        .find(|tool_id| !authority.permits(tool_id))
    {
        return Err(PromptCompositionError::AuthorityNotGranted {
            layer: layer.layer_id(),
            tool_id: tool_id.clone(),
        });
    }
    Ok(())
}

/// A prompt/profile revision record that never carries prompt text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptCompositionDiagnostics {
    pub contract_version: u16,
    pub profile: PromptProfile,
    pub mode_id: Option<String>,
    pub layers: Vec<PromptLayerDigest>,
}

/// One layer's revision-safe fingerprint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptLayerDigest {
    pub layer_id: String,
    pub precedence: u8,
    pub source: LayerSource,
    pub origin_id: String,
    pub byte_length: usize,
    pub text_sha256: String,
}

/// Why a composition was refused.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum PromptCompositionError {
    #[error("prompt composition exceeded its contribution ceiling")]
    TooManyContributions,
    #[error("mode input is not a usable profile/policy input")]
    InvalidMode,
    #[error("layer {layer} received an empty contribution")]
    EmptyContribution { layer: &'static str },
    #[error("layer {layer} exceeded its size ceiling")]
    ContributionTooLarge { layer: &'static str },
    #[error("layer {layer} does not accept a {layer_source:?} contribution")]
    SourceNotPermitted {
        layer: &'static str,
        layer_source: LayerSource,
    },
    #[error("required layer {layer} is missing")]
    MissingRequiredLayer { layer: &'static str },
    #[error("layer {layer} repeated origin {origin_id}")]
    DuplicateOrigin {
        layer: &'static str,
        origin_id: String,
    },
    #[error("layer {layer} repeated an identical contribution")]
    DuplicateContribution { layer: &'static str },
    #[error("Whisply cannot re-emit native discovery origin {origin_id}")]
    DuplicatesNativeDiscovery { origin_id: String },
    #[error("layer {layer} cannot declare tool authority")]
    PromptCannotDeclareTools { layer: &'static str },
    #[error("{layer_source:?} contributed {bytes} bytes against a {budget}-byte budget")]
    SourceBudgetExceeded {
        layer_source: LayerSource,
        bytes: usize,
        budget: usize,
    },
    #[error("layer {layer} named tool {tool_id}, which policy has not granted")]
    AuthorityNotGranted {
        layer: &'static str,
        tool_id: String,
    },
}

#[cfg(test)]
#[path = "prompt_composition_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "prompt_corpora_tests.rs"]
mod corpora_tests;

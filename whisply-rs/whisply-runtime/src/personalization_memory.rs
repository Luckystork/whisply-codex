//! The bounded, attributed personalization and memory prompt-layer boundary.
//!
//! Plan §14.1 layer 5 carries "user personalization and memory selected by
//! current Whisply policy". This module is that selection boundary. It exists
//! so account memory cannot quietly become an unbounded, unattributed, or
//! policy-ignoring prompt channel: every admitted item names where it came
//! from, the account's own controls decide which sources are eligible, and the
//! projection is size- and count-bounded before it reaches a composition.

use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeSet;
use thiserror::Error;

use crate::prompt_composition::LayerSource;
use crate::prompt_composition::PromptContribution;
use crate::prompt_composition::PromptLayerKind;

/// The most personalization items one turn may carry.
pub const MAX_PERSONALIZATION_ITEMS: usize = 16;

/// The most personalization text one turn may carry, in bytes.
pub const MAX_PERSONALIZATION_BYTES: usize = 4_096;

/// The stable origin recorded for the projected personalization layer.
pub const PERSONALIZATION_ORIGIN_ID: &str = "account-memory";

/// Where one personalization item came from.
///
/// There is deliberately no `Unattributed` variant: an item that cannot say
/// where it came from cannot enter the prompt.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryAttribution {
    /// The user stated this directly to Whisply.
    UserStated,
    /// An account-scoped personalization setting.
    AccountProfile,
    /// Derived from prior chat history.
    ChatHistory,
    /// Derived from a saved session.
    SavedSession,
    /// Supplied by a connected app.
    ConnectedApp,
}

impl MemoryAttribution {
    /// The stable wire identifier used by fixtures and diagnostics.
    pub const fn attribution_id(self) -> &'static str {
        match self {
            Self::UserStated => "user_stated",
            Self::AccountProfile => "account_profile",
            Self::ChatHistory => "chat_history",
            Self::SavedSession => "saved_session",
            Self::ConnectedApp => "connected_app",
        }
    }
}

/// The account's current memory controls.
///
/// These mirror the account projection's memory settings. `learn_automatically`
/// is intentionally present but never consulted here: it governs whether new
/// memory is *captured*, and a capture control must not silently become an
/// injection control.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryControls {
    pub enabled: bool,
    pub learn_automatically: bool,
    pub reference_chat_history: bool,
    pub reference_saved_sessions: bool,
    pub reference_connected_apps: bool,
}

impl MemoryControls {
    /// Whether the account's current controls admit this attribution.
    pub const fn admits(&self, attribution: MemoryAttribution) -> bool {
        if !self.enabled {
            return false;
        }
        match attribution {
            MemoryAttribution::UserStated | MemoryAttribution::AccountProfile => true,
            MemoryAttribution::ChatHistory => self.reference_chat_history,
            MemoryAttribution::SavedSession => self.reference_saved_sessions,
            MemoryAttribution::ConnectedApp => self.reference_connected_apps,
        }
    }
}

/// One attributed personalization item eligible for the prompt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttributedMemory {
    pub memory_id: String,
    pub attribution: MemoryAttribution,
    pub text: String,
}

/// Selects the admitted, bounded, attributed items and projects them as one
/// non-authoritative personalization layer contribution.
///
/// Returns `Ok(None)` when the account's controls admit nothing, so the caller
/// omits layer 5 entirely rather than injecting an empty section.
pub fn personalization_contribution(
    controls: &MemoryControls,
    items: &[AttributedMemory],
) -> Result<Option<PromptContribution>, PersonalizationError> {
    let mut seen_ids = BTreeSet::new();
    let mut admitted = Vec::new();
    for item in items {
        if item.memory_id.trim().is_empty() || item.text.trim().is_empty() {
            return Err(PersonalizationError::UnusableItem);
        }
        if !seen_ids.insert(item.memory_id.as_str()) {
            return Err(PersonalizationError::DuplicateMemoryId {
                memory_id: item.memory_id.clone(),
            });
        }
        if controls.admits(item.attribution) {
            admitted.push(item);
        }
    }

    if admitted.is_empty() {
        return Ok(None);
    }
    if admitted.len() > MAX_PERSONALIZATION_ITEMS {
        return Err(PersonalizationError::TooManyItems {
            admitted: admitted.len(),
        });
    }

    let text = admitted
        .iter()
        .map(|item| format!("- ({}) {}", item.attribution.attribution_id(), item.text))
        .collect::<Vec<_>>()
        .join("\n");
    if text.len() > MAX_PERSONALIZATION_BYTES {
        return Err(PersonalizationError::ProjectionTooLarge { bytes: text.len() });
    }

    Ok(Some(PromptContribution {
        layer: PromptLayerKind::PersonalizationMemory,
        source: LayerSource::WhisplyProduct,
        origin_id: PERSONALIZATION_ORIGIN_ID.to_string(),
        text,
        declared_tool_ids: Vec::new(),
    }))
}

/// Why personalization could not be projected.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum PersonalizationError {
    #[error("a personalization item is missing its identifier or text")]
    UnusableItem,
    #[error("personalization item {memory_id} was supplied twice")]
    DuplicateMemoryId { memory_id: String },
    #[error("{admitted} admitted personalization items exceed the per-turn ceiling")]
    TooManyItems { admitted: usize },
    #[error("the personalization projection is {bytes} bytes, over its ceiling")]
    ProjectionTooLarge { bytes: usize },
}

#[cfg(test)]
#[path = "personalization_memory_tests.rs"]
mod tests;

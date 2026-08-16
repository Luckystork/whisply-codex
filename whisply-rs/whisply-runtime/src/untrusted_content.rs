//! Channels whose content Whisply must treat as untrusted observation.
//!
//! Files, fetched sites, connector records, MCP server output, and skill
//! bodies all reach a turn as data. None of them is an instruction channel and
//! none may name a tool. This module wraps that content so it is structurally
//! incapable of carrying authority into a prompt composition: the wrapper
//! always produces a contribution with no declared tools, on the layer its
//! channel is allowed to inform.

use serde::Deserialize;
use serde::Serialize;

use crate::prompt_composition::LayerSource;
use crate::prompt_composition::PromptContribution;
use crate::prompt_composition::PromptLayerKind;

/// A channel that can carry content originating outside Whisply's authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UntrustedChannel {
    File,
    Site,
    Connector,
    McpServer,
    Skill,
}

impl UntrustedChannel {
    /// Every channel the unified injection matrix must cover.
    pub const ALL: [Self; 5] = [
        Self::File,
        Self::Site,
        Self::Connector,
        Self::McpServer,
        Self::Skill,
    ];

    /// The stable wire identifier used by fixtures and diagnostics.
    pub const fn channel_id(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Site => "site",
            Self::Connector => "connector",
            Self::McpServer => "mcp_server",
            Self::Skill => "skill",
        }
    }

    /// The layer this channel's content may inform but never author.
    ///
    /// Skill bodies arrive through Codex's own progressive skill loading, so
    /// they land on the runtime-owned skills layer. Everything else reaches the
    /// turn as an attachment beside the user's request.
    pub const fn observation_layer(self) -> PromptLayerKind {
        match self {
            Self::File | Self::Site | Self::Connector | Self::McpServer => {
                PromptLayerKind::UserRequest
            }
            Self::Skill => PromptLayerKind::ActiveSkills,
        }
    }

    /// Which surface delivers this channel's content.
    pub const fn source(self) -> LayerSource {
        match self {
            Self::File | Self::Site | Self::Connector | Self::McpServer => {
                LayerSource::WhisplyProduct
            }
            Self::Skill => LayerSource::NativeRuntime,
        }
    }

    /// Wraps channel content as a non-authoritative observation.
    ///
    /// The returned contribution never declares a tool, whatever the content
    /// claims, so a composition cannot be talked into granting authority.
    pub fn observation(
        self,
        origin_id: impl Into<String>,
        content: impl Into<String>,
    ) -> PromptContribution {
        PromptContribution {
            layer: self.observation_layer(),
            source: self.source(),
            origin_id: format!("{}:{}", self.channel_id(), origin_id.into()),
            text: content.into(),
            declared_tool_ids: Vec::new(),
        }
    }
}

//! What a person is told about a plugin before they install it.
//!
//! Two facts were missing from every pre-install surface, and they are the two
//! that decide whether installing is safe.
//!
//! The first is what the plugin would actually start. A declared MCP server was
//! reported by name, and a name is not an offer: `notes` reads the same whether
//! it runs `node server.js` or `sh -c "curl … | sh"`, and both start a process
//! outside the sandbox with the person's own access.
//!
//! The second is what happens afterwards. Installing is not a decision about
//! the code in front of you; it is a decision about every version that arrives
//! later, and those arrive unannounced -- Whisply syncs the curated catalog at
//! startup, upgrades configured Git marketplaces at startup and reinstalls the
//! plugins configured from them, and re-reads an installed tree on every load.
//! Two things settle what that means, decided separately, so a person needs
//! both: how a new version reaches the machine, and whether it runs when it
//! gets there. The second follows from the package pin -- a plugin whose files
//! a person approved stops until they approve the new ones, and a plugin the
//! product installed on their behalf is re-recorded and runs.

use crate::OPENAI_API_CURATED_MARKETPLACE_NAME;
use crate::OPENAI_CURATED_MARKETPLACE_NAME;
use crate::remote::REMOTE_GLOBAL_MARKETPLACE_NAME;
use whisply_config::types::McpServerConfig;
use whisply_config::types::McpServerTransportConfig;
use whisply_protocol::protocol::HookEventName;

/// One of the MCP servers a plugin declares, written out as the thing it would
/// actually start.
///
/// A name is not a disclosure. `notes` says nothing; `node server.js` and
/// `sh -c "curl … | sh"` are both called `notes` and are not the same offer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginMcpLaunchDisclosure {
    pub server_name: String,
    /// The command line, working directory, and named environment, or the
    /// endpoint for an HTTP server.
    pub launch: String,
}

/// The event name as the product already spells it on the wire.
///
/// Derived from the same serde representation the protocol uses rather than a
/// second hand-written table, so a renamed event cannot start reading one way
/// in a disclosure and another way everywhere else.
pub fn plugin_hook_event_label(event_name: HookEventName) -> String {
    serde_json::to_value(event_name)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| format!("{event_name:?}"))
}

/// What a server would start, in one line.
///
/// Environment variables are named but never valued: the point is to show what
/// the server is reaching for, and printing a secret to disclose that it will
/// be shared is self-defeating.
pub fn plugin_mcp_launch_summary(config: &McpServerConfig) -> String {
    match &config.transport {
        McpServerTransportConfig::Stdio {
            command,
            args,
            env,
            env_vars,
            cwd,
        } => {
            let mut summary = std::iter::once(command.clone())
                .chain(args.iter().cloned())
                .collect::<Vec<_>>()
                .join(" ");
            if let Some(cwd) = cwd {
                summary.push_str(&format!(" (in {cwd})"));
            }
            let mut names = env
                .iter()
                .flat_map(|env| env.keys().cloned())
                .chain(env_vars.iter().map(|env_var| env_var.name().to_string()))
                .collect::<Vec<_>>();
            names.sort();
            names.dedup();
            if !names.is_empty() {
                summary.push_str(&format!(" (environment: {})", names.join(", ")));
            }
            summary
        }
        McpServerTransportConfig::StreamableHttp { url, .. } => format!("HTTP {url}"),
    }
}

/// How a new version of a plugin reaches this machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginUpdateArrival {
    /// Whisply keeps this catalog in step by itself: it syncs at startup and
    /// installs what it finds.
    Managed,
    /// Whisply upgrades the Git marketplace this came from at startup, and
    /// reinstalls the plugin when the marketplace moves.
    MarketplaceUpgrade,
    /// The cached copy is what runs. It changes only when a refresh is asked
    /// for, so editing the place it was installed from does nothing on its own.
    OnRequest,
}

/// Whether a new version runs when it arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginUpdateReview {
    /// Nobody approved these files in the first place, so a new version is
    /// recorded as installed the same way and runs.
    RunsOnArrival,
    /// A person approved these files. A new version no longer matches what they
    /// approved, so the plugin's MCP servers and hooks stop until they look
    /// again.
    StopsUntilApproved,
}

/// What installing this plugin signs up for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PluginUpdateBehavior {
    pub arrival: PluginUpdateArrival,
    pub review: PluginUpdateReview,
}

impl PluginUpdateBehavior {
    /// Read off the marketplace, because that is what decides both halves: the
    /// curated and account catalogs are installed for the person and re-pinned
    /// for them, and everything else is installed by them and pinned to what
    /// they saw.
    pub fn for_marketplace(marketplace_name: &str, marketplace_is_git: bool) -> Self {
        if is_product_managed_marketplace(marketplace_name) {
            return Self {
                arrival: PluginUpdateArrival::Managed,
                review: PluginUpdateReview::RunsOnArrival,
            };
        }
        Self {
            arrival: if marketplace_is_git {
                PluginUpdateArrival::MarketplaceUpgrade
            } else {
                PluginUpdateArrival::OnRequest
            },
            review: PluginUpdateReview::StopsUntilApproved,
        }
    }

    /// One sentence a person can act on, rather than two enum names.
    pub fn describe(self) -> String {
        let arrival = match self.arrival {
            PluginUpdateArrival::Managed => {
                "Whisply updates this plugin itself, in the background, without asking"
            }
            PluginUpdateArrival::MarketplaceUpgrade => {
                "this plugin changes when Whisply upgrades its marketplace, which happens \
                 in the background at startup"
            }
            PluginUpdateArrival::OnRequest => {
                "this plugin changes only when you refresh its marketplace"
            }
        };
        let review = match self.review {
            PluginUpdateReview::RunsOnArrival => "a new version runs as soon as it arrives",
            PluginUpdateReview::StopsUntilApproved => {
                "a new version stops its MCP servers and hooks until you approve it"
            }
        };
        format!("{arrival}, and {review}.")
    }
}

/// Catalogs Whisply installs from on the person's behalf. These are the ones
/// whose packages are measured at install rather than approved, which is why a
/// new version of one runs without being reviewed.
fn is_product_managed_marketplace(marketplace_name: &str) -> bool {
    matches!(
        marketplace_name,
        OPENAI_CURATED_MARKETPLACE_NAME
            | OPENAI_API_CURATED_MARKETPLACE_NAME
            | REMOTE_GLOBAL_MARKETPLACE_NAME
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_catalog_whisply_syncs_updates_itself_and_runs_what_arrives() {
        for marketplace in [
            OPENAI_CURATED_MARKETPLACE_NAME,
            OPENAI_API_CURATED_MARKETPLACE_NAME,
            REMOTE_GLOBAL_MARKETPLACE_NAME,
        ] {
            let behavior = PluginUpdateBehavior::for_marketplace(
                marketplace,
                // A managed catalog is managed whichever way it is configured.
                /*marketplace_is_git*/
                true,
            );
            assert_eq!(
                behavior,
                PluginUpdateBehavior {
                    arrival: PluginUpdateArrival::Managed,
                    review: PluginUpdateReview::RunsOnArrival,
                },
                "{marketplace} is installed for the person, so it is re-recorded for them too"
            );
        }
    }

    /// The distinction that matters to someone installing from their own
    /// marketplace: it updates in the background, but it does not start.
    #[test]
    fn a_git_marketplace_updates_in_the_background_but_waits_to_be_approved() {
        let behavior =
            PluginUpdateBehavior::for_marketplace("debug", /*marketplace_is_git*/ true);
        assert_eq!(behavior.arrival, PluginUpdateArrival::MarketplaceUpgrade);
        assert_eq!(behavior.review, PluginUpdateReview::StopsUntilApproved);
    }

    #[test]
    fn a_marketplace_whisply_does_not_upgrade_changes_only_when_asked() {
        let behavior =
            PluginUpdateBehavior::for_marketplace("debug", /*marketplace_is_git*/ false);
        assert_eq!(behavior.arrival, PluginUpdateArrival::OnRequest);
        assert_eq!(behavior.review, PluginUpdateReview::StopsUntilApproved);
    }

    /// The sentence is the whole point of the type; an enum name in a terminal
    /// is not a disclosure.
    #[test]
    fn every_combination_reads_as_a_sentence() {
        for (marketplace, is_git, expected) in [
            (
                OPENAI_CURATED_MARKETPLACE_NAME,
                false,
                "Whisply updates this plugin itself, in the background, without asking, and a \
                 new version runs as soon as it arrives.",
            ),
            (
                "debug",
                true,
                "this plugin changes when Whisply upgrades its marketplace, which happens in \
                 the background at startup, and a new version stops its MCP servers and hooks \
                 until you approve it.",
            ),
            (
                "debug",
                false,
                "this plugin changes only when you refresh its marketplace, and a new version \
                 stops its MCP servers and hooks until you approve it.",
            ),
        ] {
            assert_eq!(
                PluginUpdateBehavior::for_marketplace(marketplace, is_git).describe(),
                expected
            );
        }
    }
}

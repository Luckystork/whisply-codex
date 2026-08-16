use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use clap::Parser;
use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use tracing::warn;
use whisply_config::McpServerConfig;
use whisply_config::PluginMcpLaunchTrust;
use whisply_config::types::PluginPackageOrigin;
use whisply_core::config::Config;
use whisply_core::config::find_codex_home;
use whisply_core_plugins::ConfiguredMarketplace;
use whisply_core_plugins::OPENAI_BUNDLED_MARKETPLACE_NAME;
use whisply_core_plugins::PluginDetail;
use whisply_core_plugins::PluginHookSummary;
use whisply_core_plugins::PluginInstallOutcome;
use whisply_core_plugins::PluginInstallRequest;
use whisply_core_plugins::PluginLaunchReview;
use whisply_core_plugins::PluginMcpLaunchApproval;
use whisply_core_plugins::PluginReadRequest;
use whisply_core_plugins::PluginsConfigInput;
use whisply_core_plugins::PluginsManager;
use whisply_core_plugins::allowed_configured_marketplace_names;
use whisply_core_plugins::installed_marketplaces::marketplace_install_root;
use whisply_core_plugins::installed_marketplaces::resolve_configured_marketplace_root;
use whisply_core_plugins::marketplace::MarketplaceListError;
use whisply_core_plugins::marketplace::MarketplacePluginAuthPolicy;
use whisply_core_plugins::marketplace::MarketplacePluginInstallPolicy;
use whisply_core_plugins::marketplace::MarketplacePluginSource;
use whisply_core_plugins::marketplace::find_marketplace_manifest_path;
use whisply_core_plugins::plugin_disclosure::plugin_hook_event_label;
use whisply_core_plugins::plugin_disclosure::plugin_mcp_launch_summary;
use whisply_core_plugins::plugin_package_pin::PluginPackageTrust;
use whisply_login::AuthManager;
use whisply_plugin::PluginId;
use whisply_plugin::validate_plugin_segment;
use whisply_protocol::auth::AuthMode;
use whisply_utils_absolute_path::AbsolutePathBuf;
use whisply_utils_cli::CliConfigOverrides;

use crate::marketplace_cmd::MarketplaceCli;

const OPENAI_BUNDLED_ALPHA_MARKETPLACE_NAME: &str = "openai-bundled-alpha";
const OPENAI_PRIMARY_RUNTIME_MARKETPLACE_NAME: &str = "openai-primary-runtime";

#[derive(Debug, Parser)]
#[command(bin_name = "whisply plugin")]
pub struct PluginCli {
    #[clap(flatten)]
    pub config_overrides: CliConfigOverrides,

    #[command(subcommand)]
    pub subcommand: PluginSubcommand,
}

#[derive(Debug, clap::Subcommand)]
pub enum PluginSubcommand {
    /// Install a plugin from a configured marketplace snapshot.
    ///
    /// Pass either `PLUGIN@MARKETPLACE` or pass `PLUGIN` with
    /// `--marketplace MARKETPLACE`.
    Add(AddPluginArgs),

    /// List plugins available from configured marketplace snapshots.
    List(ListPluginsArgs),

    /// Add, list, upgrade, or remove configured plugin marketplaces.
    Marketplace(MarketplaceCli),

    /// Remove an installed plugin from local config and cache.
    ///
    /// Pass either `PLUGIN@MARKETPLACE` or pass `PLUGIN` with
    /// `--marketplace MARKETPLACE`.
    Remove(RemovePluginArgs),

    /// Approve the MCP servers an installed plugin launches right now.
    ///
    /// Whisply records what a plugin's MCP servers launch when the plugin is
    /// installed, and refuses to start one whose definition changed since. This
    /// prints the current definitions and makes them the approved ones.
    Trust(TrustPluginArgs),
}

#[derive(Debug, Parser)]
#[command(
    bin_name = "whisply plugin add",
    after_help = "Examples:\n  whisply plugin add sample@debug\n  whisply plugin add sample --marketplace debug"
)]
pub struct AddPluginArgs {
    /// Plugin selector to install: either PLUGIN@MARKETPLACE or PLUGIN with --marketplace.
    #[arg(value_name = "PLUGIN[@MARKETPLACE]")]
    plugin: String,

    /// Configured marketplace name to use when PLUGIN does not include @MARKETPLACE.
    #[arg(long = "marketplace", short = 'm', value_name = "MARKETPLACE")]
    marketplace_name: Option<String>,

    /// Output the disclosure and install result as JSON.
    #[arg(long = "json")]
    json: bool,

    /// Install what is described without asking.
    #[arg(long = "yes", short = 'y')]
    yes: bool,
}

#[derive(Debug, Parser)]
#[command(
    bin_name = "whisply plugin list",
    after_help = "Examples:\n  whisply plugin list\n  whisply plugin list --marketplace debug\n  whisply plugin list --json\n  whisply plugin list --available --json"
)]
pub struct ListPluginsArgs {
    /// Only list plugins from this configured marketplace name.
    #[arg(long = "marketplace", short = 'm', value_name = "MARKETPLACE")]
    marketplace_name: Option<String>,

    /// Output plugin list as JSON.
    #[arg(long = "json")]
    json: bool,

    /// Include uninstalled marketplace plugins in the JSON output.
    #[arg(long = "available", requires = "json")]
    available: bool,
}

#[derive(Debug, Parser)]
#[command(
    bin_name = "whisply plugin remove",
    after_help = "Examples:\n  whisply plugin remove sample@debug\n  whisply plugin remove sample --marketplace debug"
)]
pub struct RemovePluginArgs {
    /// Plugin selector to remove: either PLUGIN@MARKETPLACE or PLUGIN with --marketplace.
    #[arg(value_name = "PLUGIN[@MARKETPLACE]")]
    plugin: String,

    /// Marketplace name to use when PLUGIN does not include @MARKETPLACE.
    #[arg(long = "marketplace", short = 'm', value_name = "MARKETPLACE")]
    marketplace_name: Option<String>,

    /// Output remove result as JSON.
    #[arg(long = "json")]
    json: bool,
}

#[derive(Debug, Parser)]
#[command(
    bin_name = "whisply plugin trust",
    after_help = "Examples:\n  whisply plugin trust sample@debug\n  whisply plugin trust sample --marketplace debug --json\n  whisply plugin trust sample@debug --yes"
)]
pub struct TrustPluginArgs {
    /// Plugin selector to trust: either PLUGIN@MARKETPLACE or PLUGIN with --marketplace.
    #[arg(value_name = "PLUGIN[@MARKETPLACE]")]
    plugin: String,

    /// Marketplace name to use when PLUGIN does not include @MARKETPLACE.
    #[arg(long = "marketplace", short = 'm', value_name = "MARKETPLACE")]
    marketplace_name: Option<String>,

    /// Report the launch definitions as JSON instead of prose.
    #[arg(long = "json")]
    json: bool,

    /// Approve what is shown without asking. Required when there is no
    /// terminal to ask at.
    #[arg(long = "yes", short = 'y')]
    yes: bool,
}

pub async fn run_plugin_add(
    overrides: Vec<(String, toml::Value)>,
    args: AddPluginArgs,
) -> Result<()> {
    let PluginCommandContext {
        codex_home,
        plugins_input,
        manager,
    } = load_plugin_command_context(overrides).await?;
    let AddPluginArgs {
        plugin,
        marketplace_name,
        json,
        yes,
    } = args;
    let PluginSelection {
        plugin_name,
        marketplace_name,
        ..
    } = parse_plugin_selection(plugin, marketplace_name)?;
    let marketplace = find_marketplace_for_plugin(
        &manager,
        codex_home.as_path(),
        &plugins_input,
        &marketplace_name,
        &plugin_name,
    )?;

    // Read the plugin before installing it. Installing is what records its
    // command lines as approved, so anything the person is supposed to have
    // agreed to has to be in front of them by now, not printed afterwards.
    let disclosure = read_pre_install_disclosure(
        &manager,
        &plugins_input,
        &marketplace.path,
        &plugin_name,
        &marketplace_name,
    )
    .await;

    if !json {
        print_pre_install_disclosure(&plugin_name, &marketplace_name, disclosure.as_ref());
        if !yes && !confirm_install()? {
            println!("Nothing was installed.");
            return Ok(());
        }
    }

    let outcome = manager
        .install_plugin(
            &plugins_input.config_layer_stack,
            PluginInstallRequest {
                plugin_name,
                marketplace_path: marketplace.path,
            },
        )
        .await?;

    if json {
        // A machine consumer is not asked, so the same facts have to travel
        // with the result instead of scrolling past in a prompt.
        let output = JsonPluginAddOutput::from_outcome(outcome, disclosure.as_ref());
        println!("{}", serde_json::to_string_pretty(&output)?);
        return Ok(());
    }

    println!(
        "Added plugin `{}` from marketplace `{}`.",
        outcome.plugin_id.plugin_name, outcome.plugin_id.marketplace_name
    );
    println!(
        "Installed plugin root: {}",
        outcome.installed_path.as_path().display()
    );

    Ok(())
}

/// Everything the person is deciding about, read from the marketplace copy
/// before anything is written.
///
/// A read can fail for reasons that do not stop an install -- a remote source
/// that only materializes at install time is the ordinary one -- so this
/// returns nothing rather than refusing, and the caller says so plainly instead
/// of implying there was nothing to declare.
async fn read_pre_install_disclosure(
    manager: &PluginsManager,
    plugins_input: &PluginsConfigInput,
    marketplace_path: &AbsolutePathBuf,
    plugin_name: &str,
    marketplace_name: &str,
) -> Option<PluginDetail> {
    match manager
        .read_plugin_for_config(
            plugins_input,
            &PluginReadRequest {
                marketplace_path: marketplace_path.clone(),
                plugin_name: plugin_name.to_string(),
            },
        )
        .await
    {
        Ok(outcome) => Some(outcome.plugin),
        Err(err) => {
            warn!(
                plugin = plugin_name,
                marketplace = marketplace_name,
                error = %err,
                "failed to read plugin details before install"
            );
            None
        }
    }
}

/// Say where it came from, what it can do, what of the person's it wants, and
/// what happens the next time it changes.
///
/// Nothing contains these processes. They run with the person's own access, and
/// the only honest thing to do is name them and say so.
fn print_pre_install_disclosure(
    plugin_name: &str,
    marketplace_name: &str,
    detail: Option<&PluginDetail>,
) {
    println!("Installing `{plugin_name}` from marketplace `{marketplace_name}`.");
    let Some(detail) = detail else {
        println!(
            "Whisply could not read what this plugin declares before installing it, so what it \
             contains is unknown. Read it with `whisply plugin trust {plugin_name}@{marketplace_name}` \
             after installing, and remove it with `whisply plugin remove \
             {plugin_name}@{marketplace_name}` if it is not what you expected."
        );
        return;
    };

    println!("  Origin: {}", origin_label(&detail.source));

    let capabilities = capability_labels(detail);
    if capabilities.is_empty() {
        println!("  Capabilities: none declared.");
    } else {
        println!("  Capabilities: {}.", capabilities.join(", "));
    }

    if detail.mcp_launches.is_empty() {
        println!("  Starts: nothing outside the sandbox.");
    } else {
        println!("  Starts outside the sandbox, with your access:");
        for launch in &detail.mcp_launches {
            println!("    • {}: {}", launch.server_name, launch.launch);
        }
    }

    // Hooks are the part nobody goes looking for: they run on their event with
    // no prompt at all, so installing is the only moment they are visible.
    for hook in &detail.hooks {
        println!(
            "  Runs on {} without asking: {}",
            plugin_hook_event_label(hook.event_name),
            hook_action_label(hook)
        );
    }

    // Named, never valued: the point is to show what it is reaching for, and a
    // terminal keeps scrollback.
    if detail.host_env_usage.forwarded.is_empty() {
        println!("  Wants none of your environment.");
    } else {
        println!(
            "  Wants these from your environment: {}.",
            detail.host_env_usage.forwarded.join(", ")
        );
    }
    if !detail.host_env_usage.refused.is_empty() {
        println!(
            "  Asks for these and will not get them: {}.",
            detail.host_env_usage.refused.join(", ")
        );
    }

    println!("  Updates: {}", detail.update_behavior.describe());
}

/// A hook that runs no local command still runs; naming its handler keeps it
/// from disappearing from the list for looking harmless.
fn hook_action_label(hook: &PluginHookSummary) -> String {
    match &hook.command {
        Some(command) => command.clone(),
        None => format!("{:?} handler", hook.handler_type),
    }
}

/// What this plugin adds to Whisply, counted rather than listed, because the
/// number is the part that changes the decision.
fn capability_labels(detail: &PluginDetail) -> Vec<String> {
    let visible_skills = detail
        .skills
        .iter()
        .filter(|skill| {
            !detail
                .disabled_skill_paths
                .contains(&skill.path_to_skills_md)
        })
        .count();
    [
        (visible_skills, "skill"),
        (detail.hooks.len(), "hook"),
        (detail.mcp_launches.len(), "MCP server"),
        (detail.apps.len(), "connector"),
    ]
    .into_iter()
    .filter(|(count, _)| *count > 0)
    .map(|(count, noun)| {
        let plural = if count == 1 { "" } else { "s" };
        format!("{count} {noun}{plural}")
    })
    .collect()
}

fn origin_label(source: &MarketplacePluginSource) -> String {
    match source {
        MarketplacePluginSource::Local { path } => {
            format!("{} on this machine", path.as_path().display())
        }
        MarketplacePluginSource::Git {
            url,
            path,
            ref_name,
            sha,
        } => {
            let mut label = url.clone();
            if let Some(path) = path {
                label.push_str(&format!(" ({path})"));
            }
            // The exact commit is the only part of a Git origin that cannot be
            // moved under you, so it wins when both are known.
            match (sha, ref_name) {
                (Some(sha), _) => label.push_str(&format!(" at {sha}")),
                (None, Some(ref_name)) => label.push_str(&format!(" at {ref_name}")),
                (None, None) => {}
            }
            label
        }
        MarketplacePluginSource::Npm {
            package,
            version,
            registry,
        } => {
            let mut label = package.clone();
            if let Some(version) = version {
                label.push_str(&format!("@{version}"));
            }
            label.push_str(&format!(
                " from {}",
                registry.as_deref().unwrap_or("the default npm registry")
            ));
            label
        }
    }
}

/// Asking is the review. Without a terminal there is nobody to ask, and
/// refusing there would break every script that already installs plugins, so
/// the disclosure above stands on its own in that case.
fn confirm_install() -> Result<bool> {
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        return Ok(true);
    }
    print!("Install it? [y/N] ");
    std::io::Write::flush(&mut std::io::stdout())?;
    let mut answer = String::new();
    std::io::BufRead::read_line(&mut std::io::stdin().lock(), &mut answer)?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonPluginAddOutput {
    plugin_id: String,
    name: String,
    marketplace_name: String,
    version: String,
    installed_path: String,
    auth_policy: &'static str,
    /// Absent when the plugin could not be read before installing, which is not
    /// the same as a plugin that declares nothing.
    #[serde(skip_serializing_if = "Option::is_none")]
    disclosure: Option<JsonPluginDisclosure>,
}

/// The same four facts the prose says, for a caller that installs without a
/// person watching.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonPluginDisclosure {
    origin: JsonPluginSource,
    skills: usize,
    hooks: Vec<JsonPluginHook>,
    mcp_servers: Vec<JsonPluginMcpLaunch>,
    connectors: Vec<String>,
    host_env_vars: Vec<String>,
    refused_host_env_vars: Vec<String>,
    update_behavior: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonPluginHook {
    event_name: String,
    /// `None` for handlers that execute nothing locally. The hook still runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonPluginMcpLaunch {
    server_name: String,
    launch: String,
}

impl JsonPluginDisclosure {
    fn from_detail(detail: &PluginDetail) -> Self {
        Self {
            origin: JsonPluginSource::from_marketplace_source(detail.source.clone()),
            skills: detail
                .skills
                .iter()
                .filter(|skill| {
                    !detail
                        .disabled_skill_paths
                        .contains(&skill.path_to_skills_md)
                })
                .count(),
            hooks: detail
                .hooks
                .iter()
                .map(|hook| JsonPluginHook {
                    event_name: plugin_hook_event_label(hook.event_name),
                    command: hook.command.clone(),
                })
                .collect(),
            mcp_servers: detail
                .mcp_launches
                .iter()
                .map(|launch| JsonPluginMcpLaunch {
                    server_name: launch.server_name.clone(),
                    launch: launch.launch.clone(),
                })
                .collect(),
            connectors: detail
                .apps
                .iter()
                .map(|connector| connector.0.clone())
                .collect(),
            host_env_vars: detail.host_env_usage.forwarded.clone(),
            refused_host_env_vars: detail.host_env_usage.refused.clone(),
            update_behavior: detail.update_behavior.describe(),
        }
    }
}

impl JsonPluginAddOutput {
    fn from_outcome(outcome: PluginInstallOutcome, detail: Option<&PluginDetail>) -> Self {
        Self {
            plugin_id: outcome.plugin_id.as_key(),
            name: outcome.plugin_id.plugin_name,
            marketplace_name: outcome.plugin_id.marketplace_name,
            version: outcome.plugin_version,
            installed_path: outcome.installed_path.as_path().display().to_string(),
            auth_policy: auth_policy_label(outcome.auth_policy),
            disclosure: detail.map(JsonPluginDisclosure::from_detail),
        }
    }
}

pub async fn run_plugin_list(
    overrides: Vec<(String, toml::Value)>,
    args: ListPluginsArgs,
) -> Result<()> {
    let PluginCommandContext {
        codex_home,
        plugins_input,
        manager,
        ..
    } = load_plugin_command_context(overrides).await?;
    let outcome = manager
        .list_marketplaces_for_config(&plugins_input, &[], /*include_openai_curated*/ true)
        .context("failed to list marketplace plugins")?;
    ensure_configured_marketplace_snapshots_loaded(
        codex_home.as_path(),
        &plugins_input,
        &outcome.errors,
        /*marketplace_name*/ None,
    )?;

    let marketplaces = outcome
        .marketplaces
        .into_iter()
        .filter(|marketplace| {
            args.marketplace_name
                .as_ref()
                .is_none_or(|name| marketplace.name == *name)
        })
        .collect::<Vec<_>>();
    let marketplace_sources = configured_marketplace_sources(&plugins_input, codex_home.as_path());

    if args.json {
        let output = JsonPluginListOutput::from_marketplaces(
            marketplaces,
            args.available,
            &marketplace_sources,
        );
        println!("{}", serde_json::to_string_pretty(&output)?);
        return Ok(());
    }

    if marketplaces.is_empty() {
        if let Some(marketplace_name) = args.marketplace_name {
            println!("No plugins found in marketplace `{marketplace_name}`.");
        } else {
            println!("No marketplace plugins found.");
        }
    } else {
        for (index, marketplace) in marketplaces.into_iter().enumerate() {
            let mut rows = Vec::new();
            let mut plugin_width = "PLUGIN".len();
            let mut status_width = "STATUS".len();
            let mut installed_version_width = "VERSION".len();
            let mut path_width = "PATH".len();

            for plugin in &marketplace.plugins {
                let state = if plugin.installed && plugin.enabled {
                    "installed, enabled"
                } else if plugin.installed {
                    "installed, disabled"
                } else {
                    "not installed"
                };
                let installed_version = plugin.installed_version.clone().unwrap_or_default();
                let path = match &plugin.source {
                    whisply_core_plugins::marketplace::MarketplacePluginSource::Local { path } => {
                        path.as_path().display().to_string()
                    }
                    whisply_core_plugins::marketplace::MarketplacePluginSource::Git {
                        url,
                        path,
                        ref_name,
                        sha,
                    } => {
                        let mut parts = vec![url.clone()];
                        if let Some(path) = path {
                            parts.push(format!("path `{path}`"));
                        }
                        if let Some(ref_name) = ref_name {
                            parts.push(format!("ref `{ref_name}`"));
                        }
                        if let Some(sha) = sha {
                            parts.push(format!("sha `{sha}`"));
                        }
                        parts.join(", ")
                    }
                    whisply_core_plugins::marketplace::MarketplacePluginSource::Npm {
                        package,
                        version,
                        registry,
                    } => {
                        let mut parts = vec![package.clone()];
                        if let Some(version) = version {
                            parts.push(format!("version `{version}`"));
                        }
                        if let Some(registry) = registry {
                            parts.push(format!("registry `{registry}`"));
                        }
                        parts.join(", ")
                    }
                };
                plugin_width = plugin_width.max(plugin.id.len());
                status_width = status_width.max(state.len());
                installed_version_width = installed_version_width.max(installed_version.len());
                path_width = path_width.max(path.len());
                rows.push((plugin.id.clone(), state, installed_version, path));
            }

            if index > 0 {
                println!();
            }
            println!("Marketplace `{}`", marketplace.name);
            println!("{}", marketplace.path.as_path().display());
            println!();
            println!(
                "{:<plugin_width$}  {:<status_width$}  {:<installed_version_width$}  {:<path_width$}",
                "PLUGIN", "STATUS", "VERSION", "PATH"
            );
            for (plugin, status, installed_version, path) in rows {
                println!(
                    "{plugin:<plugin_width$}  {status:<status_width$}  {installed_version:<installed_version_width$}  {path:<path_width$}"
                );
            }
        }
    }

    Ok(())
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonPluginListOutput {
    installed: Vec<JsonPluginListEntry>,
    available: Vec<JsonPluginListEntry>,
}

impl JsonPluginListOutput {
    fn from_marketplaces(
        marketplaces: Vec<whisply_core_plugins::ConfiguredMarketplace>,
        include_available: bool,
        marketplace_sources: &HashMap<String, JsonMarketplaceSource>,
    ) -> Self {
        let mut installed = Vec::new();
        let mut available = Vec::new();

        for marketplace in marketplaces {
            let marketplace_source = marketplace_sources.get(&marketplace.name).cloned();
            for plugin in marketplace.plugins {
                let entry = JsonPluginListEntry::from_configured_plugin(
                    &marketplace.name,
                    marketplace_source.clone(),
                    plugin,
                );
                if entry.installed {
                    installed.push(entry);
                } else if include_available {
                    available.push(entry);
                }
            }
        }

        Self {
            installed,
            available,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonPluginListEntry {
    plugin_id: String,
    name: String,
    marketplace_name: String,
    version: Option<String>,
    installed: bool,
    enabled: bool,
    source: JsonPluginSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    marketplace_source: Option<JsonMarketplaceSource>,
    install_policy: &'static str,
    auth_policy: &'static str,
}

impl JsonPluginListEntry {
    fn from_configured_plugin(
        marketplace_name: &str,
        marketplace_source: Option<JsonMarketplaceSource>,
        plugin: whisply_core_plugins::ConfiguredMarketplacePlugin,
    ) -> Self {
        let version = plugin.installed_version.or(plugin.local_version);
        Self {
            plugin_id: plugin.id,
            name: plugin.name,
            marketplace_name: marketplace_name.to_string(),
            version,
            installed: plugin.installed,
            enabled: plugin.enabled,
            source: JsonPluginSource::from_marketplace_source(plugin.source),
            marketplace_source,
            install_policy: install_policy_label(plugin.policy.installation),
            auth_policy: auth_policy_label(plugin.policy.authentication),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(tag = "source", rename_all = "kebab-case")]
enum JsonPluginSource {
    Local {
        path: String,
    },
    Git {
        url: String,
        #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
        ref_name: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        sha: Option<String>,
    },
    GitSubdir {
        url: String,
        path: String,
        #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
        ref_name: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        sha: Option<String>,
    },
    Npm {
        package: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        version: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        registry: Option<String>,
    },
}

impl JsonPluginSource {
    fn from_marketplace_source(source: MarketplacePluginSource) -> Self {
        match source {
            MarketplacePluginSource::Local { path } => Self::Local {
                path: path.as_path().display().to_string(),
            },
            MarketplacePluginSource::Git {
                url,
                path: Some(path),
                ref_name,
                sha,
            } => Self::GitSubdir {
                url,
                path,
                ref_name,
                sha,
            },
            MarketplacePluginSource::Git {
                url,
                path: None,
                ref_name,
                sha,
            } => Self::Git { url, ref_name, sha },
            MarketplacePluginSource::Npm {
                package,
                version,
                registry,
            } => Self::Npm {
                package,
                version,
                registry,
            },
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct JsonMarketplaceSource {
    source_type: String,
    source: String,
}

pub(crate) fn configured_marketplace_sources(
    plugins_input: &PluginsConfigInput,
    codex_home: &Path,
) -> HashMap<String, JsonMarketplaceSource> {
    let Some(user_config) = plugins_input.config_layer_stack.effective_user_config() else {
        return HashMap::new();
    };
    let Some(marketplaces) = user_config
        .get("marketplaces")
        .and_then(toml::Value::as_table)
    else {
        return HashMap::new();
    };
    let allowed_marketplace_names =
        allowed_configured_marketplace_names(&plugins_input.config_layer_stack, codex_home);

    marketplaces
        .iter()
        .filter(|(marketplace_name, _)| allowed_marketplace_names.contains(*marketplace_name))
        .filter_map(|(marketplace_name, marketplace)| {
            let source_type = marketplace
                .get("source_type")
                .and_then(toml::Value::as_str)?;
            let source = marketplace.get("source").and_then(toml::Value::as_str)?;
            Some((
                marketplace_name.clone(),
                JsonMarketplaceSource {
                    source_type: source_type.to_string(),
                    source: source.to_string(),
                },
            ))
        })
        .collect()
}

fn install_policy_label(policy: MarketplacePluginInstallPolicy) -> &'static str {
    match policy {
        MarketplacePluginInstallPolicy::NotAvailable => "NOT_AVAILABLE",
        MarketplacePluginInstallPolicy::Available => "AVAILABLE",
        MarketplacePluginInstallPolicy::InstalledByDefault => "INSTALLED_BY_DEFAULT",
    }
}

fn auth_policy_label(policy: MarketplacePluginAuthPolicy) -> &'static str {
    match policy {
        MarketplacePluginAuthPolicy::OnInstall => "ON_INSTALL",
        MarketplacePluginAuthPolicy::OnUse => "ON_USE",
    }
}

pub async fn run_plugin_remove(
    overrides: Vec<(String, toml::Value)>,
    args: RemovePluginArgs,
) -> Result<()> {
    let PluginCommandContext { manager, .. } = load_plugin_command_context(overrides).await?;
    let RemovePluginArgs {
        plugin,
        marketplace_name,
        json,
    } = args;
    let selection = parse_plugin_selection(plugin, marketplace_name)?;

    manager
        .uninstall_plugin(selection.plugin_key.clone())
        .await?;
    if json {
        let output = JsonPluginRemoveOutput::from_selection(selection);
        println!("{}", serde_json::to_string_pretty(&output)?);
        return Ok(());
    }

    println!(
        "Removed plugin `{}` from marketplace `{}`.",
        selection.plugin_name, selection.marketplace_name
    );

    Ok(())
}

pub async fn run_plugin_trust(
    overrides: Vec<(String, toml::Value)>,
    args: TrustPluginArgs,
) -> Result<()> {
    let PluginCommandContext { manager, .. } = load_plugin_command_context(overrides).await?;
    let TrustPluginArgs {
        plugin,
        marketplace_name,
        json,
        yes,
    } = args;
    let selection = parse_plugin_selection(plugin, marketplace_name)?;
    let review = manager
        .review_plugin_mcp_launch_definitions(selection.plugin_key.clone())
        .await?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&JsonPluginTrustOutput::from_review(&review))?
        );
    } else {
        println!("{}", package_line(&selection.plugin_key, &review));
        if review.servers.is_empty() {
            println!(
                "Plugin `{}` declares no MCP servers, so there is nothing else to approve.",
                selection.plugin_key
            );
        } else {
            println!(
                "`{}` will start these outside the sandbox:",
                selection.plugin_key
            );
            for approval in &review.servers {
                println!(
                    "  • {} [{}]: {}",
                    approval.server_name,
                    trust_label(approval.trust),
                    launch_summary(&approval.config)
                );
            }
        }
    }

    // Nothing to read is nothing to decide, and that review is still worth
    // recording: it is how a server appearing later is known to be new.
    if !review.servers.is_empty() && !yes && !confirm_launch_approval()? {
        println!("Nothing was approved.");
        return Ok(());
    }

    manager
        .trust_plugin_mcp_launch_definitions(selection.plugin_key.clone(), &review)
        .await?;
    if !json && !review.servers.is_empty() {
        println!(
            "Approved. A later change to any of them, or to the files they run, is refused until it is approved here again."
        );
    }

    Ok(())
}

fn trust_label(trust: PluginMcpLaunchTrust) -> &'static str {
    match trust {
        PluginMcpLaunchTrust::Trusted => "unchanged",
        PluginMcpLaunchTrust::Changed => "changed since it was approved",
        PluginMcpLaunchTrust::Unpinned => "not approved yet",
    }
}

fn package_trust_label(trust: PluginPackageTrust, origin: PluginPackageOrigin) -> &'static str {
    let approved = matches!(origin, PluginPackageOrigin::Approved);
    match trust {
        PluginPackageTrust::Trusted if approved => "unchanged since you approved them",
        // Curated and remote plugins are measured when the product installs
        // them. Saying "unchanged" alone invites the reader to supply the wrong
        // second half of the sentence.
        PluginPackageTrust::Trusted => "unchanged since Whisply installed them",
        PluginPackageTrust::Changed if approved => "changed since you approved them",
        PluginPackageTrust::Changed => "changed since Whisply installed them",
        PluginPackageTrust::Unreadable => "cannot be read",
        PluginPackageTrust::Unpinned => "not recorded yet",
    }
}

/// The files come first, because a command line that has not changed can still
/// start a program that has.
fn package_line(plugin_key: &str, review: &PluginLaunchReview) -> String {
    format!(
        "Files installed by `{plugin_key}`: {}.",
        package_trust_label(review.package_trust, review.package_origin)
    )
}

/// Asking is the review. Without a terminal to ask at, `--yes` has to say
/// explicitly that the person already looked.
fn confirm_launch_approval() -> Result<bool> {
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        bail!(
            "no terminal to confirm at; re-run with --yes once you have read the definitions above"
        );
    }
    print!("Approve these? [y/N] ");
    std::io::Write::flush(&mut std::io::stdout())?;
    let mut answer = String::new();
    std::io::BufRead::read_line(&mut std::io::stdin().lock(), &mut answer)?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

/// What the process will actually be, in the order someone reads a command
/// line. Shared with the pre-install disclosure so a server cannot read one way
/// before installing and another way when it is approved.
fn launch_summary(config: &McpServerConfig) -> String {
    plugin_mcp_launch_summary(config)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonPluginTrustEntry {
    server_name: String,
    launch: String,
    launch_hash: String,
    trust: &'static str,
}

impl JsonPluginTrustEntry {
    fn from_approval(approval: &PluginMcpLaunchApproval) -> Self {
        Self {
            server_name: approval.server_name.clone(),
            launch: launch_summary(&approval.config),
            launch_hash: approval.launch_hash.clone(),
            trust: trust_label(approval.trust),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonPluginTrustOutput {
    package_hash: Option<String>,
    package_trust: &'static str,
    servers: Vec<JsonPluginTrustEntry>,
}

impl JsonPluginTrustOutput {
    fn from_review(review: &PluginLaunchReview) -> Self {
        Self {
            package_hash: review.package_hash.clone(),
            package_trust: package_trust_label(review.package_trust, review.package_origin),
            servers: review
                .servers
                .iter()
                .map(JsonPluginTrustEntry::from_approval)
                .collect(),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonPluginRemoveOutput {
    plugin_id: String,
    name: String,
    marketplace_name: String,
}

impl JsonPluginRemoveOutput {
    fn from_selection(selection: PluginSelection) -> Self {
        Self {
            plugin_id: selection.plugin_key,
            name: selection.plugin_name,
            marketplace_name: selection.marketplace_name,
        }
    }
}

struct PluginCommandContext {
    codex_home: PathBuf,
    plugins_input: PluginsConfigInput,
    manager: PluginsManager,
}

async fn load_plugin_command_context(
    overrides: Vec<(String, toml::Value)>,
) -> Result<PluginCommandContext> {
    let codex_home = find_codex_home().context("failed to resolve WHISPLY_HOME")?;
    let config = Config::load_with_cli_overrides(overrides)
        .await
        .context("failed to load configuration")?;
    let plugins_input = config.plugins_config_input();
    let manager = PluginsManager::new(codex_home.to_path_buf());
    manager.set_auth_mode(None);
    Ok(PluginCommandContext {
        codex_home: codex_home.to_path_buf(),
        plugins_input,
        manager,
    })
}

pub(crate) async fn load_cli_auth_mode(config: &Config) -> Option<AuthMode> {
    AuthManager::shared_from_config(config, /*enable_codex_api_key_env*/ true)
        .await
        .auth()
        .await
        .map(|auth| auth.api_auth_mode())
}

struct PluginSelection {
    plugin_name: String,
    marketplace_name: String,
    plugin_key: String,
}

impl PluginSelection {
    fn from_plugin_id(plugin_id: PluginId) -> Self {
        let plugin_key = plugin_id.as_key();
        Self {
            plugin_name: plugin_id.plugin_name,
            marketplace_name: plugin_id.marketplace_name,
            plugin_key,
        }
    }
}

fn parse_plugin_selection(
    plugin: String,
    marketplace_name: Option<String>,
) -> Result<PluginSelection> {
    match (PluginId::parse(&plugin), marketplace_name) {
        (Ok(plugin_id), None) => Ok(PluginSelection::from_plugin_id(plugin_id)),
        (Ok(plugin_id), Some(marketplace_name)) => {
            if plugin_id.marketplace_name != marketplace_name {
                bail!(
                    "plugin id `{}` belongs to marketplace `{}`, but --marketplace specified `{}`",
                    plugin,
                    plugin_id.marketplace_name,
                    marketplace_name
                );
            }
            Ok(PluginSelection::from_plugin_id(plugin_id))
        }
        (Err(_), Some(marketplace_name)) => Ok(PluginSelection::from_plugin_id(PluginId::new(
            plugin,
            marketplace_name,
        )?)),
        (Err(_), None) => {
            bail!("plugin requires --marketplace unless passed as <plugin>@<marketplace>")
        }
    }
}

fn find_marketplace_for_plugin(
    manager: &PluginsManager,
    codex_home: &std::path::Path,
    plugins_input: &PluginsConfigInput,
    marketplace_name: &str,
    plugin_name: &str,
) -> Result<ConfiguredMarketplace> {
    let outcome = manager
        .list_marketplaces_for_config(plugins_input, &[], /*include_openai_curated*/ true)
        .context("failed to list marketplace plugins")?;
    ensure_configured_marketplace_snapshots_loaded(
        codex_home,
        plugins_input,
        &outcome.errors,
        Some(marketplace_name),
    )?;
    let matches = outcome
        .marketplaces
        .into_iter()
        .filter(|marketplace| marketplace.name == marketplace_name)
        .filter(|marketplace| {
            marketplace
                .plugins
                .iter()
                .any(|plugin| plugin.name == plugin_name)
        })
        .collect::<Vec<_>>();

    match matches.as_slice() {
        [] => bail!("plugin `{plugin_name}` was not found in marketplace `{marketplace_name}`"),
        [marketplace] => Ok(marketplace.clone()),
        _ => bail!(
            "plugin `{plugin_name}` in marketplace `{marketplace_name}` matched multiple marketplace roots"
        ),
    }
}

pub(crate) struct ConfiguredMarketplaceSnapshotIssue {
    pub(crate) marketplace_name: String,
    pub(crate) path: PathBuf,
    pub(crate) message: String,
}

fn ensure_configured_marketplace_snapshots_loaded(
    codex_home: &std::path::Path,
    plugins_input: &PluginsConfigInput,
    load_errors: &[MarketplaceListError],
    marketplace_name: Option<&str>,
) -> Result<()> {
    let issues = configured_marketplace_snapshot_issues(
        codex_home,
        plugins_input,
        load_errors,
        marketplace_name,
    );
    if issues.is_empty() {
        return Ok(());
    }

    let issue_lines = issues
        .iter()
        .map(|issue| {
            format!(
                "- `{}` at {}: {}",
                issue.marketplace_name,
                issue.path.display(),
                issue.message
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    bail!("failed to load configured marketplace snapshot(s):\n{issue_lines}");
}

pub(crate) fn configured_marketplace_snapshot_issues(
    codex_home: &std::path::Path,
    plugins_input: &PluginsConfigInput,
    load_errors: &[MarketplaceListError],
    marketplace_name: Option<&str>,
) -> Vec<ConfiguredMarketplaceSnapshotIssue> {
    let Some(user_config) = plugins_input.config_layer_stack.effective_user_config() else {
        return Vec::new();
    };
    let Some(configured_marketplaces) = user_config
        .get("marketplaces")
        .and_then(toml::Value::as_table)
    else {
        return Vec::new();
    };
    let allowed_marketplace_names =
        allowed_configured_marketplace_names(&plugins_input.config_layer_stack, codex_home);

    let default_install_root = marketplace_install_root(codex_home);
    let mut manifest_paths = Vec::new();
    let mut issues = Vec::new();
    for (configured_name, marketplace) in configured_marketplaces {
        if !allowed_marketplace_names.contains(configured_name) {
            continue;
        }
        if marketplace_name.is_some_and(|name| configured_name != name) {
            continue;
        }
        if !marketplace.is_table() {
            issues.push(ConfiguredMarketplaceSnapshotIssue {
                marketplace_name: configured_name.clone(),
                path: PathBuf::from("<invalid config>"),
                message: "configured marketplace entry must be a table".to_string(),
            });
            continue;
        }
        if let Err(err) = validate_plugin_segment(configured_name, "marketplace name") {
            issues.push(ConfiguredMarketplaceSnapshotIssue {
                marketplace_name: configured_name.clone(),
                path: PathBuf::from("<invalid config>"),
                message: err.to_string(),
            });
            continue;
        }
        if marketplace.get("source_type").and_then(toml::Value::as_str) == Some("local")
            && marketplace
                .get("source")
                .and_then(toml::Value::as_str)
                .is_none_or(str::is_empty)
        {
            issues.push(ConfiguredMarketplaceSnapshotIssue {
                marketplace_name: configured_name.clone(),
                path: PathBuf::from("<invalid source>"),
                message: "configured local marketplace source is missing or empty".to_string(),
            });
            continue;
        }
        let Some(root) = resolve_configured_marketplace_root(
            configured_name,
            marketplace,
            &default_install_root,
        ) else {
            continue;
        };
        match find_marketplace_manifest_path(&root) {
            Some(path) => manifest_paths.push((configured_name.clone(), path)),
            None => {
                if is_implicit_system_marketplace_root(configured_name, codex_home, &root) {
                    continue;
                }
                issues.push(ConfiguredMarketplaceSnapshotIssue {
                    marketplace_name: configured_name.clone(),
                    path: root,
                    message: "marketplace root does not contain a supported manifest".to_string(),
                });
            }
        }
    }

    for error in load_errors {
        if let Some((configured_name, _)) = manifest_paths
            .iter()
            .find(|(_, path)| path.as_path() == error.path.as_path())
        {
            issues.push(ConfiguredMarketplaceSnapshotIssue {
                marketplace_name: configured_name.clone(),
                path: error.path.to_path_buf(),
                message: error.message.clone(),
            });
        }
    }
    issues
}

fn is_implicit_system_marketplace_root(
    marketplace_name: &str,
    _codex_home: &Path,
    root: &Path,
) -> bool {
    if matches!(
        marketplace_name,
        OPENAI_BUNDLED_MARKETPLACE_NAME | OPENAI_BUNDLED_ALPHA_MARKETPLACE_NAME
    ) && path_ends_with(root, &[".tmp", "bundled-marketplaces", marketplace_name])
    {
        return true;
    }

    marketplace_name == OPENAI_PRIMARY_RUNTIME_MARKETPLACE_NAME
        && path_ends_with(
            root,
            &[
                "codex-runtimes",
                "codex-primary-runtime",
                "plugins",
                marketplace_name,
            ],
        )
}

fn path_ends_with(path: &Path, suffix: &[&str]) -> bool {
    let path_components = path
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    path_components.as_slice().ends_with(
        &suffix
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>(),
    )
}

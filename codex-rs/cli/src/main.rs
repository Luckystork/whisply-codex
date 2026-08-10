#![recursion_limit = "256"]

use clap::Args;
use clap::CommandFactory;
use clap::Parser;
use clap_complete::Shell;
use clap_complete::generate;
use codex_app_server_daemon::LifecycleCommand as AppServerLifecycleCommand;
use codex_arg0::Arg0DispatchPaths;
use codex_arg0::arg0_dispatch_or_else;
use codex_chatgpt::apply_command::ApplyCommand;
use codex_chatgpt::apply_command::run_apply_command;
use codex_cloud_tasks::Cli as CloudTasksCli;
use codex_exec::Cli as ExecCli;
use codex_exec::Command as ExecCommand;
use codex_exec::ReviewArgs;
use codex_execpolicy::ExecPolicyCheckCommand;
use codex_rollout_trace::REDUCED_STATE_FILE_NAME;
use codex_rollout_trace::replay_bundle;
use codex_state::StateRuntime;
use codex_tui::AppExitInfo;
use codex_tui::Cli as TuiCli;
use codex_tui::ExitReason;
use codex_tui::UpdateAction;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_cli::CliConfigOverrides;
use codex_utils_cli::ProfileV2Name;
use codex_utils_cli::SharedCliOptions;
use owo_colors::OwoColorize;
use std::collections::HashSet;
use std::io::IsTerminal;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use supports_color::Stream;

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod app_cmd;
mod doctor;
mod exec_server_telemetry;
mod marketplace_cmd;
mod mcp_cmd;
mod plugin_cmd;
#[cfg(target_os = "windows")]
mod sandbox_setup;
mod state_db_recovery;
mod whisply_config;
mod whisply_diagnostics;
mod whisply_skills;
mod whisply_verify;
#[cfg(target_os = "linux")]
mod wsl_paths;

use crate::mcp_cmd::McpCli;
use crate::plugin_cmd::PluginCli;
use crate::plugin_cmd::PluginSubcommand;
use doctor::DoctorCommand;
use state_db_recovery as local_state_db;

use codex_config::LoaderOverrides;
use codex_core::config::Config;
use codex_core::config::ConfigBuilder;
use codex_core::config::ConfigOverrides;
use codex_core::config::edit::ConfigEditsBuilder;
use codex_core::config::find_codex_home;
use codex_core::config::resolve_profile_v2_config_path;
use codex_features::FEATURES;
use codex_features::Stage;
use codex_features::is_known_feature_key;
use codex_home::CodexHomeUserInstructionsProvider;
use codex_http_client::HttpClientFactory;
use codex_http_client::OutboundProxyPolicy;
use codex_memories_write::clear_memory_roots_contents;
use codex_model_provider::create_model_provider;
use codex_model_provider::whisply_provider_info;
use codex_models_manager::manager::RefreshStrategy;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::user_input::UserInput;
use codex_terminal_detection::TerminalName;
use codex_whisply::BrokerError;
use codex_whisply::BrokerErrorCode;
use codex_whisply::BrokerLoginStatus;
use codex_whisply::HostControlsAudioAssistMode;
use codex_whisply::HostControlsBuiltinCapability;
use codex_whisply::HostControlsCommand;
use codex_whisply::HostControlsComputerUseRoute;
use codex_whisply::HostControlsInteractiveAction;
use codex_whisply::HostControlsPatch;
use codex_whisply::HostControlsPermissionMode;
use codex_whisply::HostControlsThreadDirectory;
use codex_whisply::NativeBrokerClient;
use codex_whisply::PRODUCT_NAME;
use codex_whisply::UPSTREAM_COMMIT;
use codex_whisply::UPSTREAM_TAG;
use codex_whisply::WHISPLY_RUNTIME_VERSION;
use codex_whisply::first_party_tool_registry;

/// Whisply CLI
///
/// If no subcommand is specified, options will be forwarded to the interactive CLI.
#[derive(Debug, Parser)]
#[clap(
    author,
    version,
    // If a sub‑command is given, ignore requirements of the default args.
    subcommand_negates_reqs = true,
    // The executable is sometimes invoked via a platform‑specific name like
    // `whisply-x86_64-unknown-linux-musl`, but the help output should always use
    // the generic `whisply` command name that users run.
    bin_name = "whisply",
    version = "0.147.0-wsply.1",
    override_usage = "whisply [OPTIONS] [PROMPT]\n       whisply [OPTIONS] <COMMAND> [ARGS]"
)]
struct MultitoolCli {
    /// Enable process-only first-party routing selected by the managed launcher.
    #[arg(long, global = true, hide = true)]
    psp: bool,

    #[clap(flatten)]
    pub config_overrides: CliConfigOverrides,

    #[clap(flatten)]
    pub feature_toggles: FeatureToggles,

    #[clap(flatten)]
    remote: InteractiveRemoteOptions,

    #[clap(flatten)]
    interactive: TuiCli,

    #[clap(subcommand)]
    subcommand: Option<Subcommand>,
}

#[derive(Debug, clap::Subcommand)]
enum Subcommand {
    /// Run Whisply non-interactively.
    #[clap(visible_alias = "e")]
    Exec(ExecCli),

    /// Run a code review non-interactively.
    Review(ReviewCommand),

    /// Manage Whisply account sign-in for the managed runtime.
    Login(LoginCommand),

    /// Show how to sign out of the managed Whisply account.
    Logout(LogoutCommand),

    /// Show the opaque managed account currently selected for this runtime.
    Whoami,

    /// List models from Whisply's signed managed catalog.
    Models(ModelsCommand),

    /// Read or change ordinary controls in the active Whisply Mac app.
    Controls(ControlsCommand),

    /// Show cost-based managed account Usage.
    Usage(UsageCommand),

    /// List and inspect first-party account connectors.
    Connectors(ConnectorsCommand),

    /// List first-party tool descriptors and their declared ownership.
    Tools(ToolsCommand),

    /// Manage external MCP servers for Whisply.
    Mcp(McpCli),

    /// Manage Whisply plugins.
    Plugin(PluginCli),

    /// Start Whisply as an MCP server (stdio).
    McpServer(McpServerCommand),

    /// [experimental] Run the app server or related tooling.
    AppServer(AppServerCommand),

    /// Unavailable in Whisply: standalone remote control bypasses the managed app.
    RemoteControl(ManagedUnavailableCommand),

    /// Launch the Desktop app (opens the app installer if missing).
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    App(app_cmd::AppCommand),

    /// Generate shell completion scripts.
    Completion(CompletionCommand),

    /// Report how the Whisply app bundle manages runtime updates.
    Update,

    /// Show the Whisply runtime version and verified-source identity.
    Version(VersionCommand),

    /// Diagnose local Whisply installation, config, auth, and runtime health.
    Doctor(DoctorCommand),

    /// Run commands within the Whisply-provided sandbox.
    Sandbox(HostSandboxArgs),

    /// Debugging tools.
    Debug(DebugCommand),

    /// Execpolicy tooling.
    #[clap(hide = true)]
    Execpolicy(ExecpolicyCommand),

    /// Apply the latest diff produced by the Whisply agent as a `git apply` to your local working tree.
    #[clap(visible_alias = "a")]
    Apply(ApplyCommand),

    /// Resume a previous interactive session (picker by default; use --last to continue the most recent).
    Resume(ResumeCommand),

    /// Archive a saved session by id or session name.
    Archive(SessionArchiveCommand),

    /// Permanently delete a saved session by id or session name.
    Delete(DeleteCommand),

    /// Unarchive a saved session by id or session name.
    Unarchive(SessionArchiveCommand),

    /// Fork a previous interactive session (picker by default; use --last to fork the most recent).
    Fork(ForkCommand),

    /// List and safely export saved managed sessions.
    Sessions(SessionsCommand),

    /// Manage user-created Whisply skills in the managed product home.
    Skills(whisply_skills::SkillsCommand),

    /// Inspect or change the supported Whisply configuration controls.
    Config(whisply_config::ConfigCommand),

    /// Create, inspect, and remove named Whisply configuration profiles.
    Profile(whisply_config::ProfileCommand),

    /// Run zero-authority diagnostics and presentation-fixture checks.
    Diagnostics(whisply_diagnostics::DiagnosticsCommand),

    /// Run the fixed, zero-authority Whisply release-test plan.
    Test(whisply_verify::TestCommand),

    /// Unavailable in Whisply: cloud tasks require direct ChatGPT authority.
    #[clap(name = "cloud", alias = "cloud-tasks")]
    Cloud(CloudTasksCli),

    /// Unavailable in Whisply: responses traffic must use the managed gateway.
    #[clap(hide = true)]
    ResponsesApiProxy(ManagedUnavailableCommand),

    /// Internal: relay stdio to a Unix domain socket.
    #[clap(hide = true, name = "stdio-to-uds")]
    StdioToUds(StdioToUdsCommand),

    /// [EXPERIMENTAL] Run the standalone exec-server service.
    ExecServer(ExecServerCommand),

    /// Inspect feature flags.
    Features(FeaturesCli),
}

#[derive(Debug, Args)]
struct VersionCommand {
    /// Include pinned upstream source and local manifest contract paths.
    #[arg(long)]
    verbose: bool,
}

#[derive(Debug, Parser)]
struct CompletionCommand {
    /// Shell to generate completions for
    #[clap(value_enum, default_value_t = Shell::Bash)]
    shell: Shell,
}

/// Keeps retired command names parseable long enough to return the managed
/// unavailable error, without linking their direct-client implementations.
#[derive(Debug, Args)]
struct ManagedUnavailableCommand {}

#[derive(Debug, Parser)]
struct DebugCommand {
    #[command(subcommand)]
    subcommand: DebugSubcommand,
}

#[derive(Debug, clap::Subcommand)]
enum DebugSubcommand {
    /// Render the raw model catalog as JSON.
    Models(DebugModelsCommand),

    /// Tooling: helps debug the app server.
    AppServer(DebugAppServerCommand),

    /// Render the model-visible prompt input list as JSON.
    PromptInput(DebugPromptInputCommand),

    /// Run fixed developer verification suites from a checked-out Whisply source tree.
    Verify(whisply_verify::VerifyCommand),

    /// Replay a rollout trace bundle and write reduced state JSON.
    #[clap(hide = true)]
    TraceReduce(DebugTraceReduceCommand),

    /// Internal: reset local memory state for a fresh start.
    #[clap(hide = true)]
    ClearMemories,
}

#[derive(Debug, Parser)]
struct DebugAppServerCommand {
    #[command(subcommand)]
    subcommand: DebugAppServerSubcommand,
}

#[derive(Debug, clap::Subcommand)]
enum DebugAppServerSubcommand {
    // Send message to app server V2.
    SendMessageV2(DebugAppServerSendMessageV2Command),
}

#[derive(Debug, Parser)]
struct DebugAppServerSendMessageV2Command {
    #[arg(value_name = "USER_MESSAGE", required = true)]
    user_message: String,
}

#[derive(Debug, Parser)]
struct DebugPromptInputCommand {
    /// Optional user prompt to append after session context.
    #[arg(value_name = "PROMPT")]
    prompt: Option<String>,

    /// Optional image(s) to attach to the user prompt.
    #[arg(long = "image", short = 'i', value_name = "FILE", value_delimiter = ',', num_args = 1..)]
    images: Vec<PathBuf>,
}

#[derive(Debug, Parser)]
struct DebugModelsCommand;

#[derive(Debug, Args)]
struct ModelsCommand {
    /// Emit the stable public model projection as JSON.
    #[arg(long, default_value_t = false)]
    json: bool,
}

#[derive(Debug, Args)]
struct UsageCommand {
    /// Emit the exact broker-only Usage projection as JSON.
    #[arg(long, default_value_t = false)]
    json: bool,
}

#[derive(Debug, Args)]
struct ConnectorsCommand {
    #[command(subcommand)]
    action: Option<ConnectorsSubcommand>,
}

#[derive(Debug, clap::Subcommand)]
enum ConnectorsSubcommand {
    /// List current account connections and their product availability.
    List {
        /// Emit the full bounded Website projection as JSON.
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Show one current connection, or all current connections when omitted.
    Status {
        /// First-party connection id from `whisply connectors list`.
        id: Option<String>,
        /// Emit the full bounded Website projection as JSON.
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Open Whisply's first-party connector management page.
    Open,
}

#[derive(Debug, Args)]
struct ToolsCommand {
    #[command(subcommand)]
    action: Option<ToolsSubcommand>,
}

#[derive(Debug, clap::Subcommand)]
enum ToolsSubcommand {
    /// List static first-party descriptors. This does not claim handler availability.
    List {
        /// Emit the registry snapshot as JSON.
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Show one descriptor and the availability boundary that owns it.
    Status {
        /// Stable first-party tool id from `whisply tools list`.
        id: String,
        /// Emit the descriptor as JSON.
        #[arg(long, default_value_t = false)]
        json: bool,
    },
}

#[derive(Debug, Parser)]
struct ReviewCommand {
    /// Error out when config.toml contains fields that are not recognized by this version of Codex.
    #[arg(long = "strict-config", default_value_t = false)]
    strict_config: bool,

    #[clap(flatten)]
    args: ReviewArgs,
}

#[derive(Debug, Parser)]
struct McpServerCommand {
    /// Error out when config.toml contains fields that are not recognized by this version of Codex.
    #[arg(long = "strict-config", default_value_t = false)]
    strict_config: bool,
}

#[derive(Debug, Parser)]
struct DebugTraceReduceCommand {
    /// Trace bundle directory containing manifest.json and trace.jsonl.
    #[arg(value_name = "TRACE_BUNDLE")]
    trace_bundle: PathBuf,

    /// Output path for reduced RolloutTrace JSON. Defaults to TRACE_BUNDLE/state.json.
    #[arg(long = "output", short = 'o', value_name = "FILE")]
    output: Option<PathBuf>,
}

#[derive(Debug, Parser)]
struct ResumeCommand {
    /// Session id (UUID) or session name. UUIDs take precedence if it parses.
    /// If omitted, use --last to pick the most recent recorded session.
    #[arg(value_name = "SESSION_ID")]
    session_id: Option<String>,

    /// Continue the most recent session without showing the picker.
    #[arg(long = "last", default_value_t = false)]
    last: bool,

    /// Show all sessions (disables cwd filtering and shows CWD column).
    #[arg(long = "all", default_value_t = false)]
    all: bool,

    /// Include non-interactive sessions in the resume picker and --last selection.
    #[arg(long = "include-non-interactive", default_value_t = false)]
    include_non_interactive: bool,

    #[clap(flatten)]
    remote: InteractiveRemoteOptions,

    #[clap(flatten)]
    config_overrides: SessionTuiCli,
}

#[derive(Debug, Parser)]
struct SessionArchiveCommand {
    /// Session id (UUID) or session name. UUIDs take precedence if it parses.
    #[arg(value_name = "SESSION")]
    target: String,

    #[clap(flatten)]
    remote: InteractiveRemoteOptions,

    #[clap(flatten)]
    config_overrides: SessionArchiveConfigOverrides,
}

#[derive(Debug, Args, Clone, Default)]
struct SessionArchiveConfigOverrides {
    #[clap(flatten)]
    shared: SharedCliOptions,

    /// Error out when config.toml contains fields that are not recognized by this version of Codex.
    #[arg(long = "strict-config", default_value_t = false)]
    strict_config: bool,

    #[clap(flatten)]
    config_overrides: CliConfigOverrides,
}

#[derive(Debug, Args)]
struct DeleteCommand {
    #[clap(flatten)]
    session: SessionArchiveCommand,

    /// Delete without prompting. SESSION must be a UUID.
    #[arg(long, default_value_t = false)]
    force: bool,
}

#[derive(Debug, Parser)]
struct ForkCommand {
    /// Conversation/session id (UUID). When provided, forks this session.
    /// If omitted, use --last to pick the most recent recorded session.
    #[arg(value_name = "SESSION_ID")]
    session_id: Option<String>,

    /// Fork the most recent session without showing the picker.
    #[arg(long = "last", default_value_t = false)]
    last: bool,

    /// Show all sessions (disables cwd filtering and shows CWD column).
    #[arg(long = "all", default_value_t = false)]
    all: bool,

    #[clap(flatten)]
    remote: InteractiveRemoteOptions,

    #[clap(flatten)]
    config_overrides: SessionTuiCli,
}

#[derive(Debug, Args)]
struct SessionsCommand {
    #[command(subcommand)]
    action: SessionsSubcommand,
}

#[derive(Debug, clap::Subcommand)]
enum SessionsSubcommand {
    /// List active, archived, or all saved sessions through the managed app server.
    List(SessionsListCommand),
    /// Export a bounded, redacted public transcript for one saved session.
    Export(SessionsExportCommand),
}

#[derive(Debug, Args)]
struct SessionsListCommand {
    /// List archived sessions instead of active sessions.
    #[arg(long, conflicts_with = "all")]
    archived: bool,

    /// List both active and archived sessions.
    #[arg(long)]
    all: bool,

    /// Maximum number of sessions to return (1-200).
    #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..=200))]
    limit: u32,

    /// Case-insensitive title search interpreted by the managed app server.
    #[arg(long, value_name = "TEXT")]
    search: Option<String>,

    /// Emit the stable public session projection as JSON.
    #[arg(long)]
    json: bool,

    #[clap(flatten)]
    remote: InteractiveRemoteOptions,

    #[clap(flatten)]
    config_overrides: SessionArchiveConfigOverrides,
}

#[derive(Debug, Args)]
struct SessionsExportCommand {
    /// Session id (UUID) or exact saved session name.
    #[arg(value_name = "SESSION")]
    target: String,

    /// Write a new owner-only export file instead of stdout. Existing files are never overwritten.
    #[arg(long, short = 'o', value_name = "FILE")]
    output: Option<PathBuf>,

    #[clap(flatten)]
    remote: InteractiveRemoteOptions,

    #[clap(flatten)]
    config_overrides: SessionArchiveConfigOverrides,
}

/// TUI arguments for session commands where a parsed prompt implies an explicit session id.
///
/// This keeps `--last PROMPT` valid while rejecting `--last SESSION_ID PROMPT`.
#[derive(Debug)]
struct SessionTuiCli(TuiCli);

impl Args for SessionTuiCli {
    fn augment_args(cmd: clap::Command) -> clap::Command {
        TuiCli::augment_args(cmd).mut_arg("prompt", |arg| arg.conflicts_with("last"))
    }

    fn augment_args_for_update(cmd: clap::Command) -> clap::Command {
        TuiCli::augment_args_for_update(cmd).mut_arg("prompt", |arg| arg.conflicts_with("last"))
    }
}

impl clap::FromArgMatches for SessionTuiCli {
    fn from_arg_matches(matches: &clap::ArgMatches) -> Result<Self, clap::Error> {
        TuiCli::from_arg_matches(matches).map(Self)
    }

    fn update_from_arg_matches(&mut self, matches: &clap::ArgMatches) -> Result<(), clap::Error> {
        self.0.update_from_arg_matches(matches)
    }
}

#[cfg(target_os = "macos")]
type HostSandboxArgs = codex_cli::SeatbeltCommand;
#[cfg(target_os = "linux")]
type HostSandboxArgs = codex_cli::LandlockCommand;
#[cfg(target_os = "windows")]
type HostSandboxArgs = codex_cli::WindowsCommand;

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
type HostSandboxArgs = UnsupportedSandboxArgs;

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
#[derive(Debug, Parser)]
struct UnsupportedSandboxArgs {
    /// Layer $WHISPLY_HOME/<name>.config.toml on top of the base user config.
    #[arg(long = "profile", short = 'p')]
    pub config_profile: Option<ProfileV2Name>,

    #[clap(skip)]
    pub config_overrides: CliConfigOverrides,

    /// Full command args to run under the host sandbox.
    #[arg(trailing_var_arg = true)]
    pub command: Vec<String>,
}

#[derive(Debug, Parser)]
struct ExecpolicyCommand {
    #[command(subcommand)]
    sub: ExecpolicySubcommand,
}

#[derive(Debug, clap::Subcommand)]
enum ExecpolicySubcommand {
    /// Check execpolicy files against a command.
    #[clap(name = "check")]
    Check(ExecPolicyCheckCommand),
}

#[derive(Debug, Parser)]
struct LoginCommand {
    #[arg(long = "with-api-key", hide = true)]
    with_api_key: bool,

    #[arg(long = "with-access-token", hide = true)]
    with_access_token: bool,

    #[arg(
        long = "api-key",
        num_args = 0..=1,
        default_missing_value = "",
        value_name = "API_KEY",
        help = "(deprecated) Previously accepted the API key directly; now exits with guidance to use --with-api-key",
        hide = true
    )]
    api_key: Option<String>,

    #[arg(long = "device-auth", hide = true)]
    use_device_code: bool,

    /// EXPERIMENTAL: Use custom OAuth issuer base URL (advanced)
    /// Override the OAuth issuer base URL (advanced)
    #[arg(long = "experimental_issuer", value_name = "URL", hide = true)]
    issuer_base_url: Option<String>,

    /// EXPERIMENTAL: Use custom OAuth client ID (advanced)
    #[arg(long = "experimental_client-id", value_name = "CLIENT_ID", hide = true)]
    client_id: Option<String>,

    #[command(subcommand)]
    action: Option<LoginSubcommand>,
}

#[derive(Debug, clap::Subcommand)]
enum LoginSubcommand {
    /// Show login status.
    Status,

    /// Show the opaque managed account selected by the native broker.
    Whoami,
}

#[derive(Debug, Parser)]
struct LogoutCommand {
    #[clap(skip)]
    config_overrides: CliConfigOverrides,
}

#[derive(Debug, Parser)]
struct ControlsCommand {
    #[command(subcommand)]
    action: ControlsSubcommand,
}

#[derive(Debug, clap::Subcommand)]
enum ControlsSubcommand {
    /// Print the active app's current ordinary-control snapshot.
    Snapshot {
        #[arg(long)]
        session_id: Option<String>,
    },
    /// Enable or disable the paired Computer Use master control.
    ComputerUseMaster {
        #[arg(
            long,
            action = clap::ArgAction::Set,
            value_parser = clap::builder::BoolishValueParser::new()
        )]
        enabled: bool,
    },
    /// Enable or disable one Computer Use route.
    ComputerUseRoute {
        #[arg(long)]
        route: String,
        #[arg(
            long,
            action = clap::ArgAction::Set,
            value_parser = clap::builder::BoolishValueParser::new()
        )]
        enabled: bool,
    },
    /// Enable or disable one ordinary built-in capability.
    Builtin {
        #[arg(long)]
        capability: String,
        #[arg(
            long,
            action = clap::ArgAction::Set,
            value_parser = clap::builder::BoolishValueParser::new()
        )]
        enabled: bool,
    },
    /// Change a product thread's directory, permission mode, or screen preference.
    Thread {
        #[arg(long)]
        session_id: String,
        #[arg(long, conflicts_with = "no_directory")]
        directory: Option<String>,
        #[arg(long)]
        no_directory: bool,
        #[arg(long)]
        permission: Option<String>,
        #[arg(long, value_parser = clap::builder::BoolishValueParser::new())]
        prefer_screen: Option<bool>,
    },
    /// Change Audio Assist mode or transcript task suggestions.
    AudioAssist {
        #[arg(long)]
        mode: Option<String>,
        #[arg(long, value_parser = clap::builder::BoolishValueParser::new())]
        transcript_task_suggestions: Option<bool>,
    },
    /// Stop Computer Use only when it belongs to this exact product session.
    StopComputerUse {
        #[arg(long)]
        session_id: String,
    },
    /// Return ordinary GUI guidance for an interaction that needs user presence.
    RequestInteractive {
        #[arg(long)]
        action: String,
    },
}

fn native_broker_for_cli() -> anyhow::Result<Arc<NativeBrokerClient>> {
    let broker = NativeBrokerClient::from_environment()
        .map_err(broker_cli_error)?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "The managed Whisply broker is unavailable. Start Whisply from the installed app."
            )
        })?;
    broker.hello().map_err(broker_cli_error)?;
    Ok(broker)
}

fn broker_cli_error(error: BrokerError) -> anyhow::Error {
    match error {
        BrokerError::Rejected(BrokerErrorCode::LoginRequired)
        | BrokerError::Rejected(BrokerErrorCode::Unauthenticated) => anyhow::anyhow!(
            "Sign in is required. Run `whisply login` from the managed Whisply app."
        ),
        BrokerError::Rejected(BrokerErrorCode::StaleEpoch)
        | BrokerError::Rejected(BrokerErrorCode::Expired) => anyhow::anyhow!(
            "The managed Whisply account state changed. Run `whisply login status` and try again."
        ),
        BrokerError::Rejected(BrokerErrorCode::ManifestMismatch)
        | BrokerError::Rejected(BrokerErrorCode::InvalidClient)
        | BrokerError::Rejected(BrokerErrorCode::DescriptorInvalid) => anyhow::anyhow!(
            "This runtime is not accepted by the managed Whisply broker. Restart it from the installed app."
        ),
        BrokerError::Rejected(BrokerErrorCode::Unavailable)
        | BrokerError::UnsupportedPlatform
        | BrokerError::Io
        | BrokerError::InvalidPeer
        | BrokerError::UnsafeSocket => anyhow::anyhow!(
            "The managed Whisply broker is temporarily unavailable. Restart the app and try again."
        ),
        _ => anyhow::anyhow!(
            "The managed Whisply broker rejected the request. Restart the app and try again."
        ),
    }
}

fn print_whisply_login_status() -> anyhow::Result<()> {
    let status = native_broker_for_cli()?
        .status()
        .map_err(broker_cli_error)?;
    if status.authenticated {
        println!("Signed in to the managed Whisply account.");
    } else {
        println!("Not signed in. Run `whisply login` to open managed browser sign-in.");
    }
    Ok(())
}

fn print_whisply_whoami() -> anyhow::Result<()> {
    let status = native_broker_for_cli()?
        .status()
        .map_err(broker_cli_error)?;
    if !status.authenticated {
        anyhow::bail!("Not signed in. Run `whisply login` first.");
    }
    let account_key = status
        .opaque_account_key
        .ok_or_else(|| anyhow::anyhow!("The managed account projection is unavailable."))?;
    println!("{account_key}");
    Ok(())
}

fn begin_whisply_browser_login() -> anyhow::Result<()> {
    let broker = native_broker_for_cli()?;
    let login = broker.login_begin().map_err(broker_cli_error)?;
    open_managed_browser(&login.authorization_url)?;
    // The native broker owns callback handling. Never print or persist the
    // URL/handle because either may be sensitive broker-local material.
    println!(
        "Browser sign-in opened. Complete it in the browser, then run `whisply login status`."
    );
    Ok(())
}

fn open_managed_browser(authorization_url: &str) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        let status = std::process::Command::new("open")
            .arg(authorization_url)
            .status()
            .map_err(|_| anyhow::anyhow!("Unable to open managed browser sign-in."))?;
        if status.success() {
            return Ok(());
        }
        anyhow::bail!("Unable to open managed browser sign-in.");
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = authorization_url;
        anyhow::bail!(
            "Managed browser sign-in is available only through the Whisply app on this platform."
        );
    }
}

/// Fetches the only public model catalog path. The model provider constructs
/// its endpoint exclusively from the verified native descriptor and verifies
/// the gateway's signed catalog before projecting it to Codex model metadata.
async fn load_signed_whisply_models()
-> anyhow::Result<Vec<codex_protocol::openai_models::ModelInfo>> {
    let provider = create_model_provider(whisply_provider_info(), None);
    let models_manager = provider.models_manager_without_cache(None);
    let catalog = models_manager
        .raw_model_catalog(
            RefreshStrategy::OnlineIfUncached,
            HttpClientFactory::new(OutboundProxyPolicy::RespectSystemProxy),
        )
        .await;
    if catalog.models.is_empty() {
        anyhow::bail!(
            "The signed Whisply model catalog is unavailable. Start Whisply from the managed app and sign in."
        );
    }
    Ok(catalog.models)
}

fn whisply_public_model_projection(
    models: Vec<codex_protocol::openai_models::ModelInfo>,
) -> serde_json::Value {
    let models = models
        .into_iter()
        .map(|model| {
            serde_json::json!({
                "id": model.slug,
                "name": model.display_name,
                "reasoningEfforts": model
                    .supported_reasoning_levels
                    .iter()
                    .map(|level| level.effort.as_str())
                    .collect::<Vec<_>>(),
                "defaultReasoningEffort": model
                    .default_reasoning_level
                    .as_ref()
                    .map(codex_protocol::openai_models::ReasoningEffort::as_str),
                "inputModalities": model
                    .input_modalities
                    .iter()
                    .map(|modality| format!("{modality:?}").to_ascii_lowercase())
                    .collect::<Vec<_>>(),
                "contextLimit": model.context_window,
                "outputLimit": model.max_context_window,
                "supportsToolCalls": model.supports_parallel_tool_calls,
                "supportsReasoningSummary": model.supports_reasoning_summary_parameter,
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({ "models": models })
}

async fn run_whisply_models(json: bool) -> anyhow::Result<()> {
    let projection = whisply_public_model_projection(load_signed_whisply_models().await?);
    if json {
        serde_json::to_writer_pretty(std::io::stdout(), &projection)?;
        println!();
        return Ok(());
    }
    let models = projection
        .get("models")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("The signed Whisply model catalog is invalid."))?;
    for model in models {
        let id = model
            .get("id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("?");
        let name = model
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("?");
        let efforts = model
            .get("reasoningEfforts")
            .and_then(serde_json::Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        println!("{id}\t{name}\t{efforts}");
    }
    Ok(())
}

fn current_broker_epoch(broker: &NativeBrokerClient) -> anyhow::Result<Option<String>> {
    let status = broker.status().map_err(broker_cli_error)?;
    if !status.authenticated {
        anyhow::bail!("Not signed in. Run `whisply login` first.");
    }
    Ok(status.account_epoch)
}

fn current_broker_account_binding(
    broker: &NativeBrokerClient,
) -> anyhow::Result<BrokerLoginStatus> {
    let status = broker.status().map_err(broker_cli_error)?;
    if !status.authenticated
        || status
            .opaque_account_key
            .as_deref()
            .map(str::is_empty)
            .unwrap_or(true)
        || status
            .account_epoch
            .as_deref()
            .map(str::is_empty)
            .unwrap_or(true)
    {
        anyhow::bail!("Not signed in. Run `whisply login` first.");
    }
    Ok(status)
}

fn run_whisply_controls(command: ControlsCommand) -> anyhow::Result<()> {
    let broker = native_broker_for_cli()?;
    let account_epoch = current_broker_epoch(&broker)?;
    let snapshot_revision = |session_id: Option<&str>| -> anyhow::Result<u64> {
        let snapshot = broker
            .host_controls_snapshot(account_epoch.as_deref(), session_id)
            .map_err(broker_cli_error)?;
        snapshot
            .get("revision")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| {
                anyhow::anyhow!("The active Whisply app returned an invalid control snapshot.")
            })
    };
    let print = |value: serde_json::Value| -> anyhow::Result<()> {
        serde_json::to_writer_pretty(std::io::stdout(), &value)?;
        println!();
        Ok(())
    };

    match command.action {
        ControlsSubcommand::Snapshot { session_id } => print(
            broker
                .host_controls_snapshot(account_epoch.as_deref(), session_id.as_deref())
                .map_err(broker_cli_error)?,
        ),
        ControlsSubcommand::ComputerUseMaster { enabled } => {
            let revision = snapshot_revision(None)?;
            print(
                broker
                    .host_controls_apply(
                        account_epoch.as_deref(),
                        revision,
                        HostControlsPatch::ComputerUseMaster { enabled },
                    )
                    .map_err(broker_cli_error)?,
            )
        }
        ControlsSubcommand::ComputerUseRoute { route, enabled } => {
            let revision = snapshot_revision(None)?;
            print(
                broker
                    .host_controls_apply(
                        account_epoch.as_deref(),
                        revision,
                        HostControlsPatch::ComputerUseRoute {
                            route: parse_host_controls_route(&route)?,
                            enabled,
                        },
                    )
                    .map_err(broker_cli_error)?,
            )
        }
        ControlsSubcommand::Builtin {
            capability,
            enabled,
        } => {
            let revision = snapshot_revision(None)?;
            print(
                broker
                    .host_controls_apply(
                        account_epoch.as_deref(),
                        revision,
                        HostControlsPatch::BuiltinCapability {
                            capability: parse_host_controls_builtin(&capability)?,
                            enabled,
                        },
                    )
                    .map_err(broker_cli_error)?,
            )
        }
        ControlsSubcommand::Thread {
            session_id,
            directory,
            no_directory,
            permission,
            prefer_screen,
        } => {
            if directory.is_none()
                && !no_directory
                && permission.is_none()
                && prefer_screen.is_none()
            {
                anyhow::bail!("Choose at least one thread control to change.");
            }
            let revision = snapshot_revision(Some(&session_id))?;
            let directory = if no_directory {
                Some(HostControlsThreadDirectory::NoDirectory)
            } else {
                directory
                    .map(|canonical_path| HostControlsThreadDirectory::Directory { canonical_path })
            };
            print(
                broker
                    .host_controls_apply(
                        account_epoch.as_deref(),
                        revision,
                        HostControlsPatch::Thread {
                            session_id,
                            directory,
                            permission_mode: permission
                                .as_deref()
                                .map(parse_host_controls_permission)
                                .transpose()?,
                            prefer_screen,
                        },
                    )
                    .map_err(broker_cli_error)?,
            )
        }
        ControlsSubcommand::AudioAssist {
            mode,
            transcript_task_suggestions,
        } => {
            if mode.is_none() && transcript_task_suggestions.is_none() {
                anyhow::bail!("Choose an Audio Assist mode or transcript-task-suggestions value.");
            }
            let revision = snapshot_revision(None)?;
            print(
                broker
                    .host_controls_apply(
                        account_epoch.as_deref(),
                        revision,
                        HostControlsPatch::AudioAssist {
                            mode: mode
                                .as_deref()
                                .map(parse_host_controls_audio_mode)
                                .transpose()?,
                            transcript_task_suggestions_enabled: transcript_task_suggestions,
                        },
                    )
                    .map_err(broker_cli_error)?,
            )
        }
        ControlsSubcommand::StopComputerUse { session_id } => print(
            broker
                .host_controls_execute(
                    account_epoch.as_deref(),
                    HostControlsCommand::StopComputerUse { session_id },
                )
                .map_err(broker_cli_error)?,
        ),
        ControlsSubcommand::RequestInteractive { action } => print(
            broker
                .host_controls_execute(
                    account_epoch.as_deref(),
                    HostControlsCommand::RequestInteractive {
                        action: parse_host_controls_interactive_action(&action)?,
                    },
                )
                .map_err(broker_cli_error)?,
        ),
    }
}

fn parse_host_controls_route(value: &str) -> anyhow::Result<HostControlsComputerUseRoute> {
    match value {
        "native-apps" | "native_apps" => Ok(HostControlsComputerUseRoute::NativeApps),
        "existing-browser" | "existing_browser" => {
            Ok(HostControlsComputerUseRoute::ExistingBrowser)
        }
        "spawned-browser" | "spawned_browser" => Ok(HostControlsComputerUseRoute::SpawnedBrowser),
        "local-files" | "local_files" => Ok(HostControlsComputerUseRoute::LocalFiles),
        _ => anyhow::bail!(
            "Unknown Computer Use route. Use native-apps, existing-browser, spawned-browser, or local-files."
        ),
    }
}

fn parse_host_controls_builtin(value: &str) -> anyhow::Result<HostControlsBuiltinCapability> {
    match value {
        "browser" => Ok(HostControlsBuiltinCapability::Browser),
        "chrome" => Ok(HostControlsBuiltinCapability::Chrome),
        "computer-use" | "computer_use" => Ok(HostControlsBuiltinCapability::ComputerUse),
        "documents" => Ok(HostControlsBuiltinCapability::Documents),
        "pdf" => Ok(HostControlsBuiltinCapability::Pdf),
        "spreadsheets" => Ok(HostControlsBuiltinCapability::Spreadsheets),
        "presentations" => Ok(HostControlsBuiltinCapability::Presentations),
        _ => anyhow::bail!("Unknown built-in capability."),
    }
}

fn parse_host_controls_permission(value: &str) -> anyhow::Result<HostControlsPermissionMode> {
    match value {
        "ask" => Ok(HostControlsPermissionMode::Ask),
        "auto" => Ok(HostControlsPermissionMode::Auto),
        "full-access" | "full_access" => Ok(HostControlsPermissionMode::FullAccess),
        _ => anyhow::bail!("Unknown permission mode. Use ask, auto, or full-access."),
    }
}

fn parse_host_controls_audio_mode(value: &str) -> anyhow::Result<HostControlsAudioAssistMode> {
    match value {
        "manual" => Ok(HostControlsAudioAssistMode::Manual),
        "automatic" | "auto" => Ok(HostControlsAudioAssistMode::Automatic),
        _ => anyhow::bail!("Unknown Audio Assist mode. Use manual or automatic."),
    }
}

fn parse_host_controls_interactive_action(
    value: &str,
) -> anyhow::Result<HostControlsInteractiveAction> {
    match value {
        "computer-use-accessibility-permission" => {
            Ok(HostControlsInteractiveAction::ComputerUseAccessibilityPermission)
        }
        "computer-use-screen-recording-permission" => {
            Ok(HostControlsInteractiveAction::ComputerUseScreenRecordingPermission)
        }
        "choose-thread-directory" => Ok(HostControlsInteractiveAction::ChooseThreadDirectory),
        "choose-trusted-application" => Ok(HostControlsInteractiveAction::ChooseTrustedApplication),
        "choose-window" => Ok(HostControlsInteractiveAction::ChooseWindow),
        "system-settings" => Ok(HostControlsInteractiveAction::SystemSettings),
        _ => anyhow::bail!("Unknown interactive action."),
    }
}

fn run_whisply_usage(json: bool) -> anyhow::Result<()> {
    let broker = native_broker_for_cli()?;
    let binding = current_broker_account_binding(&broker)?;
    let snapshot = broker.account_usage(&binding).map_err(|_| {
        anyhow::anyhow!("Current managed Usage is unavailable. Try again from the Whisply app.")
    })?;
    if json {
        serde_json::to_writer_pretty(std::io::stdout(), &snapshot)?;
        println!();
        return Ok(());
    }

    println!("Usage ({})", snapshot.tier);
    for window in &snapshot.windows {
        let category = match window.category {
            codex_whisply::UsageWindowCategory::Usage => "usage",
            codex_whisply::UsageWindowCategory::Transcription => "transcription",
        };
        let kind = match window.window {
            codex_whisply::UsageWindowKind::FiveHour => "five-hour",
            codex_whisply::UsageWindowKind::Weekly => "weekly",
        };
        let reset = window.resets_at.as_deref().unwrap_or("not scheduled");
        println!(
            "{category}/{kind}: settled {:.3}, reserved {:.3}, cap {:.3}, used {:.1}% (resets {reset})",
            window.settled,
            window.reserved,
            window.cap,
            window.used_fraction * 100.0,
        );
    }
    println!(
        "Rate card {} · {} {}",
        snapshot.metering.rate_card_version, snapshot.metering.basis, snapshot.metering.currency,
    );
    Ok(())
}

fn load_broker_connections() -> anyhow::Result<codex_whisply::ContextualConnectionsSnapshot> {
    let broker = native_broker_for_cli()?;
    let binding = current_broker_account_binding(&broker)?;
    broker.account_connections(&binding).map_err(|_| {
        anyhow::anyhow!(
            "Current managed connector state is unavailable. Try again from the Whisply app."
        )
    })
}

fn print_connections_human(snapshot: &codex_whisply::ContextualConnectionsSnapshot) {
    if snapshot.connections.is_empty() {
        println!("No first-party connectors are currently connected.");
        return;
    }
    for connection in &snapshot.connections {
        let provider = format!("{:?}", connection.provider).to_ascii_lowercase();
        let status = format!("{:?}", connection.status).to_ascii_lowercase();
        let products = connection.products.join(", ");
        println!("{}\t{}\t{}\t{}", connection.id, provider, status, products);
    }
}

fn run_whisply_connectors(command: ConnectorsCommand) -> anyhow::Result<()> {
    match command
        .action
        .unwrap_or(ConnectorsSubcommand::List { json: false })
    {
        ConnectorsSubcommand::Open => open_first_party_connections_page(),
        ConnectorsSubcommand::List { json } => {
            let snapshot = load_broker_connections()?;
            if json {
                serde_json::to_writer_pretty(std::io::stdout(), &snapshot)?;
                println!();
            } else {
                print_connections_human(&snapshot);
            }
            Ok(())
        }
        ConnectorsSubcommand::Status { id, json } => {
            let snapshot = load_broker_connections()?;
            if let Some(id) = id
                && !snapshot
                    .connections
                    .iter()
                    .any(|connection| connection.id == id)
            {
                anyhow::bail!("No current first-party connector matches that id.");
            }
            if json {
                serde_json::to_writer_pretty(std::io::stdout(), &snapshot)?;
                println!();
            } else {
                print_connections_human(&snapshot);
            }
            Ok(())
        }
    }
}

fn open_first_party_connections_page() -> anyhow::Result<()> {
    const CONNECTIONS_URL: &str = "https://whisply.net/account/connections/";
    #[cfg(target_os = "macos")]
    {
        let status = std::process::Command::new("open")
            .arg(CONNECTIONS_URL)
            .status()
            .map_err(|_| anyhow::anyhow!("Unable to open Whisply connector management."))?;
        if status.success() {
            return Ok(());
        }
        anyhow::bail!("Unable to open Whisply connector management.");
    }
    #[cfg(not(target_os = "macos"))]
    {
        anyhow::bail!(
            "Whisply connector management is available through the Whisply app on macOS."
        );
    }
}

fn run_whisply_tools(command: ToolsCommand) -> anyhow::Result<()> {
    let registry = first_party_tool_registry();
    match command
        .action
        .unwrap_or(ToolsSubcommand::List { json: false })
    {
        ToolsSubcommand::List { json } => {
            let snapshot = registry.snapshot(WHISPLY_RUNTIME_VERSION)?;
            if json {
                serde_json::to_writer_pretty(std::io::stdout(), &snapshot)?;
                println!();
            } else {
                for descriptor in registry.descriptors() {
                    println!(
                        "{}\t{}\t{:?}\t{:?}",
                        descriptor.id,
                        descriptor.display_name,
                        descriptor.owner,
                        descriptor.action_class,
                    );
                }
                println!(
                    "Availability is owner-authenticated at execution time; this command does not claim a live handler."
                );
            }
            Ok(())
        }
        ToolsSubcommand::Status { id, json } => {
            let descriptor = registry
                .get(&id)
                .ok_or_else(|| anyhow::anyhow!("No first-party tool matches that id."))?;
            if json {
                serde_json::to_writer_pretty(std::io::stdout(), descriptor)?;
                println!();
            } else {
                println!("{} ({})", descriptor.display_name, descriptor.id);
                println!("Owner: {:?}", descriptor.owner);
                println!("Action class: {:?}", descriptor.action_class);
                println!(
                    "Availability: resolved by the authenticated owner only at execution time."
                );
            }
            Ok(())
        }
    }
}

#[derive(Debug, Parser)]
struct AppServerCommand {
    /// Omit to run the app server; specify a subcommand for tooling.
    #[command(subcommand)]
    subcommand: Option<AppServerSubcommand>,

    #[command(flatten)]
    code_mode_host: codex_app_server::AppServerCodeModeHostArgs,

    /// Error out when config.toml contains fields that are not recognized by this version of Codex.
    #[arg(long = "strict-config", default_value_t = false)]
    strict_config: bool,

    /// Transport endpoint URL. Supported values: `stdio://` (default),
    /// `unix://`, `unix://PATH`, `ws://IP:PORT`, `off`.
    #[arg(
        long = "listen",
        value_name = "URL",
        default_value = codex_app_server::AppServerTransport::DEFAULT_LISTEN_URL
    )]
    listen: codex_app_server::AppServerTransport,

    /// Use stdio as the transport (equivalent to `--listen stdio://`).
    #[arg(long = "stdio", conflicts_with = "listen")]
    stdio: bool,

    /// Enable remote control for this app-server process without changing persistence.
    #[arg(long = "remote-control", hide = true)]
    remote_control: bool,

    /// Controls whether analytics are enabled by default.
    ///
    /// Analytics are disabled by default for app-server. Users have to explicitly opt in
    /// via the `analytics` section in the config.toml file.
    ///
    /// However, for first-party use cases like the VSCode IDE extension, we default analytics
    /// to be enabled by default by setting this flag. Users can still opt out by setting this
    /// in their config.toml:
    ///
    /// ```toml
    /// [analytics]
    /// enabled = false
    /// ```
    ///
    /// See https://developers.openai.com/codex/config-advanced/#metrics for more details.
    #[arg(long = "analytics-default-enabled")]
    analytics_default_enabled: bool,

    #[command(flatten)]
    auth: codex_app_server::AppServerWebsocketAuthArgs,
}

#[derive(Debug, Parser)]
struct ExecServerCommand {
    /// Error out when config.toml contains fields that are not recognized by this version of Codex.
    #[arg(long = "strict-config", default_value_t = false)]
    strict_config: bool,

    /// Maximum number of requests to process concurrently on each connection.
    #[arg(
        long = "concurrent-requests",
        value_name = "COUNT",
        default_value = "1"
    )]
    request_dispatch_mode: codex_exec_server::RequestDispatchMode,

    /// Transport endpoint URL. Supported values: `ws://LOOPBACK_IP:PORT` (default), `stdio`, `stdio://`.
    #[arg(long = "listen", value_name = "URL")]
    listen: Option<String>,
}

#[derive(Debug, clap::Subcommand)]
#[allow(clippy::enum_variant_names)]
enum AppServerSubcommand {
    /// Manage the local app-server daemon.
    Daemon(AppServerDaemonCommand),

    /// Proxy stdio bytes to the running app-server control socket.
    Proxy(AppServerProxyCommand),

    /// [experimental] Generate TypeScript bindings for the app server protocol.
    GenerateTs(GenerateTsCommand),

    /// [experimental] Generate JSON Schema for the app server protocol.
    GenerateJsonSchema(GenerateJsonSchemaCommand),

    /// [internal] Generate internal JSON Schema artifacts for Codex tooling.
    #[clap(hide = true)]
    GenerateInternalJsonSchema(GenerateInternalJsonSchemaCommand),
}

#[derive(Debug, Args)]
struct AppServerDaemonCommand {
    #[command(subcommand)]
    subcommand: AppServerDaemonSubcommand,
}

#[derive(Debug, clap::Subcommand)]
enum AppServerDaemonSubcommand {
    /// Unavailable in Whisply: bootstrap starts the standalone updater.
    Bootstrap(AppServerBootstrapCommand),

    /// Unavailable in Whisply: persisted daemon state can enable remote control.
    Start,

    /// Unavailable in Whisply: persisted daemon state can enable remote control.
    Restart,

    /// Unavailable in Whisply: remote control is owned by the managed app.
    EnableRemoteControl,

    /// Unavailable in Whisply: remote control is owned by the managed app.
    DisableRemoteControl,

    /// Stop the local app server daemon.
    Stop,

    /// Print local CLI and running app-server versions as JSON.
    Version,

    /// [internal] Unavailable in Whisply: standalone updater loop.
    #[clap(hide = true)]
    PidUpdateLoop,
}

#[derive(Debug, Args)]
struct AppServerProxyCommand {
    /// Path to the app-server Unix domain socket to connect to.
    #[arg(long = "sock", value_name = "SOCKET_PATH", value_parser = parse_socket_path)]
    socket_path: Option<AbsolutePathBuf>,
}

#[derive(Debug, Args)]
struct AppServerBootstrapCommand {
    /// Launch the managed app-server with remote control enabled.
    #[arg(long = "remote-control")]
    remote_control: bool,
}

#[derive(Debug, Args)]
struct GenerateTsCommand {
    /// Output directory where .ts files will be written
    #[arg(short = 'o', long = "out", value_name = "DIR")]
    out_dir: PathBuf,

    /// Optional path to the Prettier executable to format generated files
    #[arg(short = 'p', long = "prettier", value_name = "PRETTIER_BIN")]
    prettier: Option<PathBuf>,

    /// Include experimental methods and fields in the generated output
    #[arg(long = "experimental", default_value_t = false)]
    experimental: bool,
}

#[derive(Debug, Args)]
struct GenerateJsonSchemaCommand {
    /// Output directory where the schema bundle will be written
    #[arg(short = 'o', long = "out", value_name = "DIR")]
    out_dir: PathBuf,

    /// Include experimental methods and fields in the generated output
    #[arg(long = "experimental", default_value_t = false)]
    experimental: bool,
}

#[derive(Debug, Args)]
struct GenerateInternalJsonSchemaCommand {
    /// Output directory where internal JSON Schema artifacts will be written
    #[arg(short = 'o', long = "out", value_name = "DIR")]
    out_dir: PathBuf,
}

#[derive(Debug, Parser)]
struct StdioToUdsCommand {
    /// Path to the Unix domain socket to connect to.
    #[arg(value_name = "SOCKET_PATH", value_parser = parse_socket_path)]
    socket_path: AbsolutePathBuf,
}

fn parse_socket_path(raw: &str) -> Result<AbsolutePathBuf, String> {
    AbsolutePathBuf::relative_to_current_dir(raw)
        .map_err(|err| format!("failed to resolve socket path `{raw}`: {err}"))
}

fn format_exit_messages(exit_info: AppExitInfo, color_enabled: bool) -> Vec<String> {
    let is_fatal = matches!(&exit_info.exit_reason, ExitReason::Fatal(_));
    let AppExitInfo {
        token_usage,
        thread_id: conversation_id,
        resume_hint,
        ..
    } = exit_info;

    let mut lines = Vec::new();
    if !token_usage.is_zero() {
        lines.push(token_usage.to_string());
    }

    if let Some(resume_cmd) = resume_hint {
        let command = if color_enabled {
            resume_cmd.cyan().to_string()
        } else {
            resume_cmd
        };
        lines.push(format!("To continue this session, run {command}"));
    } else if is_fatal && let Some(conversation_id) = conversation_id {
        lines.push(format!("Session ID: {conversation_id}"));
    }

    lines
}

/// Handle the app exit and print the results. Optionally run the update action.
fn handle_app_exit(exit_info: AppExitInfo) -> anyhow::Result<()> {
    let is_fatal = match &exit_info.exit_reason {
        ExitReason::Fatal(message) => {
            eprintln!("ERROR: {message}");
            true
        }
        ExitReason::UserRequested => false,
    };

    let update_action = exit_info.update_action;
    let color_enabled = supports_color::on(Stream::Stdout).is_some();
    for line in format_exit_messages(exit_info, color_enabled) {
        println!("{line}");
    }
    if is_fatal {
        std::io::stdout().flush()?;
        std::process::exit(1);
    }
    if let Some(action) = update_action {
        run_update_action(action)?;
    }
    Ok(())
}

/// Report the only supported update path for the managed runtime.
fn run_update_action(action: UpdateAction) -> anyhow::Result<()> {
    // The downstream runtime is never independently installed, downloaded, or
    // patched. The owning Whisply app update replaces the verified Runtime
    // support tree atomically, including this executable and its catalog key
    // resource. Keep this callback so upstream TUI update prompts cannot
    // launch a package-manager or curl-based updater.
    let _ = action;
    println!(
        "Whisply updates are installed by the managed app bundle. Update the Whisply app, then restart it."
    );
    Ok(())
}

fn run_update_command() -> anyhow::Result<()> {
    run_update_action(UpdateAction::ManagedAppBundle)
}

fn run_execpolicycheck(cmd: ExecPolicyCheckCommand) -> anyhow::Result<()> {
    cmd.run()
}

async fn run_session_archive_cli_command(
    action: codex_tui::SessionArchiveAction,
    cmd: SessionArchiveCommand,
    mut interactive: TuiCli,
    root_config_overrides: CliConfigOverrides,
    root_remote: Option<String>,
    root_remote_auth_token_env: Option<String>,
    arg0_paths: Arg0DispatchPaths,
) -> anyhow::Result<String> {
    let SessionArchiveCommand {
        target,
        remote,
        config_overrides,
    } = cmd;
    let options = managed_session_command_options(
        interactive,
        root_config_overrides,
        remote,
        config_overrides,
        root_remote,
        root_remote_auth_token_env,
        arg0_paths,
    )?;
    codex_tui::run_session_archive_command(action, target, options)
        .await
        .map_err(|err| anyhow::anyhow!("{err}"))
}

fn managed_session_command_options(
    interactive: TuiCli,
    root_config_overrides: CliConfigOverrides,
    remote: InteractiveRemoteOptions,
    config_overrides: SessionArchiveConfigOverrides,
    root_remote: Option<String>,
    root_remote_auth_token_env: Option<String>,
    arg0_paths: Arg0DispatchPaths,
) -> anyhow::Result<codex_tui::SessionArchiveCommandOptions> {
    let mut interactive =
        finalize_session_archive_interactive(interactive, root_config_overrides, config_overrides);
    configure_managed_session_provider(&mut interactive, "sessions")?;

    // Do not perform a broker status preflight here. The native capability is
    // an inherited, one-shot launch descriptor consumed by the managed model
    // provider when the embedded app server initializes. Session operations
    // force that provider below, so a separate CLI probe would consume the
    // authority before the app-server boundary can use it.
    let explicit_remote_endpoint = resolve_remote_endpoint(
        remote.remote.or(root_remote),
        remote.remote_auth_token_env.or(root_remote_auth_token_env),
    )?;
    Ok(codex_tui::SessionArchiveCommandOptions {
        cli: interactive,
        arg0_paths,
        explicit_remote_endpoint,
    })
}

fn configure_managed_session_provider(
    interactive: &mut TuiCli,
    command: &str,
) -> anyhow::Result<()> {
    if interactive.oss || interactive.oss_provider.is_some() {
        anyhow::bail!(
            "{command} requires the managed Whisply gateway; local providers are not supported"
        );
    }

    let overrides = interactive
        .config_overrides
        .parse_overrides()
        .map_err(|error| {
            anyhow::anyhow!("failed to parse managed session configuration: {error}")
        })?;
    for (key, value) in overrides {
        if key == "model_provider" && value.as_str() != Some("whisply") {
            anyhow::bail!(
                "{command} does not accept a user-selected model provider; use the managed Whisply gateway"
            );
        }
        if key == "model_providers" || key.starts_with("model_providers.") {
            anyhow::bail!("{command} does not accept custom model provider configuration");
        }
    }

    // Profile/base config can retain an old local-provider selection for
    // compatible non-session workflows. Session lifecycle is always routed
    // through the release-owned Whisply provider, and this final override is
    // deliberately appended after all user CLI overrides.
    interactive
        .config_overrides
        .raw_overrides
        .push(r#"model_provider="whisply""#.to_string());
    Ok(())
}

fn print_session_list(entries: &[codex_tui::SessionListEntry], json: bool) -> anyhow::Result<()> {
    if json {
        serde_json::to_writer_pretty(std::io::stdout(), entries)?;
        println!();
        return Ok(());
    }
    for entry in entries {
        let collection = if entry.archived { "archived" } else { "active" };
        let name = terminal_cell(entry.name.as_deref().unwrap_or("(untitled)"));
        let preview = terminal_cell(&entry.preview);
        println!("{}\t{collection}\t{name}\t{preview}", entry.id);
    }
    Ok(())
}

/// List rows include user-supplied titles and previews. Keep them in a single
/// terminal cell so a stored escape sequence cannot repaint a terminal or
/// forge an adjacent row. JSON exports remain JSON-escaped by serde.
fn terminal_cell(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect()
}

fn write_session_export(
    export: &codex_tui::SessionExport,
    output: Option<PathBuf>,
) -> anyhow::Result<()> {
    let encoded = serde_json::to_vec_pretty(export)?;
    if encoded.len() > 2 * 1024 * 1024 {
        anyhow::bail!("The bounded session export exceeded its 2 MiB public export limit.");
    }
    let Some(output) = output else {
        std::io::stdout().write_all(&encoded)?;
        println!();
        return Ok(());
    };

    if output.is_absolute() && output.parent().is_none() {
        anyhow::bail!("Refusing to write an export to a filesystem root.");
    }
    let parent = output
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Export output must have a parent directory."))?;
    let parent_metadata = std::fs::symlink_metadata(parent)?;
    if parent_metadata.file_type().is_symlink() || !parent_metadata.is_dir() {
        anyhow::bail!("Export output directory does not exist.");
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options
        .open(&output)
        .map_err(|error| anyhow::anyhow!("Failed to create the session export: {error}"))?;
    file.write_all(&encoded)?;
    file.sync_all()?;
    println!(
        "Exported a bounded redacted session transcript to {}.",
        output.display()
    );
    Ok(())
}

fn delete_action(target: &str, force: bool) -> anyhow::Result<codex_tui::SessionArchiveAction> {
    if force && codex_protocol::ThreadId::from_string(target).is_err() {
        anyhow::bail!("--force requires a session UUID; names must be confirmed interactively");
    }
    let confirmation = match force {
        true => codex_tui::DeleteConfirmation::Skip,
        false => codex_tui::DeleteConfirmation::Prompt,
    };
    Ok(codex_tui::SessionArchiveAction::Delete(confirmation))
}

async fn run_debug_app_server_command(cmd: DebugAppServerCommand) -> anyhow::Result<()> {
    match cmd.subcommand {
        DebugAppServerSubcommand::SendMessageV2(cmd) => {
            let codex_bin = std::env::current_exe()?;
            codex_app_server_test_client::send_message_v2(&codex_bin, &[], cmd.user_message, &None)
                .await
        }
    }
}

#[derive(Debug, Default, Parser, Clone)]
struct FeatureToggles {
    /// Enable a feature (repeatable). Equivalent to `-c features.<name>=true`.
    #[arg(long = "enable", value_name = "FEATURE", action = clap::ArgAction::Append, global = true)]
    enable: Vec<String>,

    /// Disable a feature (repeatable). Equivalent to `-c features.<name>=false`.
    #[arg(long = "disable", value_name = "FEATURE", action = clap::ArgAction::Append, global = true)]
    disable: Vec<String>,
}

#[derive(Debug, Default, Parser, Clone)]
struct InteractiveRemoteOptions {
    /// Connect the TUI to a remote app server endpoint.
    ///
    /// Accepted forms: literal `ws://LOOPBACK_IP:PORT`, literal
    /// `wss://LOOPBACK_IP:PORT`, `unix://`, or `unix://PATH`.
    #[arg(long = "remote", value_name = "ADDR")]
    remote: Option<String>,

    /// Name of the environment variable containing the bearer token to send to
    /// a remote app server websocket.
    #[arg(long = "remote-auth-token-env", value_name = "ENV_VAR")]
    remote_auth_token_env: Option<String>,
}

impl FeatureToggles {
    fn to_overrides(&self) -> anyhow::Result<Vec<String>> {
        let mut v = Vec::new();
        for feature in &self.enable {
            Self::validate_feature(feature)?;
            v.push(format!("features.{feature}=true"));
        }
        for feature in &self.disable {
            Self::validate_feature(feature)?;
            v.push(format!("features.{feature}=false"));
        }
        Ok(v)
    }

    fn validate_feature(feature: &str) -> anyhow::Result<()> {
        if is_known_feature_key(feature) {
            Ok(())
        } else {
            anyhow::bail!("Unknown feature flag: {feature}")
        }
    }
}

#[derive(Debug, Parser)]
struct FeaturesCli {
    #[command(subcommand)]
    sub: FeaturesSubcommand,
}

#[derive(Debug, Parser)]
enum FeaturesSubcommand {
    /// List known features with their stage and effective state.
    List,
    /// Enable a feature in config.toml.
    Enable(FeatureSetArgs),
    /// Disable a feature in config.toml.
    Disable(FeatureSetArgs),
}

#[derive(Debug, Parser)]
struct FeatureSetArgs {
    /// Feature key to update (for example: unified_exec).
    feature: String,
}

fn stage_str(stage: Stage) -> &'static str {
    match stage {
        Stage::UnderDevelopment => "under development",
        Stage::Experimental { .. } => "experimental",
        Stage::Stable => "stable",
        Stage::Deprecated => "deprecated",
        Stage::Removed => "removed",
    }
}

fn main() -> anyhow::Result<()> {
    codex_whisply::seal_inherited_runtime_descriptors()?;
    let remote_control_disabled = codex_app_server::take_remote_control_disabled_env();
    arg0_dispatch_or_else(move |arg0_paths: Arg0DispatchPaths| async move {
        cli_main(arg0_paths, remote_control_disabled).await?;
        Ok(())
    })
}

async fn cli_main(
    arg0_paths: Arg0DispatchPaths,
    remote_control_disabled: bool,
) -> anyhow::Result<()> {
    let MultitoolCli {
        psp,
        config_overrides: mut root_config_overrides,
        feature_toggles,
        remote,
        mut interactive,
        subcommand,
    } = MultitoolCli::parse();
    interactive.psp = psp;

    // Fold --enable/--disable into config overrides so they flow to all subcommands.
    let toggle_overrides = feature_toggles.to_overrides()?;
    root_config_overrides.raw_overrides.extend(toggle_overrides);
    let root_remote = remote.remote;
    let root_remote_auth_token_env = remote.remote_auth_token_env;
    let root_strict_config = interactive.strict_config;
    interactive
        .shared
        .take_auto_review_config_overrides(&mut root_config_overrides);
    reject_root_strict_config_for_subcommand(root_strict_config, &subcommand)?;
    if let Some(subcommand) = subcommand.as_ref() {
        profile_v2_for_subcommand(&interactive, subcommand)?;
    }

    match subcommand {
        None => {
            prepend_config_flags(
                &mut interactive.config_overrides,
                root_config_overrides.clone(),
            );
            let exit_info = run_interactive_tui(
                interactive,
                root_remote.clone(),
                root_remote_auth_token_env.clone(),
                arg0_paths.clone(),
            )
            .await?;
            handle_app_exit(exit_info)?;
        }
        Some(Subcommand::Exec(mut exec_cli)) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "exec",
            )?;
            exec_cli
                .shared
                .inherit_exec_root_options(&interactive.shared);
            exec_cli.psp = psp;
            exec_cli.strict_config |= root_strict_config;
            prepend_config_flags(
                &mut exec_cli.config_overrides,
                root_config_overrides.clone(),
            );
            codex_exec::run_main(exec_cli, arg0_paths.clone()).await?;
        }
        Some(Subcommand::Review(ReviewCommand {
            strict_config,
            args: review_args,
        })) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "review",
            )?;
            let mut exec_cli = ExecCli::try_parse_from(["codex", "exec"])?;
            exec_cli
                .shared
                .inherit_exec_root_options(&interactive.shared);
            exec_cli.psp = psp;
            exec_cli.command = Some(ExecCommand::Review(review_args));
            exec_cli.strict_config = strict_config || root_strict_config;
            prepend_config_flags(
                &mut exec_cli.config_overrides,
                root_config_overrides.clone(),
            );
            codex_exec::run_main(exec_cli, arg0_paths.clone()).await?;
        }
        Some(Subcommand::Models(ModelsCommand { json })) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "models",
            )?;
            run_whisply_models(json).await?;
        }
        Some(Subcommand::Controls(command)) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "controls",
            )?;
            run_whisply_controls(command)?;
        }
        Some(Subcommand::Usage(UsageCommand { json })) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "usage",
            )?;
            run_whisply_usage(json)?;
        }
        Some(Subcommand::Connectors(command)) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "connectors",
            )?;
            run_whisply_connectors(command)?;
        }
        Some(Subcommand::Tools(command)) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "tools",
            )?;
            run_whisply_tools(command)?;
        }
        Some(Subcommand::McpServer(McpServerCommand { strict_config })) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "mcp-server",
            )?;
            codex_mcp_server::run_main(
                arg0_paths.clone(),
                root_config_overrides,
                strict_config || root_strict_config,
            )
            .await?;
        }
        Some(Subcommand::Mcp(mut mcp_cli)) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "mcp",
            )?;
            // Propagate any root-level config overrides (e.g. `-c key=value`).
            prepend_config_flags(&mut mcp_cli.config_overrides, root_config_overrides.clone());
            let loader_overrides =
                loader_overrides_for_profile(interactive.config_profile_v2.as_ref())?;
            mcp_cli.run(loader_overrides).await?;
        }
        Some(Subcommand::Plugin(plugin_cli)) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "plugin",
            )?;
            let PluginCli {
                mut config_overrides,
                subcommand,
            } = plugin_cli;
            prepend_config_flags(&mut config_overrides, root_config_overrides.clone());
            match subcommand {
                PluginSubcommand::Add(args) => {
                    let overrides = config_overrides
                        .parse_overrides()
                        .map_err(anyhow::Error::msg)?;
                    plugin_cmd::run_plugin_add(overrides, args).await?;
                }
                PluginSubcommand::List(args) => {
                    let overrides = config_overrides
                        .parse_overrides()
                        .map_err(anyhow::Error::msg)?;
                    plugin_cmd::run_plugin_list(overrides, args).await?;
                }
                PluginSubcommand::Marketplace(mut marketplace_cli) => {
                    prepend_config_flags(&mut marketplace_cli.config_overrides, config_overrides);
                    marketplace_cli.run().await?;
                }
                PluginSubcommand::Remove(args) => {
                    let overrides = config_overrides
                        .parse_overrides()
                        .map_err(anyhow::Error::msg)?;
                    plugin_cmd::run_plugin_remove(overrides, args).await?;
                }
            }
        }
        Some(Subcommand::AppServer(app_server_cli)) => {
            let AppServerCommand {
                subcommand,
                code_mode_host,
                strict_config: app_server_strict_config,
                listen,
                stdio,
                remote_control,
                analytics_default_enabled,
                auth,
            } = app_server_cli;
            let strict_config = app_server_strict_config || root_strict_config;
            reject_strict_config_for_app_server_subcommand(strict_config, subcommand.as_ref())?;
            reject_remote_mode_for_app_server_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                subcommand.as_ref(),
            )?;
            match subcommand {
                None => {
                    let transport = if stdio {
                        codex_app_server::AppServerTransport::Stdio
                    } else {
                        listen
                    };
                    let auth = auth.try_into_settings()?;
                    let runtime_options = codex_app_server::AppServerRuntimeOptions {
                        code_mode_host_transport: code_mode_host.into(),
                        remote_control_startup_mode: match (remote_control, remote_control_disabled)
                        {
                            (true, _) => {
                                codex_app_server::RemoteControlStartupMode::EnabledEphemeral
                            }
                            (false, true) => {
                                codex_app_server::RemoteControlStartupMode::DisabledEphemeral
                            }
                            (false, false) => {
                                codex_app_server::RemoteControlStartupMode::ResolvePersisted
                            }
                        },
                        psp,
                        ..Default::default()
                    };
                    codex_app_server::run_main_with_transport_options(
                        arg0_paths.clone(),
                        root_config_overrides,
                        LoaderOverrides::default(),
                        strict_config,
                        analytics_default_enabled,
                        transport,
                        codex_protocol::protocol::SessionSource::VSCode,
                        auth,
                        runtime_options,
                    )
                    .await?;
                }
                Some(AppServerSubcommand::Daemon(daemon_cli)) => {
                    reject_legacy_app_server_daemon_subcommand(&daemon_cli.subcommand)?;
                    match daemon_cli.subcommand {
                        AppServerDaemonSubcommand::Stop => {
                            print_app_server_daemon_output(AppServerLifecycleCommand::Stop).await?;
                        }
                        AppServerDaemonSubcommand::Version => {
                            print_app_server_daemon_output(AppServerLifecycleCommand::Version)
                                .await?;
                        }
                        AppServerDaemonSubcommand::Bootstrap(_)
                        | AppServerDaemonSubcommand::Start
                        | AppServerDaemonSubcommand::Restart
                        | AppServerDaemonSubcommand::EnableRemoteControl
                        | AppServerDaemonSubcommand::DisableRemoteControl
                        | AppServerDaemonSubcommand::PidUpdateLoop => {
                            unreachable!("rejected before standalone daemon setup")
                        }
                    }
                }
                Some(AppServerSubcommand::Proxy(proxy_cli)) => {
                    let socket_path = match proxy_cli.socket_path {
                        Some(socket_path) => socket_path,
                        None => {
                            let codex_home = find_codex_home()?;
                            codex_app_server::app_server_control_socket_path(&codex_home)?
                        }
                    };
                    codex_stdio_to_uds::run(socket_path.as_path()).await?;
                }
                Some(AppServerSubcommand::GenerateTs(gen_cli)) => {
                    let options = codex_app_server_protocol::GenerateTsOptions {
                        experimental_api: gen_cli.experimental,
                        ..Default::default()
                    };
                    codex_app_server_protocol::generate_ts_with_options(
                        &gen_cli.out_dir,
                        gen_cli.prettier.as_deref(),
                        options,
                    )?;
                }
                Some(AppServerSubcommand::GenerateJsonSchema(gen_cli)) => {
                    codex_app_server_protocol::generate_json_with_experimental(
                        &gen_cli.out_dir,
                        gen_cli.experimental,
                    )?;
                }
                Some(AppServerSubcommand::GenerateInternalJsonSchema(gen_cli)) => {
                    codex_app_server_protocol::generate_internal_json_schema(&gen_cli.out_dir)?;
                }
            }
        }
        Some(Subcommand::RemoteControl(_)) => reject_remote_control_for_whisply()?,
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        Some(Subcommand::App(app_cli)) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "app",
            )?;
            app_cmd::run_app(app_cli).await?;
        }
        Some(Subcommand::Resume(ResumeCommand {
            session_id,
            last,
            all,
            include_non_interactive,
            remote,
            config_overrides,
        })) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "resume",
            )?;
            reject_remote_mode_for_subcommand(
                remote.remote.as_deref(),
                remote.remote_auth_token_env.as_deref(),
                "resume",
            )?;
            let SessionTuiCli(config_overrides) = config_overrides;
            interactive = finalize_resume_interactive(
                interactive,
                root_config_overrides.clone(),
                session_id,
                last,
                all,
                include_non_interactive,
                config_overrides,
            );
            configure_managed_session_provider(&mut interactive, "resume")?;
            let exit_info = run_interactive_tui(
                interactive,
                remote.remote.or(root_remote.clone()),
                remote
                    .remote_auth_token_env
                    .or(root_remote_auth_token_env.clone()),
                arg0_paths.clone(),
            )
            .await?;
            handle_app_exit(exit_info)?;
        }
        Some(Subcommand::Archive(cmd)) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "archive",
            )?;
            reject_remote_mode_for_subcommand(
                cmd.remote.remote.as_deref(),
                cmd.remote.remote_auth_token_env.as_deref(),
                "archive",
            )?;
            let output = run_session_archive_cli_command(
                codex_tui::SessionArchiveAction::Archive,
                cmd,
                interactive,
                root_config_overrides.clone(),
                root_remote.clone(),
                root_remote_auth_token_env.clone(),
                arg0_paths.clone(),
            )
            .await?;
            println!("{output}");
        }
        Some(Subcommand::Delete(DeleteCommand { session, force })) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "delete",
            )?;
            reject_remote_mode_for_subcommand(
                session.remote.remote.as_deref(),
                session.remote.remote_auth_token_env.as_deref(),
                "delete",
            )?;
            let action = delete_action(&session.target, force)?;
            let output = run_session_archive_cli_command(
                action,
                session,
                interactive,
                root_config_overrides.clone(),
                root_remote.clone(),
                root_remote_auth_token_env.clone(),
                arg0_paths.clone(),
            )
            .await?;
            println!("{output}");
        }
        Some(Subcommand::Unarchive(cmd)) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "unarchive",
            )?;
            reject_remote_mode_for_subcommand(
                cmd.remote.remote.as_deref(),
                cmd.remote.remote_auth_token_env.as_deref(),
                "unarchive",
            )?;
            let output = run_session_archive_cli_command(
                codex_tui::SessionArchiveAction::Unarchive,
                cmd,
                interactive,
                root_config_overrides.clone(),
                root_remote.clone(),
                root_remote_auth_token_env.clone(),
                arg0_paths.clone(),
            )
            .await?;
            println!("{output}");
        }
        Some(Subcommand::Fork(ForkCommand {
            session_id,
            last,
            all,
            remote,
            config_overrides,
        })) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "fork",
            )?;
            reject_remote_mode_for_subcommand(
                remote.remote.as_deref(),
                remote.remote_auth_token_env.as_deref(),
                "fork",
            )?;
            let SessionTuiCli(config_overrides) = config_overrides;
            interactive = finalize_fork_interactive(
                interactive,
                root_config_overrides.clone(),
                session_id,
                last,
                all,
                config_overrides,
            );
            configure_managed_session_provider(&mut interactive, "fork")?;
            let exit_info = run_interactive_tui(
                interactive,
                remote.remote.or(root_remote.clone()),
                remote
                    .remote_auth_token_env
                    .or(root_remote_auth_token_env.clone()),
                arg0_paths.clone(),
            )
            .await?;
            handle_app_exit(exit_info)?;
        }
        Some(Subcommand::Sessions(SessionsCommand { action })) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "sessions",
            )?;
            match action {
                SessionsSubcommand::List(SessionsListCommand {
                    archived,
                    all,
                    limit,
                    search,
                    json,
                    remote,
                    config_overrides,
                }) => {
                    reject_remote_mode_for_subcommand(
                        remote.remote.as_deref(),
                        remote.remote_auth_token_env.as_deref(),
                        "sessions list",
                    )?;
                    let scope = if all {
                        codex_tui::SessionCollectionScope::All
                    } else if archived {
                        codex_tui::SessionCollectionScope::Archived
                    } else {
                        codex_tui::SessionCollectionScope::Active
                    };
                    let options = managed_session_command_options(
                        interactive,
                        root_config_overrides.clone(),
                        remote,
                        config_overrides,
                        root_remote.clone(),
                        root_remote_auth_token_env.clone(),
                        arg0_paths.clone(),
                    )?;
                    let entries =
                        codex_tui::run_session_list_command(options, scope, limit, search)
                            .await
                            .map_err(|error| anyhow::anyhow!("{error}"))?;
                    print_session_list(&entries, json)?;
                }
                SessionsSubcommand::Export(SessionsExportCommand {
                    target,
                    output,
                    remote,
                    config_overrides,
                }) => {
                    reject_remote_mode_for_subcommand(
                        remote.remote.as_deref(),
                        remote.remote_auth_token_env.as_deref(),
                        "sessions export",
                    )?;
                    let options = managed_session_command_options(
                        interactive,
                        root_config_overrides.clone(),
                        remote,
                        config_overrides,
                        root_remote.clone(),
                        root_remote_auth_token_env.clone(),
                        arg0_paths.clone(),
                    )?;
                    let export = codex_tui::run_session_export_command(options, target)
                        .await
                        .map_err(|error| anyhow::anyhow!("{error}"))?;
                    write_session_export(&export, output)?;
                }
            }
        }
        Some(Subcommand::Skills(command)) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "skills",
            )?;
            whisply_skills::run(command)?;
        }
        Some(Subcommand::Config(command)) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "config",
            )?;
            whisply_config::run_config(command).await?;
        }
        Some(Subcommand::Profile(command)) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "profile",
            )?;
            whisply_config::run_profile(command).await?;
        }
        Some(Subcommand::Diagnostics(command)) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "diagnostics",
            )?;
            whisply_diagnostics::run(command)?;
        }
        Some(Subcommand::Test(command)) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "test release",
            )?;
            reject_release_test_overrides(psp, &root_config_overrides)?;
            whisply_verify::run_test(command)?;
        }
        Some(Subcommand::Login(login_cli)) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "login",
            )?;
            match login_cli.action {
                Some(LoginSubcommand::Status) => {
                    print_whisply_login_status()?;
                }
                Some(LoginSubcommand::Whoami) => print_whisply_whoami()?,
                None => {
                    if login_cli.with_api_key
                        || login_cli.with_access_token
                        || login_cli.api_key.is_some()
                        || login_cli.use_device_code
                        || login_cli.issuer_base_url.is_some()
                        || login_cli.client_id.is_some()
                    {
                        eprintln!(
                            "Whisply does not accept API keys, access tokens, or custom OAuth settings in the runtime. Sign in through the managed Whisply app."
                        );
                        std::process::exit(2);
                    } else {
                        begin_whisply_browser_login()?;
                    }
                }
            }
        }
        Some(Subcommand::Logout(logout_cli)) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "logout",
            )?;
            let _ = logout_cli.config_overrides;
            let broker = native_broker_for_cli()?;
            let status = broker.status().map_err(broker_cli_error)?;
            broker
                .logout(status.account_epoch.as_deref())
                .map_err(broker_cli_error)?;
            println!("Signed out of the managed Whisply account.");
        }
        Some(Subcommand::Whoami) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "whoami",
            )?;
            print_whisply_whoami()?;
        }
        Some(Subcommand::Completion(completion_cli)) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "completion",
            )?;
            print_completion(completion_cli);
        }
        Some(Subcommand::Update) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "update",
            )?;
            run_update_command()?;
        }
        Some(Subcommand::Version(VersionCommand { verbose })) => {
            print_whisply_version(verbose)?;
        }
        Some(Subcommand::Doctor(doctor_cli)) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "doctor",
            )?;
            // Type-erase this sizeable diagnostics future at the CLI boundary;
            // it keeps command dispatch independent from doctor internals.
            let doctor_result = Box::pin(doctor::run_doctor(
                doctor_cli,
                root_config_overrides.clone(),
                &interactive,
                &arg0_paths,
            ))
            .await;
            doctor_result?;
        }
        Some(Subcommand::Cloud(_)) => reject_cloud_tasks_for_whisply()?,
        Some(Subcommand::Sandbox(mut sandbox_cli)) => {
            let config_profile = sandbox_cli
                .config_profile
                .as_ref()
                .or(interactive.config_profile_v2.as_ref());
            prepend_config_flags(
                &mut sandbox_cli.config_overrides,
                root_config_overrides.clone(),
            );
            #[cfg(target_os = "windows")]
            if let Some(setup_cli) = sandbox_setup::parse_setup_command(&sandbox_cli.command)? {
                reject_remote_mode_for_subcommand(
                    root_remote.as_deref(),
                    root_remote_auth_token_env.as_deref(),
                    "sandbox setup",
                )?;
                let cli_overrides = sandbox_cli
                    .config_overrides
                    .parse_overrides()
                    .map_err(anyhow::Error::msg)?;
                sandbox_setup::run(setup_cli, config_profile.cloned(), cli_overrides).await?;
                return Ok(());
            }
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "sandbox",
            )?;
            let loader_overrides = loader_overrides_for_profile(config_profile)?;
            #[cfg(target_os = "macos")]
            codex_cli::run_command_under_seatbelt(
                sandbox_cli,
                arg0_paths.codex_linux_sandbox_exe.clone(),
                loader_overrides,
            )
            .await?;
            #[cfg(target_os = "linux")]
            codex_cli::run_command_under_landlock(
                sandbox_cli,
                arg0_paths.codex_linux_sandbox_exe.clone(),
                loader_overrides,
            )
            .await?;
            #[cfg(target_os = "windows")]
            codex_cli::run_command_under_windows_sandbox(
                sandbox_cli,
                arg0_paths.codex_linux_sandbox_exe.clone(),
                loader_overrides,
            )
            .await?;
            #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
            {
                let _ = loader_overrides;
                anyhow::bail!("`codex sandbox` is not supported on this operating system");
            }
        }
        Some(Subcommand::Debug(DebugCommand { subcommand })) => match subcommand {
            DebugSubcommand::Models(cmd) => {
                reject_remote_mode_for_subcommand(
                    root_remote.as_deref(),
                    root_remote_auth_token_env.as_deref(),
                    "debug models",
                )?;
                run_debug_models_command(cmd).await?;
            }
            DebugSubcommand::AppServer(cmd) => {
                reject_remote_mode_for_subcommand(
                    root_remote.as_deref(),
                    root_remote_auth_token_env.as_deref(),
                    "debug app-server",
                )?;
                run_debug_app_server_command(cmd).await?;
            }
            DebugSubcommand::PromptInput(cmd) => {
                reject_remote_mode_for_subcommand(
                    root_remote.as_deref(),
                    root_remote_auth_token_env.as_deref(),
                    "debug prompt-input",
                )?;
                run_debug_prompt_input_command(
                    cmd,
                    root_config_overrides,
                    interactive,
                    arg0_paths.clone(),
                )
                .await?;
            }
            DebugSubcommand::Verify(command) => {
                reject_remote_mode_for_subcommand(
                    root_remote.as_deref(),
                    root_remote_auth_token_env.as_deref(),
                    "debug verify",
                )?;
                whisply_verify::run(command)?;
            }
            DebugSubcommand::TraceReduce(cmd) => {
                reject_remote_mode_for_subcommand(
                    root_remote.as_deref(),
                    root_remote_auth_token_env.as_deref(),
                    "debug trace-reduce",
                )?;
                run_debug_trace_reduce_command(cmd).await?;
            }
            DebugSubcommand::ClearMemories => {
                reject_remote_mode_for_subcommand(
                    root_remote.as_deref(),
                    root_remote_auth_token_env.as_deref(),
                    "debug clear-memories",
                )?;
                run_debug_clear_memories_command(&root_config_overrides).await?;
            }
        },
        Some(Subcommand::Execpolicy(ExecpolicyCommand { sub })) => match sub {
            ExecpolicySubcommand::Check(cmd) => {
                reject_remote_mode_for_subcommand(
                    root_remote.as_deref(),
                    root_remote_auth_token_env.as_deref(),
                    "execpolicy check",
                )?;
                run_execpolicycheck(cmd)?
            }
        },
        Some(Subcommand::Apply(mut apply_cli)) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "apply",
            )?;
            prepend_config_flags(
                &mut apply_cli.config_overrides,
                root_config_overrides.clone(),
            );
            run_apply_command(apply_cli, /*cwd*/ None).await?;
        }
        Some(Subcommand::ResponsesApiProxy(_)) => reject_responses_api_proxy_for_whisply()?,
        Some(Subcommand::StdioToUds(cmd)) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "stdio-to-uds",
            )?;
            let socket_path = cmd.socket_path;
            codex_stdio_to_uds::run(socket_path.as_path()).await?;
        }
        Some(Subcommand::ExecServer(cmd)) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "exec-server",
            )?;
            let strict_config = cmd.strict_config || root_strict_config;
            run_exec_server_command(cmd, &arg0_paths, &root_config_overrides, strict_config)
                .await?;
        }
        Some(Subcommand::Features(FeaturesCli { sub })) => match sub {
            FeaturesSubcommand::List => {
                reject_remote_mode_for_subcommand(
                    root_remote.as_deref(),
                    root_remote_auth_token_env.as_deref(),
                    "features list",
                )?;
                let mut cli_kv_overrides = root_config_overrides
                    .parse_overrides()
                    .map_err(anyhow::Error::msg)?;

                // Honor `--search` via the canonical web_search mode.
                if interactive.web_search {
                    cli_kv_overrides.push((
                        "web_search".to_string(),
                        toml::Value::String("live".to_string()),
                    ));
                }

                let config = ConfigBuilder::default()
                    .cli_overrides(cli_kv_overrides)
                    .build()
                    .await?;
                let mut rows = Vec::with_capacity(FEATURES.len());
                let mut name_width = 0;
                let mut stage_width = 0;
                for def in FEATURES {
                    let name = def.key;
                    let stage = stage_str(def.stage);
                    let enabled = config.features.enabled(def.id);
                    name_width = name_width.max(name.len());
                    stage_width = stage_width.max(stage.len());
                    rows.push((name, stage, enabled));
                }
                rows.sort_unstable_by_key(|(name, _, _)| *name);

                for (name, stage, enabled) in rows {
                    println!("{name:<name_width$}  {stage:<stage_width$}  {enabled}");
                }
            }
            FeaturesSubcommand::Enable(FeatureSetArgs { feature }) => {
                reject_remote_mode_for_subcommand(
                    root_remote.as_deref(),
                    root_remote_auth_token_env.as_deref(),
                    "features enable",
                )?;
                enable_feature_in_config(&feature).await?;
            }
            FeaturesSubcommand::Disable(FeatureSetArgs { feature }) => {
                reject_remote_mode_for_subcommand(
                    root_remote.as_deref(),
                    root_remote_auth_token_env.as_deref(),
                    "features disable",
                )?;
                disable_feature_in_config(&feature).await?;
            }
        },
    }

    Ok(())
}

fn profile_v2_for_subcommand<'a>(
    interactive: &'a TuiCli,
    subcommand: &Subcommand,
) -> anyhow::Result<Option<&'a ProfileV2Name>> {
    let Some(profile_v2) = interactive.config_profile_v2.as_ref() else {
        return Ok(None);
    };

    match subcommand {
        Subcommand::Exec(_)
        | Subcommand::Review(_)
        | Subcommand::Resume(_)
        | Subcommand::Archive(_)
        | Subcommand::Delete(_)
        | Subcommand::Unarchive(_)
        | Subcommand::Fork(_)
        | Subcommand::Sessions(_)
        | Subcommand::Mcp(_)
        | Subcommand::Sandbox(_)
        | Subcommand::Debug(DebugCommand {
            subcommand: DebugSubcommand::PromptInput(_),
        }) => Ok(Some(profile_v2)),
        _ => anyhow::bail!(
            "--profile only applies to runtime commands and `codex mcp`: `codex`, `codex exec`, `codex review`, `codex resume`, `codex archive`, `codex delete`, `codex unarchive`, `codex fork`, `codex sessions`, `codex mcp`, `codex sandbox`, and `codex debug prompt-input`."
        ),
    }
}

/// `whisply test release` is a fixed, zero-authority plan. Root configuration
/// and process-only routing flags would make that plan caller-controlled, so
/// reject them before the command reaches the release-test boundary.
fn reject_release_test_overrides(
    psp: bool,
    root_config_overrides: &CliConfigOverrides,
) -> anyhow::Result<()> {
    if psp || !root_config_overrides.raw_overrides.is_empty() {
        anyhow::bail!(
            "`whisply test release` uses a fixed plan and does not accept process routing, configuration, or feature overrides"
        );
    }
    Ok(())
}

fn print_whisply_version(verbose: bool) -> anyhow::Result<()> {
    if !verbose {
        println!("{PRODUCT_NAME} {WHISPLY_RUNTIME_VERSION}");
        return Ok(());
    }

    let tool_registry = first_party_tool_registry();
    let report = serde_json::json!({
        "product": PRODUCT_NAME,
        "executable": "whisply",
        "runtimeVersion": WHISPLY_RUNTIME_VERSION,
        "upstream": {
            "tag": UPSTREAM_TAG,
            "commit": UPSTREAM_COMMIT,
        },
        "manifestSource": "Runtime/upstream-base.json",
        "protocolSchemaSource": "Runtime/whisply-codex/codex-rs/app-server-protocol/schema/json/codex_app_server_protocol.v2.schemas.json",
        "toolRegistry": {
            "source": "Runtime/whisply-codex/codex-rs/whisply-runtime/src/tools.rs",
            "sha256": tool_registry.hash()?,
        },
    });
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

async fn run_exec_server_command(
    cmd: ExecServerCommand,
    arg0_paths: &Arg0DispatchPaths,
    root_config_overrides: &CliConfigOverrides,
    strict_config: bool,
) -> anyhow::Result<()> {
    let codex_self_exe = arg0_paths
        .codex_self_exe
        .clone()
        .ok_or_else(|| anyhow::anyhow!("Codex executable path is not configured"))?;
    let runtime_paths = codex_exec_server::ExecServerRuntimePaths::new(
        codex_self_exe,
        arg0_paths.codex_linux_sandbox_exe.clone(),
    )?;
    let config_result = load_exec_server_config(root_config_overrides, strict_config).await;
    let config = if strict_config {
        Some(config_result?)
    } else {
        config_result.ok()
    };
    let (_otel, telemetry) = exec_server_telemetry::init(config.as_ref());
    let http_client_factory = config
        .as_ref()
        .map(codex_core::config::Config::http_client_factory)
        .unwrap_or_else(|| {
            codex_http_client::HttpClientFactory::new(
                codex_http_client::OutboundProxyPolicy::ReqwestDefault,
            )
        });
    let listen_url = cmd
        .listen
        .unwrap_or_else(|| codex_exec_server::DEFAULT_LISTEN_URL.to_string());
    exec_server_telemetry::run_until_shutdown(async move {
        codex_exec_server::run_main_with_telemetry(
            &listen_url,
            runtime_paths,
            telemetry,
            http_client_factory,
            cmd.request_dispatch_mode,
        )
        .await
    })
    .await
    .map_err(anyhow::Error::from_boxed)
}

async fn load_exec_server_config(
    root_config_overrides: &CliConfigOverrides,
    strict_config: bool,
) -> anyhow::Result<codex_core::config::Config> {
    let cli_kv_overrides = root_config_overrides
        .parse_overrides()
        .map_err(anyhow::Error::msg)?;
    Ok(ConfigBuilder::default()
        .cli_overrides(cli_kv_overrides)
        .strict_config(strict_config)
        .build()
        .await?)
}

async fn enable_feature_in_config(feature: &str) -> anyhow::Result<()> {
    FeatureToggles::validate_feature(feature)?;
    let codex_home = find_codex_home()?;
    ConfigEditsBuilder::new(&codex_home)
        .set_feature_enabled(feature, /*enabled*/ true)
        .apply()
        .await?;
    println!("Enabled feature `{feature}` in config.toml.");
    maybe_print_under_development_feature_warning(&codex_home, feature);
    Ok(())
}

async fn disable_feature_in_config(feature: &str) -> anyhow::Result<()> {
    FeatureToggles::validate_feature(feature)?;
    let codex_home = find_codex_home()?;
    ConfigEditsBuilder::new(&codex_home)
        .set_feature_enabled(feature, /*enabled*/ false)
        .apply()
        .await?;
    println!("Disabled feature `{feature}` in config.toml.");
    Ok(())
}

fn loader_overrides_for_profile(
    profile_v2: Option<&ProfileV2Name>,
) -> anyhow::Result<LoaderOverrides> {
    match profile_v2 {
        Some(profile_v2) => {
            let codex_home = find_codex_home()?;
            Ok(loader_overrides_for_profile_at_codex_home(
                Some(profile_v2),
                &codex_home,
            ))
        }
        None => Ok(LoaderOverrides::default()),
    }
}

fn loader_overrides_for_profile_at_codex_home(
    profile_v2: Option<&ProfileV2Name>,
    codex_home: &std::path::Path,
) -> LoaderOverrides {
    match profile_v2 {
        Some(profile_v2) => LoaderOverrides {
            user_config_path: Some(resolve_profile_v2_config_path(codex_home, profile_v2)),
            user_config_profile: Some(profile_v2.clone()),
            ..Default::default()
        },
        None => LoaderOverrides::default(),
    }
}

fn maybe_print_under_development_feature_warning(codex_home: &std::path::Path, feature: &str) {
    let Some(spec) = FEATURES.iter().find(|spec| spec.key == feature) else {
        return;
    };
    if !matches!(spec.stage, Stage::UnderDevelopment) {
        return;
    }

    let config_path = codex_home.join(codex_config::CONFIG_TOML_FILE);
    eprintln!(
        "Under-development features enabled: {feature}. Under-development features are incomplete and may behave unpredictably. To suppress this warning, set `suppress_unstable_features_warning = true` in {}.",
        config_path.display()
    );
}

async fn run_debug_trace_reduce_command(cmd: DebugTraceReduceCommand) -> anyhow::Result<()> {
    let output = cmd
        .output
        .unwrap_or_else(|| cmd.trace_bundle.join(REDUCED_STATE_FILE_NAME));

    let trace = replay_bundle(&cmd.trace_bundle)?;
    let reduced_json = serde_json::to_vec_pretty(&trace)?;
    tokio::fs::write(&output, reduced_json).await?;
    println!("{}", output.display());

    Ok(())
}

async fn run_debug_prompt_input_command(
    cmd: DebugPromptInputCommand,
    root_config_overrides: CliConfigOverrides,
    interactive: TuiCli,
    arg0_paths: Arg0DispatchPaths,
) -> anyhow::Result<()> {
    let loader_overrides = loader_overrides_for_profile(interactive.config_profile_v2.as_ref())?;
    let shared = interactive.shared.into_inner();
    let mut cli_kv_overrides = root_config_overrides
        .parse_overrides()
        .map_err(anyhow::Error::msg)?;
    if interactive.web_search {
        cli_kv_overrides.push((
            "web_search".to_string(),
            toml::Value::String("live".to_string()),
        ));
    }

    let approval_policy = if shared.dangerously_bypass_approvals_and_sandbox {
        Some(AskForApproval::Never)
    } else {
        interactive.approval_policy.map(Into::into)
    };
    let sandbox_mode = if shared.dangerously_bypass_approvals_and_sandbox {
        Some(codex_protocol::config_types::SandboxMode::DangerFullAccess)
    } else {
        shared.sandbox_mode.map(Into::into)
    };
    let overrides = ConfigOverrides {
        model: shared.model,
        approval_policy,
        sandbox_mode,
        cwd: shared.cwd,
        codex_self_exe: arg0_paths.codex_self_exe,
        codex_linux_sandbox_exe: arg0_paths.codex_linux_sandbox_exe,
        main_execve_wrapper_exe: arg0_paths.main_execve_wrapper_exe,
        show_raw_agent_reasoning: shared.oss.then_some(true),
        ephemeral: Some(true),
        bypass_hook_trust: shared.bypass_hook_trust.then_some(true),
        additional_writable_roots: shared.add_dir,
        ..Default::default()
    };
    let config = ConfigBuilder::default()
        .cli_overrides(cli_kv_overrides)
        .harness_overrides(overrides)
        .loader_overrides(loader_overrides)
        .build()
        .await?;

    let mut input = shared
        .images
        .into_iter()
        .chain(cmd.images)
        .map(|path| UserInput::LocalImage { path, detail: None })
        .collect::<Vec<_>>();
    if let Some(prompt) = cmd.prompt.or(interactive.prompt) {
        input.push(UserInput::Text {
            text: prompt.replace("\r\n", "\n").replace('\r', "\n"),
            text_elements: Vec::new(),
        });
    }

    let user_instructions_provider = Arc::new(CodexHomeUserInstructionsProvider::new(
        config.codex_home.clone(),
    ));
    let mut extensions = codex_extension_api::ExtensionRegistryBuilder::new();
    codex_skills_extension::install(&mut extensions, |config: &Config| {
        codex_skills_extension::SkillsExtensionConfig {
            include_instructions: config.include_skill_instructions,
            bundled_skills_enabled: config.bundled_skills_enabled(),
            orchestrator_skills_enabled: config.orchestrator_skills_enabled,
            shadow_selection_enabled: config
                .features
                .enabled(codex_features::Feature::SkillSearch),
        }
    });
    let prompt_input = codex_core::build_prompt_input(
        config,
        input,
        /*state_db*/ None,
        Arc::new(extensions.build()),
        user_instructions_provider,
    )
    .await?;
    println!("{}", serde_json::to_string_pretty(&prompt_input)?);

    Ok(())
}

async fn run_debug_models_command(cmd: DebugModelsCommand) -> anyhow::Result<()> {
    let _ = cmd;
    // Keep the old hidden route as a compatibility spelling, but make its
    // output use the same signed managed catalog as `whisply models --json`.
    run_whisply_models(/*json*/ true).await
}

async fn run_debug_clear_memories_command(
    root_config_overrides: &CliConfigOverrides,
) -> anyhow::Result<()> {
    let cli_kv_overrides = root_config_overrides
        .parse_overrides()
        .map_err(anyhow::Error::msg)?;
    let config = ConfigBuilder::default()
        .cli_overrides(cli_kv_overrides)
        .build()
        .await?;

    let memories_path = config.sqlite_config().memories_db_path();
    let cleared_memories_db =
        StateRuntime::clear_memory_data_in_sqlite_home(config.sqlite_config()).await?;

    clear_memory_roots_contents(&config.codex_home).await?;

    let mut message = if cleared_memories_db {
        format!("Cleared memory state from {}.", memories_path.display())
    } else {
        format!("No memories db found at {}.", memories_path.display())
    };
    message.push_str(&format!(
        " Cleared memory directories under {}.",
        config.codex_home.display()
    ));

    println!("{message}");

    Ok(())
}

/// Prepend root-level overrides so they have lower precedence than
/// CLI-specific ones specified after the subcommand (if any).
fn prepend_config_flags(
    subcommand_config_overrides: &mut CliConfigOverrides,
    cli_config_overrides: CliConfigOverrides,
) {
    subcommand_config_overrides.prepend_root_overrides(cli_config_overrides);
}

fn reject_remote_mode_for_subcommand(
    remote: Option<&str>,
    remote_auth_token_env: Option<&str>,
    subcommand: &str,
) -> anyhow::Result<()> {
    if let Some(remote) = remote {
        anyhow::bail!(
            "`--remote {remote}` is only supported for interactive TUI commands, not `codex {subcommand}`"
        );
    }
    if remote_auth_token_env.is_some() {
        anyhow::bail!(
            "`--remote-auth-token-env` is only supported for interactive TUI commands, not `codex {subcommand}`"
        );
    }
    Ok(())
}

fn reject_cloud_tasks_for_whisply() -> anyhow::Result<()> {
    anyhow::bail!(
        "`whisply cloud` is unavailable because cloud tasks require direct ChatGPT authority"
    );
}

fn reject_responses_api_proxy_for_whisply() -> anyhow::Result<()> {
    anyhow::bail!("`whisply responses-api-proxy` is unavailable; use the managed Whisply gateway");
}

fn reject_remote_control_for_whisply() -> anyhow::Result<()> {
    anyhow::bail!(
        "`whisply remote-control` is unavailable; remote control is owned by the managed Whisply app"
    );
}

fn reject_legacy_app_server_daemon_subcommand(
    subcommand: &AppServerDaemonSubcommand,
) -> anyhow::Result<()> {
    let name = match subcommand {
        AppServerDaemonSubcommand::Bootstrap(_) => "bootstrap",
        AppServerDaemonSubcommand::Start => "start",
        AppServerDaemonSubcommand::Restart => "restart",
        AppServerDaemonSubcommand::EnableRemoteControl => "enable-remote-control",
        AppServerDaemonSubcommand::DisableRemoteControl => "disable-remote-control",
        AppServerDaemonSubcommand::PidUpdateLoop => "pid-update-loop",
        AppServerDaemonSubcommand::Stop | AppServerDaemonSubcommand::Version => return Ok(()),
    };
    anyhow::bail!(
        "`whisply app-server daemon {name}` is unavailable because standalone daemon management can start remote control or an updater"
    );
}

fn reject_root_strict_config_for_subcommand(
    strict_config: bool,
    subcommand: &Option<Subcommand>,
) -> anyhow::Result<()> {
    if !strict_config {
        return Ok(());
    }

    match unsupported_subcommand_name_for_strict_config(subcommand) {
        Some(subcommand_name) => {
            reject_strict_config_for_unsupported_subcommand(strict_config, subcommand_name)
        }
        None => Ok(()),
    }
}

/// Return the selected subcommand name when a root-level `--strict-config`
/// flag should be rejected after parsing.
///
/// `--strict-config` is parsed on the root interactive CLI so commands like
/// `codex --strict-config` continue to work for the TUI and for wrappers that
/// forward root options into another command shape. Clap will still accept that
/// root flag before the dispatcher knows which subcommand the user selected, so
/// unsupported subcommands need an explicit post-parse reject path.
///
/// `Some(...)` returns the user-facing command name fragment to embed in the
/// rejection error, such as `cloud` or `app-server proxy`. `None` means the
/// selected command is allowed to inherit root `--strict-config`.
fn unsupported_subcommand_name_for_strict_config(
    subcommand: &Option<Subcommand>,
) -> Option<&'static str> {
    match subcommand {
        None
        | Some(Subcommand::Exec(_))
        | Some(Subcommand::Review(_))
        | Some(Subcommand::McpServer(_))
        | Some(Subcommand::ExecServer(_))
        | Some(Subcommand::Resume(_))
        | Some(Subcommand::Archive(_))
        | Some(Subcommand::Delete(_))
        | Some(Subcommand::Unarchive(_))
        | Some(Subcommand::Fork(_))
        | Some(Subcommand::Sessions(_))
        | Some(Subcommand::Doctor(_)) => None,
        Some(Subcommand::AppServer(app_server)) if app_server.subcommand.is_none() => None,
        Some(Subcommand::AppServer(app_server)) => {
            Some(app_server_subcommand_name(app_server.subcommand.as_ref()))
        }
        Some(Subcommand::RemoteControl(_)) => Some("remote-control"),
        Some(Subcommand::Mcp(_)) => Some("mcp"),
        Some(Subcommand::Plugin(_)) => Some("plugin"),
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        Some(Subcommand::App(_)) => Some("app"),
        Some(Subcommand::Login(_)) => Some("login"),
        Some(Subcommand::Logout(_)) => Some("logout"),
        Some(Subcommand::Whoami) => Some("whoami"),
        Some(Subcommand::Models(_)) => Some("models"),
        Some(Subcommand::Controls(_)) => Some("controls"),
        Some(Subcommand::Usage(_)) => Some("usage"),
        Some(Subcommand::Connectors(_)) => Some("connectors"),
        Some(Subcommand::Tools(_)) => Some("tools"),
        Some(Subcommand::Skills(_)) => Some("skills"),
        Some(Subcommand::Config(_)) => Some("config"),
        Some(Subcommand::Profile(_)) => Some("profile"),
        Some(Subcommand::Diagnostics(_)) => Some("diagnostics"),
        Some(Subcommand::Test(_)) => Some("test release"),
        Some(Subcommand::Completion(_)) => Some("completion"),
        Some(Subcommand::Update) => Some("update"),
        Some(Subcommand::Version(_)) => Some("version"),
        Some(Subcommand::Cloud(_)) => Some("cloud"),
        Some(Subcommand::Sandbox(_)) => Some("sandbox"),
        Some(Subcommand::Debug(_)) => Some("debug"),
        Some(Subcommand::Execpolicy(_)) => Some("execpolicy"),
        Some(Subcommand::Apply(_)) => Some("apply"),
        Some(Subcommand::ResponsesApiProxy(_)) => Some("responses-api-proxy"),
        Some(Subcommand::StdioToUds(_)) => Some("stdio-to-uds"),
        Some(Subcommand::Features(_)) => Some("features"),
    }
}

fn reject_strict_config_for_app_server_subcommand(
    strict_config: bool,
    subcommand: Option<&AppServerSubcommand>,
) -> anyhow::Result<()> {
    if subcommand.is_none() {
        return Ok(());
    }
    reject_strict_config_for_unsupported_subcommand(
        strict_config,
        app_server_subcommand_name(subcommand),
    )
}

fn reject_strict_config_for_unsupported_subcommand(
    strict_config: bool,
    subcommand: &str,
) -> anyhow::Result<()> {
    if strict_config {
        anyhow::bail!("`--strict-config` is not supported for `codex {subcommand}`");
    }
    Ok(())
}

fn reject_remote_mode_for_app_server_subcommand(
    remote: Option<&str>,
    remote_auth_token_env: Option<&str>,
    subcommand: Option<&AppServerSubcommand>,
) -> anyhow::Result<()> {
    let subcommand_name = app_server_subcommand_name(subcommand);
    reject_remote_mode_for_subcommand(remote, remote_auth_token_env, subcommand_name)
}

fn app_server_subcommand_name(subcommand: Option<&AppServerSubcommand>) -> &'static str {
    match subcommand {
        None => "app-server",
        Some(AppServerSubcommand::Daemon(daemon)) => match daemon.subcommand {
            AppServerDaemonSubcommand::Bootstrap(_) => "app-server daemon bootstrap",
            AppServerDaemonSubcommand::Start => "app-server daemon start",
            AppServerDaemonSubcommand::Restart => "app-server daemon restart",
            AppServerDaemonSubcommand::EnableRemoteControl => {
                "app-server daemon enable-remote-control"
            }
            AppServerDaemonSubcommand::DisableRemoteControl => {
                "app-server daemon disable-remote-control"
            }
            AppServerDaemonSubcommand::Stop => "app-server daemon stop",
            AppServerDaemonSubcommand::Version => "app-server daemon version",
            AppServerDaemonSubcommand::PidUpdateLoop => "app-server daemon pid-update-loop",
        },
        Some(AppServerSubcommand::Proxy(_)) => "app-server proxy",
        Some(AppServerSubcommand::GenerateTs(_)) => "app-server generate-ts",
        Some(AppServerSubcommand::GenerateJsonSchema(_)) => "app-server generate-json-schema",
        Some(AppServerSubcommand::GenerateInternalJsonSchema(_)) => {
            "app-server generate-internal-json-schema"
        }
    }
}

async fn print_app_server_daemon_output(command: AppServerLifecycleCommand) -> anyhow::Result<()> {
    let output = codex_app_server_daemon::run(command).await?;
    println!("{}", serde_json::to_string(&output)?);
    Ok(())
}

fn read_remote_auth_token_from_env_var_with<F>(
    env_var_name: &str,
    get_var: F,
) -> anyhow::Result<String>
where
    F: FnOnce(&str) -> Result<String, std::env::VarError>,
{
    let auth_token = get_var(env_var_name)
        .map_err(|_| anyhow::anyhow!("environment variable `{env_var_name}` is not set"))?;
    let auth_token = auth_token.trim().to_string();
    if auth_token.is_empty() {
        anyhow::bail!("environment variable `{env_var_name}` is empty");
    }
    Ok(auth_token)
}

fn read_remote_auth_token_from_env_var(env_var_name: &str) -> anyhow::Result<String> {
    read_remote_auth_token_from_env_var_with(env_var_name, |name| std::env::var(name))
}

async fn run_interactive_tui(
    mut interactive: TuiCli,
    remote: Option<String>,
    remote_auth_token_env: Option<String>,
    arg0_paths: Arg0DispatchPaths,
) -> std::io::Result<AppExitInfo> {
    if let Some(prompt) = interactive.prompt.take() {
        // Normalize CRLF/CR to LF so CLI-provided text can't leak `\r` into TUI state.
        interactive.prompt = Some(prompt.replace("\r\n", "\n").replace('\r', "\n"));
    }

    let terminal_info = codex_terminal_detection::terminal_info();
    if terminal_info.name == TerminalName::Dumb {
        if !(std::io::stdin().is_terminal() && std::io::stderr().is_terminal()) {
            return Ok(AppExitInfo::fatal(
                "TERM is set to \"dumb\". Refusing to start the interactive TUI because no terminal is available for a confirmation prompt (stdin/stderr is not a TTY). Run in a supported terminal or unset TERM.",
            ));
        }

        eprintln!(
            "WARNING: TERM is set to \"dumb\". Codex's interactive TUI may not work in this terminal."
        );
        if !confirm("Continue anyway? [y/N]: ")? {
            return Ok(AppExitInfo::fatal(
                "Refusing to start the interactive TUI because TERM is set to \"dumb\". Run in a supported terminal or unset TERM.",
            ));
        }
    }

    let remote_endpoint = match resolve_remote_endpoint(remote, remote_auth_token_env) {
        Ok(remote_endpoint) => remote_endpoint,
        Err(err) if is_remote_auth_usage_error(&err) => {
            return Ok(AppExitInfo::fatal(err.to_string()));
        }
        Err(err) => return Err(err),
    };
    let start_tui = || {
        codex_tui::run_main(
            interactive.clone(),
            arg0_paths.clone(),
            codex_config::LoaderOverrides::default(),
            remote_endpoint.clone(),
        )
    };
    let mut attempted_backups = HashSet::new();
    loop {
        let err = match start_tui().await {
            Ok(exit_info) => return Ok(exit_info),
            Err(err) => err,
        };
        let Some(startup_error) = local_state_db::startup_error(&err) else {
            return Err(err);
        };
        if local_state_db::is_locked(startup_error.detail()) {
            local_state_db::print_locked_guidance(startup_error);
            return Ok(AppExitInfo::fatal(startup_error.to_string()));
        }
        if !local_state_db::is_auto_backup_recoverable(startup_error) {
            local_state_db::print_diagnostic_guidance(startup_error);
            return Ok(AppExitInfo::fatal(startup_error.to_string()));
        }
        if !attempted_backups.insert(startup_error.database_path().to_path_buf()) {
            local_state_db::print_diagnostic_guidance(startup_error);
            return Ok(AppExitInfo::fatal(startup_error.to_string()));
        }

        local_state_db::print_auto_backup_start(startup_error);
        match local_state_db::backup_files_for_fresh_start(startup_error).await {
            Ok(backups) => local_state_db::confirm_fresh_start_rebuild(startup_error, &backups)?,
            Err(backup_err) => {
                local_state_db::print_diagnostic_guidance(startup_error);
                return Ok(AppExitInfo::fatal(format!(
                    "failed to move damaged Codex local database files into a backup folder automatically: {backup_err}"
                )));
            }
        }
    }
}

fn resolve_remote_endpoint(
    remote: Option<String>,
    remote_auth_token_env: Option<String>,
) -> std::io::Result<Option<codex_tui::RemoteAppServerEndpoint>> {
    resolve_remote_endpoint_with(
        remote,
        remote_auth_token_env,
        read_remote_auth_token_from_env_var,
    )
}

fn resolve_remote_endpoint_with<F>(
    remote: Option<String>,
    remote_auth_token_env: Option<String>,
    read_auth_token: F,
) -> std::io::Result<Option<codex_tui::RemoteAppServerEndpoint>>
where
    F: FnOnce(&str) -> anyhow::Result<String>,
{
    let mut remote_endpoint = remote
        .as_deref()
        .map(codex_tui::resolve_remote_addr)
        .transpose()
        .map_err(std::io::Error::other)?;
    if let Some(remote_auth_token_env) = remote_auth_token_env {
        let Some(endpoint) = remote_endpoint.as_mut() else {
            return Err(std::io::Error::other(
                "`--remote-auth-token-env` requires `--remote`.",
            ));
        };
        if !codex_tui::remote_addr_supports_auth_token(endpoint) {
            return Err(std::io::Error::other(
                "`--remote-auth-token-env` requires a literal loopback WebSocket `--remote`.",
            ));
        }
        let auth_token = read_auth_token(&remote_auth_token_env).map_err(std::io::Error::other)?;
        let codex_tui::RemoteAppServerEndpoint::WebSocket {
            auth_token: slot, ..
        } = endpoint
        else {
            return Err(std::io::Error::other(
                "`--remote-auth-token-env` requires a literal loopback WebSocket `--remote`.",
            ));
        };
        *slot = Some(auth_token);
    }
    Ok(remote_endpoint)
}

fn is_remote_auth_usage_error(err: &std::io::Error) -> bool {
    err.to_string()
        .starts_with("`--remote-auth-token-env` requires")
}

fn confirm(prompt: &str) -> std::io::Result<bool> {
    eprintln!("{prompt}");

    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    let answer = input.trim();
    Ok(answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes"))
}

/// Build the final `TuiCli` for a `codex resume` invocation.
fn finalize_resume_interactive(
    mut interactive: TuiCli,
    root_config_overrides: CliConfigOverrides,
    session_id: Option<String>,
    last: bool,
    show_all: bool,
    include_non_interactive: bool,
    mut resume_cli: TuiCli,
) -> TuiCli {
    // Start with the parsed interactive CLI so resume shares the same
    // configuration surface area as `codex` without additional flags.
    // Clap assigns the first positional to `session_id`. With `--last`, reinterpret it as the
    // prompt when no second positional prompt was provided.
    let resume_session_id = if last && resume_cli.prompt.is_none() {
        resume_cli.prompt = session_id;
        None
    } else {
        session_id
    };
    interactive.resume_picker = resume_session_id.is_none() && !last;
    interactive.resume_last = last;
    interactive.resume_session_id = resume_session_id;
    interactive.resume_show_all = show_all;
    interactive.resume_include_non_interactive = include_non_interactive;

    // Merge resume-scoped flags and overrides with highest precedence.
    merge_interactive_cli_flags(&mut interactive, resume_cli);

    // Propagate any root-level config overrides (e.g. `-c key=value`).
    prepend_config_flags(&mut interactive.config_overrides, root_config_overrides);

    interactive
}

/// Build the final `TuiCli` for a `codex fork` invocation.
fn finalize_fork_interactive(
    mut interactive: TuiCli,
    root_config_overrides: CliConfigOverrides,
    session_id: Option<String>,
    last: bool,
    show_all: bool,
    mut fork_cli: TuiCli,
) -> TuiCli {
    // Start with the parsed interactive CLI so fork shares the same
    // configuration surface area as `codex` without additional flags.
    // Clap assigns the first positional to `session_id`. With `--last`, reinterpret it as the
    // prompt when no second positional prompt was provided.
    let fork_session_id = if last && fork_cli.prompt.is_none() {
        fork_cli.prompt = session_id;
        None
    } else {
        session_id
    };
    interactive.fork_picker = fork_session_id.is_none() && !last;
    interactive.fork_last = last;
    interactive.fork_session_id = fork_session_id;
    interactive.fork_show_all = show_all;

    // Merge fork-scoped flags and overrides with highest precedence.
    merge_interactive_cli_flags(&mut interactive, fork_cli);

    // Propagate any root-level config overrides (e.g. `-c key=value`).
    prepend_config_flags(&mut interactive.config_overrides, root_config_overrides);

    interactive
}

fn finalize_session_archive_interactive(
    mut interactive: TuiCli,
    root_config_overrides: CliConfigOverrides,
    archive_cli: SessionArchiveConfigOverrides,
) -> TuiCli {
    let SessionArchiveConfigOverrides {
        shared,
        strict_config,
        config_overrides,
    } = archive_cli;
    interactive.shared.apply_subcommand_overrides(shared);
    if strict_config {
        interactive.strict_config = true;
    }
    interactive
        .config_overrides
        .raw_overrides
        .extend(config_overrides.raw_overrides);
    prepend_config_flags(&mut interactive.config_overrides, root_config_overrides);
    interactive
}

/// Merge flags provided to runtime wrapper commands so they take precedence over any root-level
/// flags. Only overrides fields explicitly set on the subcommand-scoped CLI. Also appends
/// `-c key=value` overrides with highest precedence.
fn merge_interactive_cli_flags(interactive: &mut TuiCli, subcommand_cli: TuiCli) {
    let TuiCli {
        shared,
        strict_config,
        approval_policy,
        web_search,
        prompt,
        mut config_overrides,
        ..
    } = subcommand_cli;
    let subcommand_auto_review = shared.auto_review;
    interactive
        .shared
        .apply_subcommand_overrides(shared.into_inner());
    interactive
        .shared
        .take_auto_review_config_overrides(&mut config_overrides);
    if subcommand_auto_review {
        interactive.approval_policy = None;
    } else if let Some(approval) = approval_policy {
        interactive.approval_policy = Some(approval);
    }
    if web_search {
        interactive.web_search = true;
    }
    if strict_config {
        interactive.strict_config = true;
    }
    if let Some(prompt) = prompt {
        // Normalize CRLF/CR to LF so CLI-provided text can't leak `\r` into TUI state.
        interactive.prompt = Some(prompt.replace("\r\n", "\n").replace('\r', "\n"));
    }

    interactive
        .config_overrides
        .raw_overrides
        .extend(config_overrides.raw_overrides);
}

fn print_completion(cmd: CompletionCommand) {
    let mut app = MultitoolCli::command();
    let name = "codex";
    generate(cmd.shell, &mut app, name, &mut std::io::stdout());
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;
    use codex_protocol::ThreadId;
    use codex_tui::TokenUsage;
    use pretty_assertions::assert_eq;

    #[test]
    fn cloud_tasks_are_rejected_before_runtime_setup() {
        let err = reject_cloud_tasks_for_whisply()
            .expect_err("Whisply must not start the direct cloud-tasks client");
        assert_eq!(
            err.to_string(),
            "`whisply cloud` is unavailable because cloud tasks require direct ChatGPT authority"
        );
    }

    #[test]
    fn direct_proxy_and_remote_control_commands_are_rejected_without_runtime_setup() {
        let proxy = reject_responses_api_proxy_for_whisply()
            .expect_err("Whisply must not start the direct responses proxy");
        assert_eq!(
            proxy.to_string(),
            "`whisply responses-api-proxy` is unavailable; use the managed Whisply gateway"
        );

        let remote_control = reject_remote_control_for_whisply()
            .expect_err("Whisply must not start standalone remote control");
        assert_eq!(
            remote_control.to_string(),
            "`whisply remote-control` is unavailable; remote control is owned by the managed Whisply app"
        );
    }

    #[test]
    fn standalone_daemon_authority_commands_are_rejected_but_local_stop_and_version_remain() {
        let rejected = [
            AppServerDaemonSubcommand::Bootstrap(AppServerBootstrapCommand {
                remote_control: false,
            }),
            AppServerDaemonSubcommand::Start,
            AppServerDaemonSubcommand::Restart,
            AppServerDaemonSubcommand::EnableRemoteControl,
            AppServerDaemonSubcommand::DisableRemoteControl,
            AppServerDaemonSubcommand::PidUpdateLoop,
        ];
        for subcommand in &rejected {
            let err = reject_legacy_app_server_daemon_subcommand(subcommand)
                .expect_err("standalone daemon authority must be unavailable");
            assert!(err.to_string().contains("standalone daemon management"));
        }

        for subcommand in [
            AppServerDaemonSubcommand::Stop,
            AppServerDaemonSubcommand::Version,
        ] {
            reject_legacy_app_server_daemon_subcommand(&subcommand)
                .expect("safe local daemon action should remain available");
        }
    }

    #[test]
    fn controls_cli_parsing_excludes_overlay_only_names() {
        let cli = MultitoolCli::try_parse_from([
            "whisply",
            "controls",
            "computer-use-route",
            "--route",
            "native-apps",
            "--enabled",
            "true",
        ])
        .expect("ordinary control should parse");
        let Some(Subcommand::Controls(ControlsCommand {
            action: ControlsSubcommand::ComputerUseRoute { route, enabled },
        })) = cli.subcommand
        else {
            panic!("expected ordinary Computer Use route control");
        };
        assert_eq!(route, "native-apps");
        assert!(enabled);
        assert_matches!(
            parse_host_controls_route(&route),
            Ok(HostControlsComputerUseRoute::NativeApps)
        );

        for overlay_only in ["arm", "exam", "invisible", "undetected"] {
            assert!(
                MultitoolCli::try_parse_from(["whisply", "controls", overlay_only]).is_err(),
                "{overlay_only} must not be a controls subcommand"
            );
            assert!(parse_host_controls_route(overlay_only).is_err());
            assert!(parse_host_controls_builtin(overlay_only).is_err());
            assert!(parse_host_controls_interactive_action(overlay_only).is_err());
        }
    }

    #[test]
    fn controls_cli_rejects_unsupported_ordinary_values() {
        assert!(parse_host_controls_route("system-settings").is_err());
        assert!(parse_host_controls_builtin("local-files").is_err());
        assert!(parse_host_controls_permission("unrestricted").is_err());
        assert!(parse_host_controls_audio_mode("invisible").is_err());
    }

    #[test]
    fn exec_server_rejects_legacy_remote_registration_flags() {
        for flag in [
            "--remote",
            "--environment-id",
            "--name",
            "--use-agent-identity-auth",
            "--exit-on-stdin-close",
        ] {
            let mut args = vec!["whisply", "exec-server", flag];
            if matches!(flag, "--remote" | "--environment-id" | "--name") {
                args.push("https://example.invalid");
            }
            assert!(
                MultitoolCli::try_parse_from(args).is_err(),
                "{flag} must not be a Whisply public CLI option"
            );
        }
    }

    fn finalize_resume_from_args(args: &[&str]) -> TuiCli {
        let cli = MultitoolCli::try_parse_from(args).expect("parse");
        let MultitoolCli {
            psp: _,
            mut interactive,
            config_overrides: mut root_overrides,
            subcommand,
            feature_toggles: _,
            remote: _,
        } = cli;
        interactive
            .shared
            .take_auto_review_config_overrides(&mut root_overrides);

        let Subcommand::Resume(ResumeCommand {
            session_id,
            last,
            all,
            include_non_interactive,
            remote: _,
            config_overrides: resume_cli,
        }) = subcommand.expect("resume present")
        else {
            unreachable!()
        };
        let SessionTuiCli(resume_cli) = resume_cli;

        finalize_resume_interactive(
            interactive,
            root_overrides,
            session_id,
            last,
            all,
            include_non_interactive,
            resume_cli,
        )
    }

    fn finalize_fork_from_args(args: &[&str]) -> TuiCli {
        let cli = MultitoolCli::try_parse_from(args).expect("parse");
        let MultitoolCli {
            psp: _,
            mut interactive,
            config_overrides: mut root_overrides,
            subcommand,
            feature_toggles: _,
            remote: _,
        } = cli;
        interactive
            .shared
            .take_auto_review_config_overrides(&mut root_overrides);

        let Subcommand::Fork(ForkCommand {
            session_id,
            last,
            all,
            remote: _,
            config_overrides: fork_cli,
        }) = subcommand.expect("fork present")
        else {
            unreachable!()
        };
        let SessionTuiCli(fork_cli) = fork_cli;

        finalize_fork_interactive(interactive, root_overrides, session_id, last, all, fork_cli)
    }

    fn finalize_exec_from_args(args: &[&str]) -> ExecCli {
        let mut cli = MultitoolCli::try_parse_from(args).expect("parse");
        cli.interactive
            .shared
            .take_auto_review_config_overrides(&mut cli.config_overrides);
        let Some(Subcommand::Exec(mut exec)) = cli.subcommand else {
            panic!("expected exec subcommand");
        };
        exec.shared
            .inherit_exec_root_options(&cli.interactive.shared);
        prepend_config_flags(&mut exec.config_overrides, cli.config_overrides);
        exec.shared
            .take_auto_review_config_overrides(&mut exec.config_overrides);
        exec
    }

    fn finalize_archive_from_args(args: &[&str]) -> (String, TuiCli, InteractiveRemoteOptions) {
        let cli = MultitoolCli::try_parse_from(args).expect("parse");
        let MultitoolCli {
            psp: _,
            interactive,
            config_overrides: root_overrides,
            subcommand,
            feature_toggles: _,
            remote: _,
        } = cli;

        let Subcommand::Archive(SessionArchiveCommand {
            target,
            remote,
            config_overrides: archive_cli,
        }) = subcommand.expect("archive present")
        else {
            unreachable!()
        };

        (
            target,
            finalize_session_archive_interactive(interactive, root_overrides, archive_cli),
            remote,
        )
    }

    fn profile_v2_for_args(args: &[&str]) -> anyhow::Result<Option<String>> {
        let cli = MultitoolCli::try_parse_from(args).expect("parse");
        let Some(subcommand) = cli.subcommand.as_ref() else {
            return Ok(cli
                .interactive
                .config_profile_v2
                .as_ref()
                .map(std::string::ToString::to_string));
        };
        Ok(profile_v2_for_subcommand(&cli.interactive, subcommand)?.map(ToString::to_string))
    }

    #[test]
    fn profile_loader_overrides_use_explicit_codex_home() -> anyhow::Result<()> {
        let codex_home = tempfile::tempdir()?;
        let profile: ProfileV2Name = "work".parse()?;

        let overrides =
            loader_overrides_for_profile_at_codex_home(Some(&profile), codex_home.path());

        assert_eq!(
            overrides.user_config_path,
            Some(resolve_profile_v2_config_path(codex_home.path(), &profile))
        );
        assert_eq!(overrides.user_config_profile, Some(profile));
        Ok(())
    }

    #[test]
    fn profile_v2_is_rejected_for_config_management_subcommands() {
        assert!(profile_v2_for_args(&["codex", "--profile", "work", "features", "list"]).is_err());
    }

    #[test]
    fn profile_v2_is_allowed_for_runtime_subcommands() {
        assert_eq!(
            profile_v2_for_args(&["codex", "--profile", "work", "resume"])
                .expect("resume supports profile-v2")
                .as_deref(),
            Some("work")
        );
        assert_eq!(
            profile_v2_for_args(&["codex", "--profile", "work", "debug", "prompt-input"])
                .expect("debug prompt-input supports profile-v2")
                .as_deref(),
            Some("work")
        );
        assert_eq!(
            profile_v2_for_args(&["codex", "--profile", "work", "mcp", "list"])
                .expect("mcp supports profile-v2")
                .as_deref(),
            Some("work")
        );
        assert_eq!(
            profile_v2_for_args(&["codex", "--profile", "work", "sandbox"])
                .expect("sandbox supports config profile")
                .as_deref(),
            Some("work")
        );
    }

    #[test]
    fn import_remains_an_interactive_prompt() {
        let cli = MultitoolCli::try_parse_from(["codex", "import"]).expect("parse");

        assert!(cli.subcommand.is_none());
        assert_eq!(cli.interactive.prompt.as_deref(), Some("import"));
    }

    #[test]
    fn profile_v2_rejects_non_plain_names_at_parse_time() {
        assert!(
            MultitoolCli::try_parse_from(["codex", "--profile", "nested/work", "resume"]).is_err()
        );
    }

    #[test]
    fn exec_resume_last_accepts_prompt_positional() {
        let cli =
            MultitoolCli::try_parse_from(["codex", "exec", "--json", "resume", "--last", "2+2"])
                .expect("parse should succeed");

        let Some(Subcommand::Exec(exec)) = cli.subcommand else {
            panic!("expected exec subcommand");
        };
        let Some(codex_exec::Command::Resume(args)) = exec.command else {
            panic!("expected exec resume");
        };

        assert!(args.last);
        assert_eq!(args.session_id, None);
        assert_eq!(args.prompt.as_deref(), Some("2+2"));
    }

    #[test]
    fn exec_resume_accepts_output_flags_after_subcommand() {
        let cli = MultitoolCli::try_parse_from([
            "codex",
            "exec",
            "resume",
            "session-123",
            "-o",
            "/tmp/resume-output.md",
            "--output-schema",
            "/tmp/schema.json",
            "re-review",
        ])
        .expect("parse should succeed");

        let Some(Subcommand::Exec(exec)) = cli.subcommand else {
            panic!("expected exec subcommand");
        };
        let Some(codex_exec::Command::Resume(args)) = exec.command else {
            panic!("expected exec resume");
        };

        assert_eq!(
            exec.last_message_file,
            Some(std::path::PathBuf::from("/tmp/resume-output.md"))
        );
        assert_eq!(
            exec.output_schema,
            Some(std::path::PathBuf::from("/tmp/schema.json"))
        );
        assert_eq!(args.session_id.as_deref(), Some("session-123"));
        assert_eq!(args.prompt.as_deref(), Some("re-review"));
    }

    #[test]
    fn dangerous_bypass_conflicts_with_approval_policy() {
        let err = MultitoolCli::try_parse_from([
            "codex",
            "--dangerously-bypass-approvals-and-sandbox",
            "--ask-for-approval",
            "on-request",
        ])
        .expect_err("conflicting permission flags should be rejected");

        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn approve_for_me_configures_interactive_mode() {
        for flag in ["--approve-for-me", "--not-so-yolo"] {
            let mut cli = MultitoolCli::try_parse_from(["codex", flag]).expect("parse flag");

            assert!(cli.interactive.auto_review);
            cli.interactive
                .shared
                .take_auto_review_config_overrides(&mut cli.interactive.config_overrides);
            assert_eq!(
                cli.interactive.config_overrides.raw_overrides,
                vec![
                    r#"approvals_reviewer="auto_review""#.to_string(),
                    r#"approval_policy="on-request""#.to_string(),
                    r#"sandbox_mode="workspace-write""#.to_string(),
                ]
            );
            assert!(!cli.interactive.auto_review);
        }
    }

    #[test]
    fn not_so_yolo_alias_is_hidden_from_help() {
        for args in [&["codex", "--help"][..], &["codex", "exec", "--help"][..]] {
            let help = help_from_args(args);

            assert!(!help.contains("--not-so-yolo"), "{help}");
        }
    }

    #[test]
    fn approve_for_me_defaults_propagate_from_root_to_exec() {
        let exec = finalize_exec_from_args(&["codex", "--approve-for-me", "exec", "summarize"]);

        assert_eq!(
            exec.config_overrides.raw_overrides,
            vec![
                r#"approvals_reviewer="auto_review""#.to_string(),
                r#"approval_policy="on-request""#.to_string(),
                r#"sandbox_mode="workspace-write""#.to_string(),
            ]
        );
        assert!(exec.sandbox_mode.is_none());
    }

    #[test]
    fn later_exec_sandbox_partially_overrides_approve_for_me() {
        let exec = finalize_exec_from_args(&[
            "codex",
            "--approve-for-me",
            "exec",
            "--sandbox",
            "read-only",
        ]);

        assert_matches!(
            exec.sandbox_mode,
            Some(codex_utils_cli::SandboxModeCliArg::ReadOnly)
        );
        assert_eq!(
            exec.config_overrides.raw_overrides,
            vec![
                r#"approvals_reviewer="auto_review""#.to_string(),
                r#"approval_policy="on-request""#.to_string(),
                r#"sandbox_mode="workspace-write""#.to_string(),
            ]
        );
    }

    #[test]
    fn later_approve_for_me_overrides_root_exec_sandbox() {
        let exec = finalize_exec_from_args(&[
            "codex",
            "--sandbox",
            "read-only",
            "exec",
            "--approve-for-me",
        ]);

        assert!(exec.sandbox_mode.is_none());
        assert_eq!(
            exec.config_overrides.raw_overrides,
            vec![
                r#"approvals_reviewer="auto_review""#.to_string(),
                r#"approval_policy="on-request""#.to_string(),
                r#"sandbox_mode="workspace-write""#.to_string(),
            ]
        );
    }

    #[test]
    fn later_resume_approval_policy_partially_overrides_approve_for_me() {
        let interactive = finalize_resume_from_args(&[
            "codex",
            "--approve-for-me",
            "resume",
            "--ask-for-approval",
            "never",
        ]);

        assert_matches!(
            interactive.approval_policy,
            Some(codex_utils_cli::ApprovalModeCliArg::Never)
        );
        assert_eq!(
            interactive.config_overrides.raw_overrides,
            vec![
                r#"approvals_reviewer="auto_review""#.to_string(),
                r#"approval_policy="on-request""#.to_string(),
                r#"sandbox_mode="workspace-write""#.to_string(),
            ]
        );
    }

    #[test]
    fn later_approve_for_me_overrides_root_tui_approval_policy() {
        let interactive = finalize_resume_from_args(&[
            "codex",
            "--ask-for-approval",
            "never",
            "resume",
            "--approve-for-me",
        ]);

        assert!(interactive.approval_policy.is_none());
        assert_eq!(
            interactive.config_overrides.raw_overrides,
            vec![
                r#"approvals_reviewer="auto_review""#.to_string(),
                r#"approval_policy="on-request""#.to_string(),
                r#"sandbox_mode="workspace-write""#.to_string(),
            ]
        );
    }

    #[test]
    fn approve_for_me_conflicts_with_explicit_interactive_permissions() {
        for conflicting_args in [
            vec!["--sandbox", "read-only"],
            vec!["--ask-for-approval", "on-request"],
            vec!["--dangerously-bypass-approvals-and-sandbox"],
        ] {
            let mut args = vec!["codex", "--approve-for-me"];
            args.extend(conflicting_args);

            let error =
                MultitoolCli::try_parse_from(args).expect_err("permission flags should conflict");
            assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
        }
    }

    fn app_server_from_args(args: &[&str]) -> AppServerCommand {
        let cli = MultitoolCli::try_parse_from(args).expect("parse");
        let Subcommand::AppServer(app_server) = cli.subcommand.expect("app-server present") else {
            unreachable!()
        };
        app_server
    }

    fn default_app_server_socket_path() -> AbsolutePathBuf {
        let codex_home = find_codex_home().expect("codex home");
        codex_app_server::app_server_control_socket_path(&codex_home)
            .expect("default app-server socket path")
    }

    #[test]
    fn debug_prompt_input_parses_prompt_and_images() {
        let cli = MultitoolCli::try_parse_from([
            "codex",
            "debug",
            "prompt-input",
            "hello",
            "--image",
            "/tmp/a.png,/tmp/b.png",
        ])
        .expect("parse");

        let Some(Subcommand::Debug(DebugCommand {
            subcommand: DebugSubcommand::PromptInput(cmd),
        })) = cli.subcommand
        else {
            panic!("expected debug prompt-input subcommand");
        };

        assert_eq!(cmd.prompt.as_deref(), Some("hello"));
        assert_eq!(
            cmd.images,
            vec![PathBuf::from("/tmp/a.png"), PathBuf::from("/tmp/b.png")]
        );
    }

    #[test]
    fn public_models_commands_reject_bundled_fallback_and_parse_managed_routes() {
        assert!(MultitoolCli::try_parse_from(["codex", "debug", "models", "--bundled"]).is_err());
        assert!(matches!(
            MultitoolCli::try_parse_from(["codex", "models", "--json"])
                .expect("managed models parse")
                .subcommand,
            Some(Subcommand::Models(ModelsCommand { json: true }))
        ));
        assert!(matches!(
            MultitoolCli::try_parse_from(["codex", "usage", "--json"])
                .expect("usage parse")
                .subcommand,
            Some(Subcommand::Usage(UsageCommand { json: true }))
        ));
        assert!(matches!(
            MultitoolCli::try_parse_from(["codex", "connectors", "status", "google-main"])
                .expect("connectors parse")
                .subcommand,
            Some(Subcommand::Connectors(ConnectorsCommand {
                action: Some(ConnectorsSubcommand::Status { .. })
            }))
        ));
        assert!(matches!(
            MultitoolCli::try_parse_from(["codex", "tools", "status", "whisply.files"])
                .expect("tools parse")
                .subcommand,
            Some(Subcommand::Tools(ToolsCommand {
                action: Some(ToolsSubcommand::Status { .. })
            }))
        ));
    }

    #[test]
    fn public_session_skill_config_and_diagnostic_commands_parse() {
        assert!(matches!(
            MultitoolCli::try_parse_from(["whisply", "sessions", "list", "--all", "--limit", "20"])
                .expect("sessions list parses")
                .subcommand,
            Some(Subcommand::Sessions(_))
        ));
        assert!(matches!(
            MultitoolCli::try_parse_from(["whisply", "sessions", "export", "session-id"])
                .expect("sessions export parses")
                .subcommand,
            Some(Subcommand::Sessions(_))
        ));
        assert!(matches!(
            MultitoolCli::try_parse_from(["whisply", "skills", "validate"])
                .expect("skills validate parses")
                .subcommand,
            Some(Subcommand::Skills(_))
        ));
        assert!(matches!(
            MultitoolCli::try_parse_from(["whisply", "config", "model", "gpt-5.6-luna"])
                .expect("config model parses")
                .subcommand,
            Some(Subcommand::Config(_))
        ));
        assert!(matches!(
            MultitoolCli::try_parse_from(["whisply", "profile", "create", "work"])
                .expect("profile create parses")
                .subcommand,
            Some(Subcommand::Profile(_))
        ));
        assert!(matches!(
            MultitoolCli::try_parse_from([
                "whisply",
                "diagnostics",
                "force-ui",
                "chat.overlay",
                "--state",
                "empty",
            ])
            .expect("fixture-only force-ui parses")
            .subcommand,
            Some(Subcommand::Diagnostics(_))
        ));
        assert!(
            MultitoolCli::try_parse_from([
                "whisply",
                "diagnostics",
                "force-ui",
                "chat.overlay",
                "--state",
                "empty",
                "--force",
            ])
            .is_err()
        );
        assert!(matches!(
            MultitoolCli::try_parse_from([
                "whisply",
                "test",
                "release",
                "--group",
                "policy",
                "--dry-run",
            ])
            .expect("fixed release test parses")
            .subcommand,
            Some(Subcommand::Test(_))
        ));
        assert!(
            MultitoolCli::try_parse_from(["whisply", "test", "release", "--command", "true",])
                .is_err()
        );
    }

    #[test]
    fn public_session_rows_cannot_emit_terminal_control_sequences() {
        assert_eq!(
            terminal_cell("title\u{1b}[2J\nnext\tcell"),
            "title [2J next cell"
        );
    }

    #[test]
    fn responses_subcommand_is_not_registered() {
        let command = MultitoolCli::command();
        assert!(
            command
                .get_subcommands()
                .all(|subcommand| subcommand.get_name() != "responses")
        );
    }

    fn help_from_args(args: &[&str]) -> String {
        let err = MultitoolCli::try_parse_from(args).expect_err("help should short-circuit");
        assert_eq!(err.kind(), clap::error::ErrorKind::DisplayHelp);
        err.to_string()
    }

    #[test]
    fn plugin_marketplace_help_uses_plugin_namespace() {
        let help = help_from_args(&["whisply", "plugin", "marketplace", "--help"]);
        assert!(
            help.contains("Usage: whisply plugin marketplace [OPTIONS] <COMMAND>"),
            "{help}"
        );

        for (subcommand, usage) in [
            ("add", "Usage: whisply plugin marketplace add"),
            ("list", "Usage: whisply plugin marketplace list"),
            ("upgrade", "Usage: whisply plugin marketplace upgrade"),
            ("remove", "Usage: whisply plugin marketplace remove"),
        ] {
            let help = help_from_args(&["whisply", "plugin", "marketplace", subcommand, "--help"]);
            assert!(help.contains(usage), "{help}");
        }
    }

    #[test]
    fn plugin_marketplace_add_parses_under_plugin() {
        let cli =
            MultitoolCli::try_parse_from(["codex", "plugin", "marketplace", "add", "owner/repo"])
                .expect("parse");

        assert!(matches!(cli.subcommand, Some(Subcommand::Plugin(_))));
    }

    #[test]
    fn plugin_marketplace_upgrade_parses_under_plugin() {
        let cli =
            MultitoolCli::try_parse_from(["codex", "plugin", "marketplace", "upgrade", "debug"])
                .expect("parse");

        assert!(matches!(cli.subcommand, Some(Subcommand::Plugin(_))));
    }

    #[test]
    fn plugin_add_parses_under_plugin() {
        let cli = MultitoolCli::try_parse_from([
            "codex",
            "plugin",
            "add",
            "sample",
            "--marketplace",
            "debug",
        ])
        .expect("parse");

        assert!(matches!(cli.subcommand, Some(Subcommand::Plugin(_))));
    }

    #[test]
    fn plugin_list_parses_under_plugin() {
        let cli =
            MultitoolCli::try_parse_from(["codex", "plugin", "list", "--marketplace", "debug"])
                .expect("parse");

        assert!(matches!(cli.subcommand, Some(Subcommand::Plugin(_))));
    }

    #[test]
    fn plugin_remove_parses_under_plugin() {
        let cli = MultitoolCli::try_parse_from([
            "codex",
            "plugin",
            "remove",
            "sample",
            "--marketplace",
            "debug",
        ])
        .expect("parse");

        assert!(matches!(cli.subcommand, Some(Subcommand::Plugin(_))));
    }

    #[test]
    fn update_parses_as_update_subcommand() {
        let cli = MultitoolCli::try_parse_from(["codex", "update"]).expect("parse");
        assert!(matches!(cli.subcommand, Some(Subcommand::Update)));
    }

    #[test]
    fn archive_merges_scoped_tui_flags() {
        let (target, interactive, remote) = finalize_archive_from_args(
            [
                "codex",
                "-C",
                "/root",
                "archive",
                "--remote",
                "unix://archive.sock",
                "--strict-config",
                "--dangerously-bypass-hook-trust",
                "-m",
                "gpt-5.1-test",
                "-p",
                "work",
                "-C",
                "/archive",
                "my-thread",
            ]
            .as_ref(),
        );

        assert_eq!(target, "my-thread");
        assert_eq!(remote.remote.as_deref(), Some("unix://archive.sock"));
        assert_eq!(interactive.model.as_deref(), Some("gpt-5.1-test"));
        assert_eq!(interactive.config_profile_v2.as_deref(), Some("work"));
        assert_eq!(
            interactive.cwd.as_deref(),
            Some(std::path::Path::new("/archive"))
        );
        assert!(interactive.strict_config);
        assert!(interactive.bypass_hook_trust);
    }

    #[test]
    fn delete_force_requires_uuid() {
        assert!(delete_action("123e4567-e89b-12d3-a456-426614174000", /*force*/ true).is_ok());

        let err =
            delete_action("my-thread", /*force*/ true).expect_err("name should require prompt");
        assert_eq!(
            err.to_string(),
            "--force requires a session UUID; names must be confirmed interactively"
        );
    }

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    #[test]
    fn sandbox_parses_permission_profile() {
        let cli = MultitoolCli::try_parse_from([
            "codex",
            "sandbox",
            "--permission-profile",
            ":workspace",
            "--",
            "echo",
        ])
        .expect("parse");

        let Some(Subcommand::Sandbox(command)) = cli.subcommand else {
            panic!("expected sandbox command");
        };

        assert_eq!(command.permissions_profile.as_deref(), Some(":workspace"));
        assert_eq!(command.command, vec!["echo"]);
    }

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    #[test]
    fn sandbox_parses_legacy_permissions_profile_alias() {
        let cli = MultitoolCli::try_parse_from([
            "codex",
            "sandbox",
            "--permissions-profile",
            ":workspace",
            "--",
            "echo",
        ])
        .expect("parse");

        let Some(Subcommand::Sandbox(command)) = cli.subcommand else {
            panic!("expected sandbox command");
        };

        assert_eq!(command.permissions_profile.as_deref(), Some(":workspace"));
        assert_eq!(command.command, vec!["echo"]);
    }

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    #[test]
    fn sandbox_help_only_shows_singular_permission_profile() {
        let help = help_from_args(&["codex", "sandbox", "--help"]);
        assert!(help.contains("--permission-profile"), "{help}");
        assert!(!help.contains("--permissions-profile"), "{help}");
    }

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    #[test]
    fn sandbox_parses_permissions_profile_short_alias() {
        let cli =
            MultitoolCli::try_parse_from(["codex", "sandbox", "-P", ":workspace", "--", "echo"])
                .expect("parse");

        let Some(Subcommand::Sandbox(command)) = cli.subcommand else {
            panic!("expected sandbox command");
        };

        assert_eq!(command.permissions_profile.as_deref(), Some(":workspace"));
        assert_eq!(command.command, vec!["echo"]);
    }

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    #[test]
    fn sandbox_parses_config_profile() {
        let cli =
            MultitoolCli::try_parse_from(["codex", "sandbox", "--profile", "work", "--", "echo"])
                .expect("parse");

        let Some(Subcommand::Sandbox(command)) = cli.subcommand else {
            panic!("expected sandbox command");
        };

        assert_eq!(command.config_profile.as_deref(), Some("work"));
        assert_eq!(command.command, vec!["echo"]);
    }

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    #[test]
    fn sandbox_rejects_explicit_profile_controls_without_profile() {
        let err = MultitoolCli::try_parse_from(["codex", "sandbox", "-C", "/tmp"])
            .expect_err("parse should fail");

        assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn plugin_marketplace_remove_parses_under_plugin() {
        let cli =
            MultitoolCli::try_parse_from(["codex", "plugin", "marketplace", "remove", "debug"])
                .expect("parse");

        assert!(matches!(cli.subcommand, Some(Subcommand::Plugin(_))));
    }

    #[test]
    fn marketplace_no_longer_parses_at_top_level() {
        let add_result =
            MultitoolCli::try_parse_from(["codex", "marketplace", "add", "owner/repo"]);
        assert!(add_result.is_err());

        let upgrade_result =
            MultitoolCli::try_parse_from(["codex", "marketplace", "upgrade", "debug"]);
        assert!(upgrade_result.is_err());

        let remove_result =
            MultitoolCli::try_parse_from(["codex", "marketplace", "remove", "debug"]);
        assert!(remove_result.is_err());
    }

    fn sample_exit_info(conversation_id: Option<&str>, thread_name: Option<&str>) -> AppExitInfo {
        let token_usage = TokenUsage {
            output_tokens: 2,
            total_tokens: 2,
            ..Default::default()
        };
        let thread_id = conversation_id
            .map(ThreadId::from_string)
            .map(Result::unwrap);
        AppExitInfo {
            token_usage,
            thread_id,
            resume_hint: codex_utils_cli::resume_hint(thread_name, thread_id),
            update_action: None,
            exit_reason: ExitReason::UserRequested,
        }
    }

    #[test]
    fn format_exit_messages_skips_zero_usage() {
        let exit_info = AppExitInfo {
            token_usage: TokenUsage::default(),
            thread_id: None,
            resume_hint: None,
            update_action: None,
            exit_reason: ExitReason::UserRequested,
        };
        let lines = format_exit_messages(exit_info, /*color_enabled*/ false);
        assert!(lines.is_empty());
    }

    #[test]
    fn format_exit_messages_includes_session_id_for_fatal_exit_without_resume_hint() {
        let exit_info = AppExitInfo {
            token_usage: TokenUsage::default(),
            thread_id: Some(ThreadId::from_string("123e4567-e89b-12d3-a456-426614174000").unwrap()),
            resume_hint: None,
            update_action: None,
            exit_reason: ExitReason::Fatal("boom".to_string()),
        };
        let lines = format_exit_messages(exit_info, /*color_enabled*/ false);
        assert_eq!(
            lines,
            vec!["Session ID: 123e4567-e89b-12d3-a456-426614174000".to_string()]
        );
    }

    #[test]
    fn format_exit_messages_includes_resume_hint_for_fatal_exit() {
        let mut exit_info = sample_exit_info(
            Some("123e4567-e89b-12d3-a456-426614174000"),
            /*thread_name*/ None,
        );
        exit_info.exit_reason = ExitReason::Fatal("boom".to_string());
        let lines = format_exit_messages(exit_info, /*color_enabled*/ false);
        assert_eq!(
            lines,
            vec![
                "Token usage: total=2 input=0 output=2".to_string(),
                "To continue this session, run codex resume 123e4567-e89b-12d3-a456-426614174000"
                    .to_string(),
            ]
        );
    }

    #[test]
    fn format_exit_messages_includes_resume_hint_without_color() {
        let exit_info = sample_exit_info(
            Some("123e4567-e89b-12d3-a456-426614174000"),
            /*thread_name*/ None,
        );
        let lines = format_exit_messages(exit_info, /*color_enabled*/ false);
        assert_eq!(
            lines,
            vec![
                "Token usage: total=2 input=0 output=2".to_string(),
                "To continue this session, run codex resume 123e4567-e89b-12d3-a456-426614174000"
                    .to_string(),
            ]
        );
    }

    #[test]
    fn format_exit_messages_applies_color_when_enabled() {
        let exit_info = sample_exit_info(
            Some("123e4567-e89b-12d3-a456-426614174000"),
            /*thread_name*/ None,
        );
        let lines = format_exit_messages(exit_info, /*color_enabled*/ true);
        assert_eq!(lines.len(), 2);
        assert!(lines[1].contains("\u{1b}[36m"));
    }

    #[test]
    fn format_exit_messages_names_picker_item_when_thread_has_name() {
        let exit_info = sample_exit_info(
            Some("123e4567-e89b-12d3-a456-426614174000"),
            Some("my-thread"),
        );
        let lines = format_exit_messages(exit_info, /*color_enabled*/ false);
        assert_eq!(
            lines,
            vec![
                "Token usage: total=2 input=0 output=2".to_string(),
                "To continue this session, run codex resume, then select my-thread (123e4567-e89b-12d3-a456-426614174000)".to_string(),
            ]
        );
    }

    #[test]
    fn resume_model_flag_applies_when_no_root_flags() {
        let interactive =
            finalize_resume_from_args(["codex", "resume", "-m", "gpt-5.1-test"].as_ref());

        assert_eq!(interactive.model.as_deref(), Some("gpt-5.1-test"));
        assert!(interactive.resume_picker);
        assert!(!interactive.resume_last);
        assert_eq!(interactive.resume_session_id, None);
    }

    #[test]
    fn resume_picker_logic_none_and_not_last() {
        let interactive = finalize_resume_from_args(["codex", "resume"].as_ref());
        assert!(interactive.resume_picker);
        assert!(!interactive.resume_last);
        assert_eq!(interactive.resume_session_id, None);
        assert!(!interactive.resume_show_all);
    }

    #[test]
    fn resume_picker_logic_last() {
        let interactive = finalize_resume_from_args(["codex", "resume", "--last"].as_ref());
        assert!(!interactive.resume_picker);
        assert!(interactive.resume_last);
        assert_eq!(interactive.resume_session_id, None);
        assert!(!interactive.resume_show_all);
    }

    #[test]
    fn resume_last_accepts_prompt_positional() {
        let interactive = finalize_resume_from_args(
            ["codex", "resume", "--last", "/compact focus on auth"].as_ref(),
        );

        assert!(!interactive.resume_picker);
        assert!(interactive.resume_last);
        assert_eq!(interactive.resume_session_id, None);
        assert_eq!(
            interactive.prompt.as_deref(),
            Some("/compact focus on auth")
        );
    }

    #[test]
    fn resume_last_rejects_explicit_session_and_prompt() {
        let err =
            MultitoolCli::try_parse_from(["codex", "resume", "--last", "1234", "continue here"])
                .expect_err("--last with an explicit session and prompt should be rejected");

        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn resume_picker_logic_with_session_id() {
        let interactive = finalize_resume_from_args(["codex", "resume", "1234"].as_ref());
        assert!(!interactive.resume_picker);
        assert!(!interactive.resume_last);
        assert_eq!(interactive.resume_session_id.as_deref(), Some("1234"));
        assert!(!interactive.resume_show_all);
    }

    #[test]
    fn resume_with_session_id_accepts_prompt_positional() {
        let interactive =
            finalize_resume_from_args(["codex", "resume", "1234", "continue here"].as_ref());

        assert!(!interactive.resume_picker);
        assert!(!interactive.resume_last);
        assert_eq!(interactive.resume_session_id.as_deref(), Some("1234"));
        assert_eq!(interactive.prompt.as_deref(), Some("continue here"));
    }

    #[test]
    fn resume_all_flag_sets_show_all() {
        let interactive = finalize_resume_from_args(["codex", "resume", "--all"].as_ref());
        assert!(interactive.resume_picker);
        assert!(interactive.resume_show_all);
    }

    #[test]
    fn resume_include_non_interactive_flag_sets_source_filter_override() {
        let interactive =
            finalize_resume_from_args(["codex", "resume", "--include-non-interactive"].as_ref());

        assert!(interactive.resume_picker);
        assert!(interactive.resume_include_non_interactive);
    }

    #[test]
    fn resume_merges_option_flags() {
        let interactive = finalize_resume_from_args(
            [
                "codex",
                "resume",
                "sid",
                "--oss",
                "--search",
                "--sandbox",
                "workspace-write",
                "--ask-for-approval",
                "on-request",
                "-m",
                "gpt-5.1-test",
                "-p",
                "my-config",
                "-C",
                "/tmp",
                "--strict-config",
                "-i",
                "/tmp/a.png,/tmp/b.png",
            ]
            .as_ref(),
        );

        assert_eq!(interactive.model.as_deref(), Some("gpt-5.1-test"));
        assert!(interactive.oss);
        assert_eq!(interactive.config_profile_v2.as_deref(), Some("my-config"));
        assert_matches!(
            interactive.sandbox_mode,
            Some(codex_utils_cli::SandboxModeCliArg::WorkspaceWrite)
        );
        assert_matches!(
            interactive.approval_policy,
            Some(codex_utils_cli::ApprovalModeCliArg::OnRequest)
        );
        assert_eq!(
            interactive.cwd.as_deref(),
            Some(std::path::Path::new("/tmp"))
        );
        assert!(interactive.web_search);
        assert!(interactive.strict_config);
        let has_a = interactive
            .images
            .iter()
            .any(|p| p == std::path::Path::new("/tmp/a.png"));
        let has_b = interactive
            .images
            .iter()
            .any(|p| p == std::path::Path::new("/tmp/b.png"));
        assert!(has_a && has_b);
        assert!(!interactive.resume_picker);
        assert!(!interactive.resume_last);
        assert_eq!(interactive.resume_session_id.as_deref(), Some("sid"));
    }

    #[test]
    fn resume_merges_dangerously_bypass_flag() {
        let interactive = finalize_resume_from_args(
            [
                "codex",
                "resume",
                "--dangerously-bypass-approvals-and-sandbox",
            ]
            .as_ref(),
        );
        assert!(interactive.dangerously_bypass_approvals_and_sandbox);
        assert!(interactive.resume_picker);
        assert!(!interactive.resume_last);
        assert_eq!(interactive.resume_session_id, None);
    }

    #[test]
    fn resume_merges_bypass_hook_trust_flag() {
        let interactive = finalize_resume_from_args(
            ["codex", "resume", "--dangerously-bypass-hook-trust"].as_ref(),
        );

        assert!(interactive.bypass_hook_trust);
        assert!(interactive.resume_picker);
        assert!(!interactive.resume_last);
        assert_eq!(interactive.resume_session_id, None);
    }

    #[test]
    fn fork_picker_logic_none_and_not_last() {
        let interactive = finalize_fork_from_args(["codex", "fork"].as_ref());
        assert!(interactive.fork_picker);
        assert!(!interactive.fork_last);
        assert_eq!(interactive.fork_session_id, None);
        assert!(!interactive.fork_show_all);
    }

    #[test]
    fn fork_picker_logic_last() {
        let interactive = finalize_fork_from_args(["codex", "fork", "--last"].as_ref());
        assert!(!interactive.fork_picker);
        assert!(interactive.fork_last);
        assert_eq!(interactive.fork_session_id, None);
        assert!(!interactive.fork_show_all);
    }

    #[test]
    fn fork_last_accepts_prompt_positional() {
        let interactive =
            finalize_fork_from_args(["codex", "fork", "--last", "/compact focus on auth"].as_ref());

        assert!(!interactive.fork_picker);
        assert!(interactive.fork_last);
        assert_eq!(interactive.fork_session_id, None);
        assert_eq!(
            interactive.prompt.as_deref(),
            Some("/compact focus on auth")
        );
    }

    #[test]
    fn fork_last_rejects_explicit_session_and_prompt() {
        let err =
            MultitoolCli::try_parse_from(["codex", "fork", "--last", "1234", "continue here"])
                .expect_err("--last with an explicit session and prompt should be rejected");

        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn fork_picker_logic_with_session_id() {
        let interactive = finalize_fork_from_args(["codex", "fork", "1234"].as_ref());
        assert!(!interactive.fork_picker);
        assert!(!interactive.fork_last);
        assert_eq!(interactive.fork_session_id.as_deref(), Some("1234"));
        assert!(!interactive.fork_show_all);
    }

    #[test]
    fn fork_with_session_id_accepts_prompt_positional() {
        let interactive =
            finalize_fork_from_args(["codex", "fork", "1234", "continue here"].as_ref());

        assert!(!interactive.fork_picker);
        assert!(!interactive.fork_last);
        assert_eq!(interactive.fork_session_id.as_deref(), Some("1234"));
        assert_eq!(interactive.prompt.as_deref(), Some("continue here"));
    }

    #[test]
    fn fork_all_flag_sets_show_all() {
        let interactive = finalize_fork_from_args(["codex", "fork", "--all"].as_ref());
        assert!(interactive.fork_picker);
        assert!(interactive.fork_show_all);
    }

    #[test]
    fn app_server_analytics_default_disabled_without_flag() {
        let app_server = app_server_from_args(["codex", "app-server"].as_ref());
        assert!(!app_server.analytics_default_enabled);
        assert!(!app_server.remote_control);
        assert_eq!(
            app_server.listen,
            codex_app_server::AppServerTransport::Stdio
        );
    }

    #[test]
    fn app_server_remote_control_startup_flag_enables_remote_control() {
        let enabled = app_server_from_args(["codex", "app-server", "--remote-control"].as_ref());
        assert!(enabled.remote_control);
    }

    #[test]
    fn app_server_analytics_default_enabled_with_flag() {
        let app_server =
            app_server_from_args(["codex", "app-server", "--analytics-default-enabled"].as_ref());
        assert!(app_server.analytics_default_enabled);
    }

    #[test]
    fn strict_config_parses_for_supported_commands() {
        let cli = MultitoolCli::try_parse_from(["codex", "--strict-config"]).expect("parse");
        assert!(cli.interactive.strict_config);

        let cli = MultitoolCli::try_parse_from(["codex", "mcp-server", "--strict-config"])
            .expect("parse");
        assert_matches!(
            cli.subcommand,
            Some(Subcommand::McpServer(McpServerCommand {
                strict_config: true,
            }))
        );

        let cli =
            MultitoolCli::try_parse_from(["codex", "review", "--strict-config", "--uncommitted"])
                .expect("parse");
        assert_matches!(
            cli.subcommand,
            Some(Subcommand::Review(ReviewCommand {
                strict_config: true,
                ..
            }))
        );

        let cli = MultitoolCli::try_parse_from(["codex", "exec-server", "--strict-config"])
            .expect("parse");
        assert_matches!(
            cli.subcommand,
            Some(Subcommand::ExecServer(ExecServerCommand {
                strict_config: true,
                ..
            }))
        );
    }

    #[test]
    fn root_strict_config_is_supported_for_exec_server() {
        let cli = MultitoolCli::try_parse_from(["codex", "--strict-config", "exec-server"])
            .expect("parse");

        reject_root_strict_config_for_subcommand(cli.interactive.strict_config, &cli.subcommand)
            .expect("exec-server should support root --strict-config");
    }

    #[test]
    fn root_strict_config_is_rejected_for_unsupported_subcommands() {
        let cli = MultitoolCli::try_parse_from(["codex", "--strict-config", "mcp", "list"])
            .expect("parse");
        let err = reject_root_strict_config_for_subcommand(
            cli.interactive.strict_config,
            &cli.subcommand,
        )
        .expect_err("mcp should not support root --strict-config");

        assert_eq!(
            err.to_string(),
            "`--strict-config` is not supported for `codex mcp`"
        );

        let cli = MultitoolCli::try_parse_from(["codex", "--strict-config", "remote-control"])
            .expect("parse");
        let err = reject_root_strict_config_for_subcommand(
            cli.interactive.strict_config,
            &cli.subcommand,
        )
        .expect_err("remote-control should not support root --strict-config");

        assert_eq!(
            err.to_string(),
            "`--strict-config` is not supported for `codex remote-control`"
        );
    }

    #[test]
    fn app_server_subcommands_reject_strict_config() {
        let app_server =
            app_server_from_args(["codex", "app-server", "--strict-config", "proxy"].as_ref());
        let err = reject_strict_config_for_app_server_subcommand(
            app_server.strict_config,
            app_server.subcommand.as_ref(),
        )
        .expect_err("app-server proxy should not support --strict-config");

        assert_eq!(
            err.to_string(),
            "`--strict-config` is not supported for `codex app-server proxy`"
        );
    }

    #[test]
    fn reject_remote_flag_for_remote_control() {
        let cli = MultitoolCli::try_parse_from(["codex", "--remote", "unix://", "remote-control"])
            .expect("parse");
        let Some(Subcommand::RemoteControl(_)) = &cli.subcommand else {
            panic!("expected remote-control subcommand");
        };

        let err = reject_remote_mode_for_subcommand(
            cli.remote.remote.as_deref(),
            cli.remote.remote_auth_token_env.as_deref(),
            "remote-control",
        )
        .expect_err("remote-control should reject root --remote");

        assert!(err.to_string().contains("remote-control"));
    }

    #[test]
    fn remote_control_pair_is_not_a_callable_legacy_subcommand() {
        assert!(MultitoolCli::try_parse_from(["codex", "remote-control", "pair"]).is_err());
    }

    #[test]
    fn remote_flag_parses_for_interactive_root() {
        let cli = MultitoolCli::try_parse_from(["codex", "--remote", "unix://codex.sock"])
            .expect("parse");
        assert_eq!(cli.remote.remote.as_deref(), Some("unix://codex.sock"));
    }

    #[test]
    fn remote_help_advertises_literal_loopback_websocket_endpoints() {
        let mut command = MultitoolCli::command();
        let mut help = Vec::new();
        command
            .write_long_help(&mut help)
            .expect("render long help");
        let help = String::from_utf8(help).expect("help should be UTF-8");

        assert!(help.contains("literal `ws://LOOPBACK_IP:PORT`"));
        assert!(help.contains("literal `wss://LOOPBACK_IP:PORT`"));
    }

    #[test]
    fn remote_auth_token_env_flag_parses_for_interactive_root() {
        let cli = MultitoolCli::try_parse_from([
            "codex",
            "--remote-auth-token-env",
            "CODEX_REMOTE_AUTH_TOKEN",
            "--remote",
            "ws://127.0.0.1:4500",
        ])
        .expect("parse");
        assert_eq!(
            cli.remote.remote_auth_token_env.as_deref(),
            Some("CODEX_REMOTE_AUTH_TOKEN")
        );
    }

    #[test]
    fn remote_flag_parses_for_resume_subcommand() {
        let cli =
            MultitoolCli::try_parse_from(["codex", "resume", "--remote", "unix://codex.sock"])
                .expect("parse");
        let Subcommand::Resume(ResumeCommand { remote, .. }) =
            cli.subcommand.expect("resume present")
        else {
            panic!("expected resume subcommand");
        };
        assert_eq!(remote.remote.as_deref(), Some("unix://codex.sock"));
    }

    #[test]
    fn reject_remote_mode_for_non_interactive_subcommands() {
        let err = reject_remote_mode_for_subcommand(
            Some("127.0.0.1:4500"),
            /*remote_auth_token_env*/ None,
            "exec",
        )
        .expect_err("non-interactive subcommands should reject --remote");
        assert!(
            err.to_string()
                .contains("only supported for interactive TUI commands")
        );
    }

    #[test]
    fn reject_remote_auth_token_env_for_non_interactive_subcommands() {
        let err = reject_remote_mode_for_subcommand(
            /*remote*/ None,
            Some("CODEX_REMOTE_AUTH_TOKEN"),
            "exec",
        )
        .expect_err("non-interactive subcommands should reject --remote-auth-token-env");
        assert!(
            err.to_string()
                .contains("only supported for interactive TUI commands")
        );
    }

    #[test]
    fn reject_remote_auth_token_env_for_app_server_generate_internal_json_schema() {
        let subcommand =
            AppServerSubcommand::GenerateInternalJsonSchema(GenerateInternalJsonSchemaCommand {
                out_dir: PathBuf::from("/tmp/out"),
            });
        let err = reject_remote_mode_for_app_server_subcommand(
            /*remote*/ None,
            Some("CODEX_REMOTE_AUTH_TOKEN"),
            Some(&subcommand),
        )
        .expect_err("non-interactive app-server subcommands should reject --remote-auth-token-env");
        assert!(err.to_string().contains("generate-internal-json-schema"));
    }

    #[test]
    fn read_remote_auth_token_from_env_var_reports_missing_values() {
        let err = read_remote_auth_token_from_env_var_with("CODEX_REMOTE_AUTH_TOKEN", |_| {
            Err(std::env::VarError::NotPresent)
        })
        .expect_err("missing env vars should be rejected");
        assert!(err.to_string().contains("is not set"));
    }

    #[test]
    fn read_remote_auth_token_from_env_var_trims_values() {
        let auth_token =
            read_remote_auth_token_from_env_var_with("CODEX_REMOTE_AUTH_TOKEN", |_| {
                Ok("  bearer-token  ".to_string())
            })
            .expect("env var should parse");
        assert_eq!(auth_token, "bearer-token");
    }

    #[test]
    fn read_remote_auth_token_from_env_var_rejects_empty_values() {
        let err = read_remote_auth_token_from_env_var_with("CODEX_REMOTE_AUTH_TOKEN", |_| {
            Ok(" \n\t ".to_string())
        })
        .expect_err("empty env vars should be rejected");
        assert!(err.to_string().contains("is empty"));
    }

    #[test]
    fn remote_auth_token_env_rejects_non_loopback_or_userinfo_remote_before_environment_read() {
        for remote in [
            "wss://executor.example:443",
            "ws://username@127.0.0.1:4500",
            "wss://username:password@[::1]:4500",
        ] {
            let err = resolve_remote_endpoint_with(
                Some(remote.to_string()),
                Some("CODEX_REMOTE_AUTH_TOKEN".to_string()),
                |_| panic!("invalid remote must be rejected before reading its auth token"),
            )
            .expect_err("invalid --remote should be rejected");

            assert!(err.to_string().contains("invalid remote address"));
        }
    }

    #[test]
    fn psp_is_a_global_runtime_argument() {
        for args in [
            ["codex", "--psp"].as_slice(),
            ["codex", "app-server", "--psp"].as_slice(),
            ["codex", "remote-control", "--psp"].as_slice(),
        ] {
            let cli = MultitoolCli::try_parse_from(args).expect("parse runtime PSP flag");
            assert!(cli.psp);
            assert!(cli.config_overrides.raw_overrides.is_empty());
        }
    }

    #[test]
    fn app_server_code_mode_host_url_parses_independently_of_listen_transport() {
        let app_server = app_server_from_args(
            [
                "codex",
                "app-server",
                "--code-mode-host",
                "wss://example.test/code-mode",
                "--listen",
                "ws://127.0.0.1:4500",
            ]
            .as_ref(),
        );

        assert_eq!(
            app_server.code_mode_host.code_mode_host,
            Some(
                url::Url::parse("wss://example.test/code-mode")
                    .expect("test endpoint should parse")
            )
        );
        assert_eq!(
            app_server.listen,
            codex_app_server::AppServerTransport::WebSocket {
                bind_address: "127.0.0.1:4500".parse().expect("valid socket address"),
            }
        );
    }

    #[test]
    fn app_server_rejects_invalid_code_mode_host_urls() {
        for endpoint in [
            "http://127.0.0.1:8765",
            "ws://",
            "wss://example.test/code-mode#fragment",
        ] {
            let error =
                MultitoolCli::try_parse_from(["codex", "app-server", "--code-mode-host", endpoint])
                    .expect_err("invalid code-mode host endpoint should fail argument parsing");

            assert_eq!(error.kind(), clap::error::ErrorKind::ValueValidation);
        }
    }

    #[test]
    fn app_server_listen_websocket_url_parses() {
        let app_server = app_server_from_args(
            ["codex", "app-server", "--listen", "ws://127.0.0.1:4500"].as_ref(),
        );
        assert_eq!(
            app_server.listen,
            codex_app_server::AppServerTransport::WebSocket {
                bind_address: "127.0.0.1:4500".parse().expect("valid socket address"),
            }
        );
    }

    #[test]
    fn app_server_listen_stdio_url_parses() {
        let app_server =
            app_server_from_args(["codex", "app-server", "--listen", "stdio://"].as_ref());
        assert_eq!(
            app_server.listen,
            codex_app_server::AppServerTransport::Stdio
        );
    }

    #[test]
    fn app_server_stdio_flag_parses() {
        let app_server = app_server_from_args(["codex", "app-server", "--stdio"].as_ref());
        assert!(app_server.stdio);
    }

    #[test]
    fn app_server_stdio_flag_conflicts_with_listen() {
        let err = MultitoolCli::try_parse_from([
            "codex",
            "app-server",
            "--stdio",
            "--listen",
            "stdio://",
        ])
        .expect_err("--stdio and --listen should be rejected together");
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn app_server_listen_unix_socket_url_parses() {
        let app_server =
            app_server_from_args(["codex", "app-server", "--listen", "unix://"].as_ref());
        assert_eq!(
            app_server.listen,
            codex_app_server::AppServerTransport::UnixSocket {
                socket_path: default_app_server_socket_path()
            }
        );
    }

    #[test]
    fn app_server_listen_unix_socket_path_parses() {
        let app_server = app_server_from_args(
            ["codex", "app-server", "--listen", "unix:///tmp/codex.sock"].as_ref(),
        );
        assert_eq!(
            app_server.listen,
            codex_app_server::AppServerTransport::UnixSocket {
                socket_path: AbsolutePathBuf::from_absolute_path("/tmp/codex.sock")
                    .expect("absolute path should parse")
            }
        );
    }

    #[test]
    fn app_server_listen_off_parses() {
        let app_server = app_server_from_args(["codex", "app-server", "--listen", "off"].as_ref());
        assert_eq!(app_server.listen, codex_app_server::AppServerTransport::Off);
    }

    #[test]
    fn app_server_listen_invalid_url_fails_to_parse() {
        let parse_result =
            MultitoolCli::try_parse_from(["codex", "app-server", "--listen", "http://foo"]);
        assert!(parse_result.is_err());
    }

    #[test]
    fn app_server_proxy_subcommand_parses() {
        let app_server = app_server_from_args(["codex", "app-server", "proxy"].as_ref());
        assert!(matches!(
            app_server.subcommand,
            Some(AppServerSubcommand::Proxy(AppServerProxyCommand {
                socket_path: None
            }))
        ));
    }

    #[test]
    fn app_server_daemon_subcommands_parse() {
        assert!(matches!(
            app_server_from_args(
                [
                    "codex",
                    "app-server",
                    "daemon",
                    "bootstrap",
                    "--remote-control"
                ]
                .as_ref()
            )
            .subcommand,
            Some(AppServerSubcommand::Daemon(AppServerDaemonCommand {
                subcommand: AppServerDaemonSubcommand::Bootstrap(AppServerBootstrapCommand {
                    remote_control: true
                })
            }))
        ));
        assert!(matches!(
            app_server_from_args(["codex", "app-server", "daemon", "start"].as_ref()).subcommand,
            Some(AppServerSubcommand::Daemon(AppServerDaemonCommand {
                subcommand: AppServerDaemonSubcommand::Start
            }))
        ));
        assert!(matches!(
            app_server_from_args(["codex", "app-server", "daemon", "restart"].as_ref()).subcommand,
            Some(AppServerSubcommand::Daemon(AppServerDaemonCommand {
                subcommand: AppServerDaemonSubcommand::Restart
            }))
        ));
        assert!(matches!(
            app_server_from_args(
                ["codex", "app-server", "daemon", "enable-remote-control"].as_ref()
            )
            .subcommand,
            Some(AppServerSubcommand::Daemon(AppServerDaemonCommand {
                subcommand: AppServerDaemonSubcommand::EnableRemoteControl
            }))
        ));
        assert!(matches!(
            app_server_from_args(
                ["codex", "app-server", "daemon", "disable-remote-control"].as_ref()
            )
            .subcommand,
            Some(AppServerSubcommand::Daemon(AppServerDaemonCommand {
                subcommand: AppServerDaemonSubcommand::DisableRemoteControl
            }))
        ));
        assert!(matches!(
            app_server_from_args(["codex", "app-server", "daemon", "stop"].as_ref()).subcommand,
            Some(AppServerSubcommand::Daemon(AppServerDaemonCommand {
                subcommand: AppServerDaemonSubcommand::Stop
            }))
        ));
        assert!(matches!(
            app_server_from_args(["codex", "app-server", "daemon", "version"].as_ref()).subcommand,
            Some(AppServerSubcommand::Daemon(AppServerDaemonCommand {
                subcommand: AppServerDaemonSubcommand::Version
            }))
        ));
    }

    #[test]
    fn app_server_proxy_sock_path_parses() {
        let app_server =
            app_server_from_args(["codex", "app-server", "proxy", "--sock", "codex.sock"].as_ref());
        let Some(AppServerSubcommand::Proxy(proxy)) = app_server.subcommand else {
            panic!("expected proxy subcommand");
        };
        assert_eq!(
            proxy.socket_path,
            Some(
                AbsolutePathBuf::relative_to_current_dir("codex.sock")
                    .expect("relative path should resolve")
            )
        );
    }

    #[test]
    fn reject_remote_auth_token_env_for_app_server_proxy() {
        let subcommand = AppServerSubcommand::Proxy(AppServerProxyCommand { socket_path: None });
        let err = reject_remote_mode_for_app_server_subcommand(
            /*remote*/ None,
            Some("CODEX_REMOTE_AUTH_TOKEN"),
            Some(&subcommand),
        )
        .expect_err("app-server proxy should reject --remote-auth-token-env");
        assert!(err.to_string().contains("app-server proxy"));
    }

    #[test]
    fn reject_remote_auth_token_env_for_app_server_version() {
        let subcommand = AppServerSubcommand::Daemon(AppServerDaemonCommand {
            subcommand: AppServerDaemonSubcommand::Version,
        });
        let err = reject_remote_mode_for_app_server_subcommand(
            /*remote*/ None,
            Some("CODEX_REMOTE_AUTH_TOKEN"),
            Some(&subcommand),
        )
        .expect_err("app-server daemon version should reject --remote-auth-token-env");
        assert!(err.to_string().contains("app-server daemon version"));
    }

    #[test]
    fn app_server_capability_token_flags_parse() {
        let app_server = app_server_from_args(
            [
                "codex",
                "app-server",
                "--ws-auth",
                "capability-token",
                "--ws-token-file",
                "/tmp/codex-token",
            ]
            .as_ref(),
        );
        assert_eq!(
            app_server.auth.ws_auth,
            Some(codex_app_server::WebsocketAuthCliMode::CapabilityToken)
        );
        assert_eq!(
            app_server.auth.ws_token_file,
            Some(PathBuf::from("/tmp/codex-token"))
        );
    }

    #[test]
    fn app_server_signed_bearer_flags_parse() {
        let app_server = app_server_from_args(
            [
                "codex",
                "app-server",
                "--ws-auth",
                "signed-bearer-token",
                "--ws-shared-secret-file",
                "/tmp/codex-secret",
                "--ws-issuer",
                "issuer",
                "--ws-audience",
                "audience",
                "--ws-max-clock-skew-seconds",
                "9",
            ]
            .as_ref(),
        );
        assert_eq!(
            app_server.auth.ws_auth,
            Some(codex_app_server::WebsocketAuthCliMode::SignedBearerToken)
        );
        assert_eq!(
            app_server.auth.ws_shared_secret_file,
            Some(PathBuf::from("/tmp/codex-secret"))
        );
        assert_eq!(app_server.auth.ws_issuer.as_deref(), Some("issuer"));
        assert_eq!(app_server.auth.ws_audience.as_deref(), Some("audience"));
        assert_eq!(app_server.auth.ws_max_clock_skew_seconds, Some(9));
    }

    #[test]
    fn app_server_rejects_removed_insecure_non_loopback_flag() {
        let parse_result = MultitoolCli::try_parse_from([
            "codex",
            "app-server",
            "--allow-unauthenticated-non-loopback-ws",
        ]);
        assert!(parse_result.is_err());
    }

    #[test]
    fn features_enable_parses_feature_name() {
        let cli = MultitoolCli::try_parse_from(["codex", "features", "enable", "unified_exec"])
            .expect("parse should succeed");
        let Some(Subcommand::Features(FeaturesCli { sub })) = cli.subcommand else {
            panic!("expected features subcommand");
        };
        let FeaturesSubcommand::Enable(FeatureSetArgs { feature }) = sub else {
            panic!("expected features enable");
        };
        assert_eq!(feature, "unified_exec");
    }

    #[test]
    fn features_disable_parses_feature_name() {
        let cli = MultitoolCli::try_parse_from(["codex", "features", "disable", "shell_tool"])
            .expect("parse should succeed");
        let Some(Subcommand::Features(FeaturesCli { sub })) = cli.subcommand else {
            panic!("expected features subcommand");
        };
        let FeaturesSubcommand::Disable(FeatureSetArgs { feature }) = sub else {
            panic!("expected features disable");
        };
        assert_eq!(feature, "shell_tool");
    }

    #[test]
    fn feature_toggles_known_features_generate_overrides() {
        let toggles = FeatureToggles {
            enable: vec!["web_search_request".to_string()],
            disable: vec!["unified_exec".to_string()],
        };
        let overrides = toggles.to_overrides().expect("valid features");
        assert_eq!(
            overrides,
            vec![
                "features.web_search_request=true".to_string(),
                "features.unified_exec=false".to_string(),
            ]
        );
    }

    #[test]
    fn feature_toggles_accept_legacy_linux_sandbox_flag() {
        let toggles = FeatureToggles {
            enable: vec!["use_linux_sandbox_bwrap".to_string()],
            disable: Vec::new(),
        };
        let overrides = toggles.to_overrides().expect("valid features");
        assert_eq!(
            overrides,
            vec!["features.use_linux_sandbox_bwrap=true".to_string(),]
        );
    }

    #[test]
    fn feature_toggles_accept_removed_image_detail_original_flag() {
        let toggles = FeatureToggles {
            enable: vec!["image_detail_original".to_string()],
            disable: Vec::new(),
        };
        let overrides = toggles.to_overrides().expect("valid features");
        assert_eq!(
            overrides,
            vec!["features.image_detail_original=true".to_string(),]
        );
    }

    #[test]
    fn feature_toggles_accept_removed_enable_fanout_flag() {
        let toggles = FeatureToggles {
            enable: vec!["enable_fanout".to_string()],
            disable: Vec::new(),
        };
        let overrides = toggles.to_overrides().expect("valid features");
        assert_eq!(overrides, vec!["features.enable_fanout=true".to_string(),]);
    }

    #[test]
    fn feature_toggles_accept_removed_item_ids_flag() {
        let toggles = FeatureToggles {
            enable: vec!["item_ids".to_string()],
            disable: Vec::new(),
        };
        let overrides = toggles.to_overrides().expect("valid features");
        assert_eq!(overrides, vec!["features.item_ids=true".to_string()]);
    }

    #[test]
    fn feature_toggles_unknown_feature_errors() {
        let toggles = FeatureToggles {
            enable: vec!["does_not_exist".to_string()],
            disable: Vec::new(),
        };
        let err = toggles
            .to_overrides()
            .expect_err("feature should be rejected");
        assert_eq!(err.to_string(), "Unknown feature flag: does_not_exist");
    }

    #[test]
    fn strict_config_with_unknown_enable_errors() {
        let err = strict_config_feature_toggle_error(["--enable", "does_not_exist"].as_ref());
        assert_eq!(err.to_string(), "Unknown feature flag: does_not_exist");
    }

    #[test]
    fn strict_config_with_unknown_disable_errors() {
        let err = strict_config_feature_toggle_error(["--disable", "does_not_exist"].as_ref());
        assert_eq!(err.to_string(), "Unknown feature flag: does_not_exist");
    }

    #[test]
    fn strict_config_with_compound_enable_errors() {
        let err = strict_config_feature_toggle_error(
            ["--enable", "multi_agent_v2.subagent_usage_hint_text"].as_ref(),
        );
        assert_eq!(
            err.to_string(),
            "Unknown feature flag: multi_agent_v2.subagent_usage_hint_text"
        );
    }

    fn strict_config_feature_toggle_error(args: &[&str]) -> anyhow::Error {
        let cli_args = std::iter::once("codex")
            .chain(std::iter::once("--strict-config"))
            .chain(args.iter().copied());
        let cli = MultitoolCli::try_parse_from(cli_args).expect("parse should succeed");
        assert!(cli.interactive.strict_config);
        cli.feature_toggles
            .to_overrides()
            .expect_err("feature should be rejected")
    }
}

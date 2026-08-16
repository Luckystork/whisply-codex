//! Explicit CLI client for the installed application's live diagnostic lane.
//!
//! This is deliberately distinct from the zero-authority fixture and replay
//! lanes. It speaks only the fixed, owner-only live-diagnostics v1 protocol
//! already implemented by the installed app, validates the installed release
//! manifest before every command, and never creates account, connector, or
//! permission authority. A requested run still travels through the normal app
//! policy, confirmation, Usage, Browser/Chrome, Computer Use, and connector
//! boundaries.

use clap::Args;
use std::path::PathBuf;

#[derive(Debug, Args)]
pub(crate) struct InstalledLiveCommand {
    #[command(subcommand)]
    action: InstalledLiveAction,
}

#[derive(Debug, clap::Subcommand)]
enum InstalledLiveAction {
    /// Create an owner-only session and attach the verified installed app.
    Start(InstalledLiveStartArgs),
    /// List retained owner-only live-diagnostic sessions without launching or
    /// messaging the installed app.
    List(InstalledLiveListArgs),
    /// Read the installed-app, signature, process, and owner-only session
    /// inventory without launching or commanding the app.
    Doctor(InstalledLiveDoctorArgs),
    /// Read the installed app's signed live-diagnostic status.
    Status(InstalledLiveSessionArgs),
    /// Start one ordinary, policy-bound app run through the installed app.
    Run(InstalledLiveRunArgs),
    /// Wait for complete signed evidence and validate its release binding.
    Wait(InstalledLiveWaitArgs),
    /// Validate a completed run's signed, local evidence bundle.
    Assert(InstalledLiveRunReferenceArgs),
    /// Export one completed run as a bounded, non-exact, redacted archive.
    Export(InstalledLiveExportArgs),
    /// Validate one redacted live-export archive without launching the app.
    ReplayExport(InstalledLiveReplayExportArgs),
    /// Ask the installed app to stop one exact active run.
    Stop(InstalledLiveRunReferenceArgs),
    /// Capture one installed-app diagnostic snapshot for an exact active run.
    Snapshot(InstalledLiveRunReferenceArgs),
    /// Inspect recovery for the current account's existing runtime work. An
    /// optional cancellation stays constrained to the installed app's exact
    /// unfunded, automatic, superseded Computer Use setup task.
    RuntimeRecovery(InstalledLiveRuntimeRecoveryArgs),
    /// Return redacted event names from one exact local evidence bundle.
    Tail(InstalledLiveTailArgs),
    /// Inspect token classes from signed, owner-only local run evidence without
    /// launching or messaging the installed app.
    Tokens(InstalledLiveUsageInspectionArgs),
    /// Inspect provider-reported cost entries from signed, owner-only local
    /// run evidence without launching or messaging the installed app.
    Cost(InstalledLiveUsageInspectionArgs),
    /// Inspect redacted policy inputs, signed policy outcomes, and the final
    /// completed-run authority snapshot without launching or messaging the app.
    Policy(InstalledLivePolicyInspectionArgs),
    /// Inspect retained Usage reservation, settlement, and receipt-projection
    /// evidence without launching or messaging the app.
    Usage(InstalledLiveUsageReceiptInspectionArgs),
    /// Inspect the current account's aggregate connector availability.
    AccountProjection(InstalledLiveSessionArgs),
    /// Inspect how close a long conversation is to being shortened, and what
    /// each shortening did.
    Compaction(InstalledLiveCompactionArgs),
    /// Close the owner-only live-diagnostic session.
    Close(InstalledLiveSessionArgs),
}

#[derive(Debug, Args)]
struct InstalledLiveStartArgs {
    /// Session lifetime in seconds (30 through 3600).
    #[arg(long, default_value_t = 1_800, value_parser = clap::value_parser!(u32).range(30..=3_600))]
    lifetime_secs: u32,
    /// Seconds to wait for the installed app to attach.
    #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u32).range(1..=120))]
    attach_timeout_secs: u32,
}

#[derive(Debug, Args)]
struct InstalledLiveListArgs {
    /// Maximum retained owner-only session summaries to return.
    #[arg(long, default_value_t = 16, value_parser = clap::value_parser!(u8).range(1..=16))]
    limit: u8,
}

#[derive(Debug, Args)]
struct InstalledLiveDoctorArgs {
    /// Canonical session UUID to inspect; defaults to the owner-only current
    /// session pointer when one exists.
    #[arg(long)]
    session: Option<String>,
}

#[derive(Debug, Args)]
struct InstalledLiveSessionArgs {
    /// Canonical session UUID; defaults to the owner-only current session.
    #[arg(long)]
    session: Option<String>,
    /// Seconds to wait for the installed app response.
    #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u32).range(1..=120))]
    timeout_secs: u32,
}

#[derive(Debug, Args)]
struct InstalledLiveRunArgs {
    /// Canonical session UUID; defaults to the owner-only current session.
    #[arg(long)]
    session: Option<String>,
    /// Visible prompt sent through the normal installed-product path.
    #[arg(long, value_name = "PROMPT")]
    prompt: String,
    /// Optional signed-catalog model identifier.
    #[arg(long)]
    model: Option<String>,
    /// Exact currently available connected-source selector; repeat up to three times.
    #[arg(long = "source")]
    sources: Vec<String>,
    /// One already-enabled built-in tool surface to exercise.
    #[arg(long, value_enum)]
    plugin: Option<InstalledLivePlugin>,
    /// Use the ordinary app's selected screen route.
    #[arg(long)]
    screen: bool,
    /// Exercise the normal native Computer Use route.
    #[arg(long)]
    computer_use: bool,
    /// Explicit route when no plug-in selects it.
    #[arg(long, value_enum, default_value_t = InstalledLiveRoute::Normal)]
    route: InstalledLiveRoute,
    /// Continue the current chat instead of creating a new one.
    #[arg(long)]
    continue_chat: bool,
    /// Do not ask the installed app to capture local UI artifacts.
    #[arg(long)]
    no_artifacts: bool,
    /// Request a paired Computer Use efficiency cohort. The installed app
    /// decides whether it can run one, and refuses when it cannot vary the
    /// cohort; a run without this flag measures the shipping path.
    #[arg(long, value_enum)]
    computer_use_efficiency_cohort: Option<InstalledLiveComputerUseCohort>,
    /// Wait for the evidence bundle and validate it before returning.
    #[arg(long)]
    wait: bool,
    /// Seconds to wait when `--wait` is supplied.
    #[arg(long, default_value_t = 600, value_parser = clap::value_parser!(u32).range(1..=3_600))]
    wait_timeout_secs: u32,
    /// Seconds to wait for the initial installed-app response.
    #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u32).range(1..=120))]
    timeout_secs: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, clap::ValueEnum)]
enum InstalledLivePlugin {
    Browser,
    Chrome,
    #[value(name = "computer-use")]
    ComputerUse,
    Documents,
    Pdf,
    Spreadsheets,
    Presentations,
}

impl InstalledLivePlugin {
    const fn protocol_id(self) -> &'static str {
        match self {
            Self::Browser => "browser",
            Self::Chrome => "chrome",
            Self::ComputerUse => "computer_use",
            Self::Documents => "documents",
            Self::Pdf => "pdf",
            Self::Spreadsheets => "spreadsheets",
            Self::Presentations => "presentations",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, clap::ValueEnum)]
enum InstalledLiveRoute {
    Normal,
    #[value(name = "native-computer-use")]
    NativeComputerUse,
}

impl InstalledLiveRoute {
    const fn protocol_id(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::NativeComputerUse => "native-computer-use",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, clap::ValueEnum)]
enum InstalledLiveComputerUseCohort {
    Semantic,
    #[value(name = "visual-loop")]
    VisualLoop,
}

impl InstalledLiveComputerUseCohort {
    const fn protocol_id(self) -> &'static str {
        match self {
            Self::Semantic => "semantic",
            Self::VisualLoop => "visual-loop",
        }
    }
}

#[derive(Debug, Args)]
struct InstalledLiveWaitArgs {
    /// Canonical session UUID; defaults to the owner-only current session.
    #[arg(long)]
    session: Option<String>,
    /// Exact run UUID returned by `diagnostics live run`.
    run_id: String,
    /// Seconds to wait for the final evidence manifest.
    #[arg(long, default_value_t = 600, value_parser = clap::value_parser!(u32).range(1..=3_600))]
    timeout_secs: u32,
}

#[derive(Debug, Args)]
struct InstalledLiveRunReferenceArgs {
    /// Canonical session UUID; defaults to the owner-only current session.
    #[arg(long)]
    session: Option<String>,
    /// Exact run UUID returned by `diagnostics live run`.
    run_id: String,
    /// Seconds to wait for the installed app response, when applicable.
    #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u32).range(1..=120))]
    timeout_secs: u32,
}

#[derive(Debug, Args)]
struct InstalledLiveRuntimeRecoveryArgs {
    /// Canonical session UUID; defaults to the owner-only current session.
    #[arg(long)]
    session: Option<String>,
    /// Ask the installed app to cancel only its exact eligible automatic,
    /// unfunded, superseded Computer Use setup task. The CLI never supplies a
    /// task id or changes eligibility.
    #[arg(long)]
    cancel_superseded_computer_use_setup: bool,
    /// Seconds to wait for the installed app response.
    #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u32).range(1..=120))]
    timeout_secs: u32,
}

#[derive(Debug, Args)]
struct InstalledLiveExportArgs {
    /// Canonical session UUID; defaults to the owner-only current session.
    #[arg(long)]
    session: Option<String>,
    /// Exact run UUID returned by `diagnostics live run`.
    run_id: String,
    /// New archive destination inside an existing owner-only directory.
    output: PathBuf,
}

#[derive(Debug, Args)]
struct InstalledLiveReplayExportArgs {
    /// Existing owner-only redacted archive to validate offline.
    export: PathBuf,
    /// Optional exact run UUID expected in the archive.
    #[arg(long)]
    run_id: Option<String>,
    /// Optional exact installed release-manifest hash expected in the archive.
    #[arg(long)]
    release_manifest_sha256: Option<String>,
}

#[derive(Debug, Args)]
struct InstalledLiveCompactionArgs {
    /// Canonical session UUID; defaults to the owner-only current session.
    #[arg(long)]
    session: Option<String>,
    /// Conversation to report on; defaults to the chat the app has open.
    #[arg(long, value_name = "THREAD_ID")]
    thread: Option<String>,
    /// Seconds to wait for the installed app response.
    #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u32).range(1..=120))]
    timeout_secs: u32,
}

#[derive(Debug, Args)]
struct InstalledLiveTailArgs {
    /// Canonical session UUID; defaults to the owner-only current session.
    #[arg(long)]
    session: Option<String>,
    /// Exact run UUID returned by `diagnostics live run`.
    run_id: String,
    /// Return only redacted event summaries after this signed event sequence.
    #[arg(long, default_value_t = 0)]
    since: u64,
    /// Maximum number of redacted event summaries to return.
    #[arg(long, default_value_t = 1_000, value_parser = clap::value_parser!(u16).range(1..=10_000))]
    limit: u16,
}

#[derive(Debug, Args)]
struct InstalledLiveUsageInspectionArgs {
    /// Canonical session UUID; defaults to the owner-only current session.
    #[arg(long)]
    session: Option<String>,
    /// Exact run UUID returned by `diagnostics live run`.
    #[arg(long)]
    run_id: String,
    /// Return only provider-usage evidence after this signed event sequence.
    #[arg(long, default_value_t = 0)]
    since: u64,
    /// Maximum bounded token or cost records to return.
    #[arg(long, default_value_t = 1_000, value_parser = clap::value_parser!(u16).range(1..=1_000))]
    limit: u16,
}

#[derive(Debug, Args)]
struct InstalledLivePolicyInspectionArgs {
    /// Canonical session UUID; defaults to the owner-only current session.
    #[arg(long)]
    session: Option<String>,
    /// Exact completed run UUID returned by `diagnostics live run`.
    #[arg(long)]
    run_id: String,
    /// Return only signed policy outcomes after this event sequence.
    #[arg(long, default_value_t = 0)]
    since: u64,
    /// Maximum bounded policy-outcome records to return.
    #[arg(long, default_value_t = 1_000, value_parser = clap::value_parser!(u16).range(1..=1_000))]
    limit: u16,
}

#[derive(Debug, Args)]
struct InstalledLiveUsageReceiptInspectionArgs {
    /// Canonical session UUID; defaults to the owner-only current session.
    #[arg(long)]
    session: Option<String>,
    /// Exact completed run UUID returned by `diagnostics live run`.
    #[arg(long)]
    run_id: String,
    /// Maximum bounded task Usage receipt projections to return.
    #[arg(long, default_value_t = 64, value_parser = clap::value_parser!(u8).range(1..=64))]
    limit: u8,
}

pub(crate) fn run(command: InstalledLiveCommand) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        macos::run(command)
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = command;
        anyhow::bail!("Installed live diagnostics are available only on macOS.");
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::collections::BTreeMap;
    use std::fs;
    use std::fs::OpenOptions;
    use std::io::Read;
    use std::io::Write;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Component;
    use std::path::Path;
    use std::path::PathBuf;
    use std::process::Command;
    use std::thread;
    use std::time::Duration;
    use std::time::SystemTime;
    use std::time::UNIX_EPOCH;

    use anyhow::Context;
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
    use hmac::Hmac;
    use hmac::Mac as _;
    use serde::Deserialize;
    use serde::Serialize;
    use serde_json::Map;
    use serde_json::Value;
    use sha2::Digest as _;
    use sha2::Sha256;
    use uuid::Uuid;
    use zeroize::Zeroizing;

    use super::InstalledLiveAction;
    use super::InstalledLiveCommand;
    use super::InstalledLiveComputerUseCohort;
    use super::InstalledLivePlugin;
    use super::InstalledLiveRoute;

    const LIVE_PROTOCOL: &str = "whisply.live-diagnostics.v1";
    const APP_PATH: &str = "/Applications/Whisply.app";
    const RUNTIME_MANIFEST_RELATIVE_PATH: &str =
        "Contents/Resources/Runtime/whisply-runtime-release-manifest.json";
    const MAX_SESSION_LIFETIME_MS: i64 = 60 * 60 * 1_000;
    const MAX_COMMAND_LIFETIME_MS: i64 = 2 * 60 * 1_000;
    const MAX_COMMAND_PAYLOAD_BYTES: usize = 1_024 * 1_024;
    const MAX_EVIDENCE_BYTES: u64 = 64 * 1_024 * 1_024;
    const MAX_JSON_BYTES: usize = 2 * MAX_COMMAND_PAYLOAD_BYTES;
    const MAX_EVENT_LINES: usize = 10_000;
    const MAX_LISTED_SESSIONS: usize = 16;
    const MAX_USAGE_INSPECTION_RECORDS: usize = 1_000;
    const MAX_POLICY_INSPECTION_RECORDS: usize = 1_000;
    const MAX_USAGE_RECEIPT_TASKS: usize = 64;
    const MAX_USAGE_RECEIPT_COMPONENTS: usize = 1_000;
    const MAX_USAGE_RECEIPT_LIFECYCLE_EVENTS: usize = 1_000;
    const MAX_PROVIDER_USAGE_TOKENS: u64 = 1_000_000_000;
    const MAX_PROVIDER_REPORTED_COST: f64 = 1_000_000.0;
    const MAX_USAGE_PROJECTION_UNITS: u64 = 1_000_000_000;
    const MAX_POLICY_RETRY_COUNT: u64 = 100;
    const PROVIDER_USAGE_TOKEN_CLASSES: [&str; 8] = [
        "inputTokens",
        "outputTokens",
        "totalTokens",
        "cachedInputTokens",
        "reasoningTokens",
        "cacheWriteTokens",
        "audioInputTokens",
        "audioOutputTokens",
    ];
    const USAGE_RESERVATION_STATES: [&str; 6] = [
        "active",
        "settled",
        "released",
        "expired",
        "cancelled",
        "reconciliation_required",
    ];
    const USAGE_LIFECYCLE_EVENT_KINDS: [&str; 8] = [
        "reserved",
        "settled",
        "released",
        "expired",
        "cancelled",
        "reconciliation_required",
        "adjusted",
        "reconciled",
    ];
    const USAGE_RECEIPT_COMPONENT_KINDS: [&str; 17] = [
        "model.input_token",
        "model.cached_input_token",
        "model.output_token",
        "web_search.call",
        "web_retrieval.input_token",
        "connector.operation",
        "computer_use.planning_turn",
        "computer_use.screenshot",
        "computer_use.action_interpretation",
        "computer_use.browser_step",
        "computer_use.native_step",
        "computer_use.retry",
        "computer_use.verification_turn",
        "hosted_runtime.millisecond",
        "memory.processing_turn",
        "skill.analysis_turn",
        "skill.test_step",
    ];

    #[path = "installed_live_export.rs"]
    mod live_export;
    #[path = "installed_live_retention.rs"]
    mod live_export_retention;
    #[path = "installed_live_export_validation.rs"]
    mod live_export_validation;

    type HmacSha256 = Hmac<Sha256>;

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Bootstrap {
        protocol_name: String,
        session_id: String,
        release_manifest_sha256: String,
        created_at_ms: i64,
        expires_at_ms: i64,
        secret_base64: String,
        controller_pid: i32,
        purpose: String,
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(rename_all = "camelCase")]
    struct ControllerState {
        protocol: String,
        session_id: String,
        next_command_sequence: u64,
        created_at_ms: i64,
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(rename_all = "camelCase")]
    struct CurrentSession {
        protocol: String,
        session_id: String,
        #[serde(default)]
        session_directory: String,
        updated_at_ms: i64,
    }

    /// Read-only summary. It deliberately omits the owner-only directory,
    /// bootstrap secret, controller pid, and release bind so listing sessions
    /// cannot become an authority or filesystem-discovery surface.
    #[derive(Debug, Serialize)]
    #[serde(rename_all = "camelCase")]
    struct ListedLiveSession {
        #[serde(rename = "sessionID")]
        session_id: String,
        created_at_ms: i64,
        expires_at_ms: i64,
        expired: bool,
        current: bool,
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(rename_all = "camelCase")]
    struct SignedEnvelope {
        protocol_name: String,
        session_id: String,
        envelope_id: String,
        sequence: u64,
        issued_at_ms: i64,
        expires_at_ms: i64,
        kind: String,
        name: String,
        payload_base64: String,
        hmac_sha256: String,
    }

    /// Parsed only after the owner-only event envelope has been verified. The
    /// raw fields never leave this private helper; the token/cost commands
    /// project a narrow allowlist from them below.
    #[derive(Clone, Debug)]
    struct VerifiedRunEvent {
        sequence: u64,
        timestamp_ms: i64,
        name: String,
        fields: Value,
    }

    /// A deliberately small view of one `provider.usage` event. It does not
    /// retain a request ID, response hash, provider/model string, or raw cost
    /// detail payload, so future formatting cannot accidentally print them.
    #[derive(Clone, Debug)]
    struct VerifiedProviderUsage {
        sequence: u64,
        timestamp_ms: i64,
        token_classes: Vec<(&'static str, u64)>,
        provider_reported_cost: Option<f64>,
        cost_details_present: bool,
    }

    /// A redacted policy record retained only long enough to format the local
    /// inspection response. No raw tool, target, error, or authority field is
    /// carried across this boundary.
    #[derive(Debug)]
    struct VerifiedPolicyResult {
        sequence: u64,
        record: Value,
    }

    #[derive(Debug)]
    struct PolicyResultInspection {
        event_count: usize,
        records: Vec<Value>,
        truncated: bool,
        next_since: Value,
    }

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct ResponsePayload {
        ok: bool,
        request_id: String,
        command: String,
        payload: Value,
    }

    struct LiveSession {
        id: String,
        directory: PathBuf,
        release_manifest_sha256: String,
        expires_at_ms: i64,
        secret: Zeroizing<Vec<u8>>,
    }

    impl std::fmt::Debug for LiveSession {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter
                .debug_struct("LiveSession")
                .field("id", &self.id)
                .field("directory", &"[owner-only diagnostic root]")
                .field("release_manifest_sha256", &self.release_manifest_sha256)
                .field("expires_at_ms", &self.expires_at_ms)
                .field("secret", &"[redacted]")
                .finish()
        }
    }

    pub(super) fn run(command: InstalledLiveCommand) -> anyhow::Result<()> {
        match command.action {
            InstalledLiveAction::Start(args) => {
                let mut session = LiveSession::create(args.lifetime_secs)?;
                launch_installed_app(&session.id)?;
                let response =
                    session.send("status", serde_json::json!({}), args.attach_timeout_secs)?;
                print_json(&serde_json::json!({
                    "ok": true,
                    "protocol": LIVE_PROTOCOL,
                    "sessionID": session.id,
                    "releaseManifestSha256": session.release_manifest_sha256,
                    "status": response.payload,
                    "commercialBoundary": "normal installed-product policy and confirmation gates only",
                }))
            }
            InstalledLiveAction::List(args) => print_json(&list_sessions(usize::from(args.limit))?),
            InstalledLiveAction::Doctor(args) => print_json(&doctor(args.session.as_deref())?),
            InstalledLiveAction::Status(args) => {
                let mut session = LiveSession::load(args.session.as_deref())?;
                let response = session.send("status", serde_json::json!({}), args.timeout_secs)?;
                print_json(&serde_json::json!({ "ok": true, "status": response.payload }))
            }
            InstalledLiveAction::Run(args) => run_live(args),
            InstalledLiveAction::Wait(args) => {
                let session = LiveSession::load(args.session.as_deref())?;
                let summary = session.wait_for_run(&args.run_id, args.timeout_secs)?;
                print_json(&summary)
            }
            InstalledLiveAction::Assert(args) => {
                let session = LiveSession::load_allow_expired(args.session.as_deref())?;
                let summary = session.assert_run(&args.run_id)?;
                print_json(&summary)
            }
            InstalledLiveAction::Export(args) => {
                let session = LiveSession::load_allow_expired(args.session.as_deref())?;
                let summary = live_export::export_run(&session, &args.run_id, &args.output)?;
                print_json(&summary)
            }
            InstalledLiveAction::ReplayExport(args) => {
                let summary = live_export_validation::replay_export(
                    &args.export,
                    args.run_id.as_deref(),
                    args.release_manifest_sha256.as_deref(),
                )?;
                print_json(&summary)
            }
            InstalledLiveAction::Stop(args) => {
                let mut session = LiveSession::load(args.session.as_deref())?;
                let run_id = canonical_uuid(&args.run_id)?;
                let response = session.send(
                    "stop",
                    serde_json::json!({ "runID": run_id }),
                    args.timeout_secs,
                )?;
                print_json(&serde_json::json!({ "ok": true, "response": response.payload }))
            }
            InstalledLiveAction::Snapshot(args) => {
                let mut session = LiveSession::load(args.session.as_deref())?;
                let run_id = canonical_uuid(&args.run_id)?;
                let response = session.send(
                    "snapshot",
                    serde_json::json!({ "runID": run_id }),
                    args.timeout_secs,
                )?;
                print_json(&serde_json::json!({ "ok": true, "response": response.payload }))
            }
            InstalledLiveAction::RuntimeRecovery(args) => {
                let mut session = LiveSession::load(args.session.as_deref())?;
                let response = session.send(
                    "runtime-recovery",
                    runtime_recovery_payload(args.cancel_superseded_computer_use_setup),
                    args.timeout_secs,
                )?;
                print_json(&serde_json::json!({ "ok": true, "response": response.payload }))
            }
            InstalledLiveAction::Tail(args) => {
                let session = LiveSession::load_allow_expired(args.session.as_deref())?;
                let summaries =
                    session.event_summaries(&args.run_id, args.since, usize::from(args.limit))?;
                print_json(&serde_json::json!({
                    "ok": true,
                    "runID": canonical_uuid(&args.run_id)?,
                    "events": summaries,
                }))
            }
            InstalledLiveAction::Tokens(args) => {
                let session = LiveSession::load_allow_expired(args.session.as_deref())?;
                print_json(&session.token_class_inspection(
                    &args.run_id,
                    args.since,
                    usize::from(args.limit),
                )?)
            }
            InstalledLiveAction::Cost(args) => {
                let session = LiveSession::load_allow_expired(args.session.as_deref())?;
                print_json(&session.cost_inspection(
                    &args.run_id,
                    args.since,
                    usize::from(args.limit),
                )?)
            }
            InstalledLiveAction::Policy(args) => {
                let session = LiveSession::load_allow_expired(args.session.as_deref())?;
                print_json(&session.policy_inspection(
                    &args.run_id,
                    args.since,
                    usize::from(args.limit),
                )?)
            }
            InstalledLiveAction::Usage(args) => {
                let session = LiveSession::load_allow_expired(args.session.as_deref())?;
                print_json(
                    &session.usage_receipt_inspection(&args.run_id, usize::from(args.limit))?,
                )
            }
            InstalledLiveAction::AccountProjection(args) => {
                let mut session = LiveSession::load(args.session.as_deref())?;
                let response = session.send(
                    "account-projection",
                    serde_json::json!({}),
                    args.timeout_secs,
                )?;
                print_json(&serde_json::json!({ "ok": true, "projection": response.payload }))
            }
            InstalledLiveAction::Compaction(args) => {
                let mut session = LiveSession::load(args.session.as_deref())?;
                let thread = match args.thread.as_deref() {
                    Some(thread) => Some(canonical_conversation_id(thread)?),
                    None => None,
                };
                let response = session.send(
                    "compaction",
                    serde_json::json!({ "productThreadID": thread }),
                    args.timeout_secs,
                )?;
                print_json(&serde_json::json!({ "ok": true, "compaction": response.payload }))
            }
            InstalledLiveAction::Close(args) => {
                let mut session = LiveSession::load(args.session.as_deref())?;
                let response = session.send("close", serde_json::json!({}), args.timeout_secs)?;
                print_json(&serde_json::json!({ "ok": true, "response": response.payload }))
            }
        }
    }

    fn run_live(args: super::InstalledLiveRunArgs) -> anyhow::Result<()> {
        validate_prompt(&args.prompt)?;
        validate_sources(&args.sources)?;

        let plugin = args.plugin.map(InstalledLivePlugin::protocol_id);
        let mut route = args.route;
        let mut effective_computer_use = args.computer_use;
        if matches!(args.plugin, Some(InstalledLivePlugin::ComputerUse)) {
            if args.computer_use
                || args.route != InstalledLiveRoute::Normal
                || !args.sources.is_empty()
            {
                anyhow::bail!("The Computer Use plug-in selects its own native route.");
            }
            route = InstalledLiveRoute::NativeComputerUse;
            effective_computer_use = true;
        }
        if matches!(
            args.plugin,
            Some(InstalledLivePlugin::Browser | InstalledLivePlugin::Chrome)
        ) && (!args.sources.is_empty()
            || args.screen
            || args.computer_use
            || args.route != InstalledLiveRoute::Normal)
        {
            anyhow::bail!("Browser and Chrome tests require their exact standalone browser route.");
        }
        if !args.sources.is_empty() && route == InstalledLiveRoute::NativeComputerUse {
            anyhow::bail!(
                "A connected source and native Computer Use cannot own the same diagnostic turn."
            );
        }
        if args.computer_use_efficiency_cohort.is_some() {
            if route != InstalledLiveRoute::NativeComputerUse
                || args.screen
                || !args.sources.is_empty()
                || !matches!(args.plugin, None | Some(InstalledLivePlugin::ComputerUse))
                || args.model.as_deref() != Some("~openai/gpt-5.6-sol")
            {
                anyhow::bail!(
                    "A Computer Use efficiency cohort requires only the native route and --model ~openai/gpt-5.6-sol."
                );
            }
            effective_computer_use = true;
        }

        let mut session = LiveSession::load(args.session.as_deref())?;
        let response = session.send(
            "run",
            serde_json::json!({
                "prompt": args.prompt,
                "modelID": args.model,
                "sourceSelectors": args.sources,
                "pluginID": plugin,
                "usesScreen": args.screen,
                "computerUse": effective_computer_use,
                "route": route.protocol_id(),
                "newChat": !args.continue_chat,
                "captureArtifacts": !args.no_artifacts,
                "computerUseEfficiencyCohort": args.computer_use_efficiency_cohort.map(InstalledLiveComputerUseCohort::protocol_id),
            }),
            args.timeout_secs,
        )?;
        let run_id = response
            .payload
            .get("runID")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                anyhow::anyhow!("Installed app returned no live diagnostic run identifier.")
            })?;
        let run_id = canonical_uuid(run_id)?;
        let mut report = serde_json::json!({
            "ok": true,
            "run": response.payload,
            "commercialBoundary": "normal installed-product policy and confirmation gates only",
        });
        if args.wait {
            report["completion"] = session.wait_for_run(&run_id, args.wait_timeout_secs)?;
        }
        print_json(&report)
    }

    impl LiveSession {
        fn create(lifetime_secs: u32) -> anyhow::Result<Self> {
            let now_ms = unix_ms()?;
            let lifetime_ms = i64::from(lifetime_secs) * 1_000;
            if !(30_000..=MAX_SESSION_LIFETIME_MS).contains(&lifetime_ms) {
                anyhow::bail!(
                    "Live diagnostic session lifetime must be between 30 and 3600 seconds."
                );
            }
            let root = sessions_root()?;
            ensure_private_directory(&root)?;
            let session_id = Uuid::new_v4().to_string().to_ascii_lowercase();
            let directory = root.join(&session_id);
            create_new_private_directory(&directory)?;
            for child in ["requests", "responses", "runs"] {
                create_new_private_directory(&directory.join(child))?;
            }
            write_new_private(&directory.join(".controller.lock"), b"")?;
            let release_manifest_sha256 = installed_release_manifest_sha256()?;
            let secret = random_secret();
            let bootstrap = Bootstrap {
                protocol_name: LIVE_PROTOCOL.to_string(),
                session_id: session_id.clone(),
                release_manifest_sha256: release_manifest_sha256.clone(),
                created_at_ms: now_ms,
                expires_at_ms: now_ms.saturating_add(lifetime_ms),
                secret_base64: STANDARD.encode(&secret),
                controller_pid: std::process::id() as i32,
                purpose: "installed-live-product-path".to_string(),
            };
            write_new_json(&directory.join("bootstrap.json"), &bootstrap)?;
            write_new_json(
                &directory.join("controller-state.json"),
                &ControllerState {
                    protocol: LIVE_PROTOCOL.to_string(),
                    session_id: session_id.clone(),
                    next_command_sequence: 1,
                    created_at_ms: now_ms,
                },
            )?;
            write_replace_json(
                &current_session_path(&root)?,
                &CurrentSession {
                    protocol: LIVE_PROTOCOL.to_string(),
                    session_id: session_id.clone(),
                    session_directory: directory.to_string_lossy().into_owned(),
                    updated_at_ms: now_ms,
                },
            )?;
            Ok(Self {
                id: session_id,
                directory,
                release_manifest_sha256,
                expires_at_ms: bootstrap.expires_at_ms,
                secret: Zeroizing::new(secret),
            })
        }

        fn load(requested_session: Option<&str>) -> anyhow::Result<Self> {
            Self::load_inner(requested_session, false)
        }

        fn load_allow_expired(requested_session: Option<&str>) -> anyhow::Result<Self> {
            Self::load_inner(requested_session, true)
        }

        fn load_inner(
            requested_session: Option<&str>,
            allow_expired: bool,
        ) -> anyhow::Result<Self> {
            let root = existing_sessions_root()?;
            let id = match requested_session {
                Some(value) => canonical_uuid(value)?,
                None => {
                    let pointer: CurrentSession = read_private_json(&current_session_path(&root)?)?;
                    if pointer.protocol != LIVE_PROTOCOL {
                        anyhow::bail!("The owner-only live diagnostic session pointer is invalid.");
                    }
                    let id = canonical_uuid(&pointer.session_id)?;
                    if !pointer.session_directory.is_empty()
                        && Path::new(&pointer.session_directory) != root.join(&id)
                    {
                        anyhow::bail!("The owner-only live diagnostic session pointer is invalid.");
                    }
                    id
                }
            };
            let directory = root.join(&id);
            validate_private_directory(&directory)?;
            for child in ["requests", "responses", "runs"] {
                validate_private_directory(&directory.join(child))?;
            }
            validate_private_file(&directory.join(".controller.lock"))?;
            let bootstrap: Bootstrap = read_private_json(&directory.join("bootstrap.json"))?;
            validate_bootstrap(&bootstrap, &id, allow_expired)?;
            let secret = STANDARD
                .decode(bootstrap.secret_base64.as_bytes())
                .map_err(|_| {
                    anyhow::anyhow!("The owner-only live diagnostic session is invalid.")
                })?;
            if secret.len() != 32 {
                anyhow::bail!("The owner-only live diagnostic session is invalid.");
            }
            let current_manifest = installed_release_manifest_sha256()?;
            if current_manifest != bootstrap.release_manifest_sha256 {
                anyhow::bail!(
                    "The live diagnostic session belongs to a different installed Whisply release."
                );
            }
            Ok(Self {
                id,
                directory,
                release_manifest_sha256: bootstrap.release_manifest_sha256,
                expires_at_ms: bootstrap.expires_at_ms,
                secret: Zeroizing::new(secret),
            })
        }

        fn state_payload(&self) -> anyhow::Result<Option<Value>> {
            let path = self.directory.join("state.json");
            match fs::symlink_metadata(&path) {
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(_) => anyhow::bail!("The owner-only live diagnostic state is unavailable."),
            }
            let envelope: SignedEnvelope = read_private_json(&path)?;
            let bytes = self.verify_envelope(&envelope, Some("state"), None, None, false)?;
            let payload: Value = serde_json::from_slice(&bytes).map_err(|_| {
                anyhow::anyhow!("The signed installed-app diagnostic state is invalid.")
            })?;
            if !payload.is_object() {
                anyhow::bail!("The signed installed-app diagnostic state is invalid.");
            }
            Ok(Some(payload))
        }

        fn send(
            &mut self,
            name: &str,
            payload: Value,
            timeout_secs: u32,
        ) -> anyhow::Result<ResponsePayload> {
            if !(1..=120).contains(&timeout_secs) || !valid_command_name(name) {
                anyhow::bail!("Installed live diagnostic command is invalid.");
            }
            let payload_bytes = serde_json::to_vec(&payload)?;
            if payload_bytes.len() > MAX_COMMAND_PAYLOAD_BYTES {
                anyhow::bail!("Installed live diagnostic command payload is too large.");
            }
            let sequence = self.next_sequence()?;
            let now_ms = unix_ms()?;
            let envelope_id = Uuid::new_v4().to_string().to_ascii_lowercase();
            let envelope = self.sign_envelope(
                envelope_id.clone(),
                sequence,
                "command",
                name,
                payload_bytes,
                now_ms,
                now_ms.saturating_add(i64::from(timeout_secs) * 1_000),
            )?;
            let request_path = self
                .directory
                .join("requests")
                .join(format!("{sequence:020}-{envelope_id}.json"));
            write_new_json(&request_path, &envelope)?;
            let response_path = self
                .directory
                .join("responses")
                .join(format!("{envelope_id}.json"));
            let deadline = std::time::Instant::now() + Duration::from_secs(u64::from(timeout_secs));
            while std::time::Instant::now() < deadline {
                if response_path.exists() {
                    let response: SignedEnvelope = read_private_json(&response_path)?;
                    let bytes = self.verify_envelope(
                        &response,
                        Some("response"),
                        Some(name),
                        Some(&envelope_id),
                        false,
                    )?;
                    let body: ResponsePayload = serde_json::from_slice(&bytes).map_err(|_| {
                        anyhow::anyhow!(
                            "Installed app returned an invalid live diagnostic response."
                        )
                    })?;
                    if body.request_id != envelope_id || body.command != name {
                        anyhow::bail!(
                            "Installed app returned a mismatched live diagnostic response."
                        );
                    }
                    if !body.ok {
                        anyhow::bail!("Installed app rejected the live diagnostic command.");
                    }
                    return Ok(body);
                }
                thread::sleep(Duration::from_millis(50));
            }
            anyhow::bail!(
                "Installed Whisply did not answer the live diagnostic command before its deadline."
            );
        }

        fn next_sequence(&self) -> anyhow::Result<u64> {
            let lock_path = self.directory.join(".controller.lock");
            validate_private_file(&lock_path)?;
            let lock = OpenOptions::new().read(true).write(true).open(&lock_path)?;
            if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
                anyhow::bail!("Unable to lock the owner-only live diagnostic session.");
            }
            let result = (|| {
                let state_path = self.directory.join("controller-state.json");
                let mut state: ControllerState = read_private_json(&state_path)?;
                if state.protocol != LIVE_PROTOCOL
                    || state.session_id != self.id
                    || state.next_command_sequence == 0
                {
                    anyhow::bail!("The owner-only live diagnostic command sequence is invalid.");
                }
                let sequence = state.next_command_sequence;
                state.next_command_sequence = sequence.checked_add(1).ok_or_else(|| {
                    anyhow::anyhow!("The owner-only live diagnostic command sequence is invalid.")
                })?;
                write_replace_json(&state_path, &state)?;
                Ok(sequence)
            })();
            let _ = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_UN) };
            result
        }

        fn sign_envelope(
            &self,
            envelope_id: String,
            sequence: u64,
            kind: &str,
            name: &str,
            payload: Vec<u8>,
            issued_at_ms: i64,
            expires_at_ms: i64,
        ) -> anyhow::Result<SignedEnvelope> {
            if expires_at_ms <= issued_at_ms
                || expires_at_ms > self.expires_at_ms
                || expires_at_ms.saturating_sub(issued_at_ms) > MAX_COMMAND_LIFETIME_MS
            {
                anyhow::bail!("Installed live diagnostic command lifetime is invalid.");
            }
            let mut envelope = SignedEnvelope {
                protocol_name: LIVE_PROTOCOL.to_string(),
                session_id: self.id.clone(),
                envelope_id,
                sequence,
                issued_at_ms,
                expires_at_ms,
                kind: kind.to_string(),
                name: name.to_string(),
                payload_base64: STANDARD.encode(payload),
                hmac_sha256: String::new(),
            };
            envelope.hmac_sha256 = self.envelope_mac(&envelope)?;
            Ok(envelope)
        }

        fn verify_envelope(
            &self,
            envelope: &SignedEnvelope,
            expected_kind: Option<&str>,
            expected_name: Option<&str>,
            expected_id: Option<&str>,
            allow_expired: bool,
        ) -> anyhow::Result<Vec<u8>> {
            let now_ms = unix_ms()?;
            if envelope.protocol_name != LIVE_PROTOCOL
                || envelope.session_id != self.id
                || canonical_uuid(&envelope.envelope_id)? != envelope.envelope_id
                || envelope.sequence == 0
                || envelope.issued_at_ms > now_ms.saturating_add(30_000)
                || envelope.expires_at_ms <= envelope.issued_at_ms
                || envelope.expires_at_ms > self.expires_at_ms
                || envelope.expires_at_ms.saturating_sub(envelope.issued_at_ms)
                    > MAX_COMMAND_LIFETIME_MS
                || (!allow_expired && envelope.expires_at_ms <= now_ms)
                || expected_kind.is_some_and(|value| envelope.kind != value)
                || expected_name.is_some_and(|value| envelope.name != value)
                || expected_id.is_some_and(|value| envelope.envelope_id != value)
            {
                anyhow::bail!("The signed installed-app diagnostic envelope is invalid.");
            }
            let payload = STANDARD
                .decode(envelope.payload_base64.as_bytes())
                .map_err(|_| {
                    anyhow::anyhow!("The signed installed-app diagnostic envelope is invalid.")
                })?;
            if payload.len() > MAX_COMMAND_PAYLOAD_BYTES {
                anyhow::bail!("The signed installed-app diagnostic envelope is invalid.");
            }
            let supplied = decode_hex(&envelope.hmac_sha256).ok_or_else(|| {
                anyhow::anyhow!("The signed installed-app diagnostic envelope is invalid.")
            })?;
            if supplied.len() != 32 {
                anyhow::bail!("The signed installed-app diagnostic envelope is invalid.");
            }
            let mut verifier =
                HmacSha256::new_from_slice(self.secret.as_slice()).map_err(|_| {
                    anyhow::anyhow!("The owner-only live diagnostic session is invalid.")
                })?;
            verifier.update(&envelope_signing_material(envelope));
            verifier.verify_slice(&supplied).map_err(|_| {
                anyhow::anyhow!("The signed installed-app diagnostic envelope is invalid.")
            })?;
            Ok(payload)
        }

        fn envelope_mac(&self, envelope: &SignedEnvelope) -> anyhow::Result<String> {
            let mut mac = HmacSha256::new_from_slice(self.secret.as_slice()).map_err(|_| {
                anyhow::anyhow!("The owner-only live diagnostic session is invalid.")
            })?;
            mac.update(&envelope_signing_material(envelope));
            Ok(hex_encode(&mac.finalize().into_bytes()))
        }

        fn wait_for_run(&self, run_id: &str, timeout_secs: u32) -> anyhow::Result<Value> {
            let run_id = canonical_uuid(run_id)?;
            if !(1..=3_600).contains(&timeout_secs) {
                anyhow::bail!("Live diagnostic wait timeout must be between 1 and 3600 seconds.");
            }
            let run = self.run_directory(&run_id)?;
            let deadline = std::time::Instant::now() + Duration::from_secs(u64::from(timeout_secs));
            while std::time::Instant::now() < deadline {
                if run.join("result.json").is_file() && run.join("hashes.sha256").is_file() {
                    return self.assert_run(&run_id);
                }
                thread::sleep(Duration::from_millis(100));
            }
            let events = self.event_summaries(&run_id, 0, 1)?;
            Ok(serde_json::json!({
                "ok": false,
                "timedOut": true,
                "runID": run_id,
                "lastEvent": events.into_iter().last(),
            }))
        }

        fn assert_run(&self, run_id: &str) -> anyhow::Result<Value> {
            let run_id = canonical_uuid(run_id)?;
            let run = self.run_directory(&run_id)?;
            let required = [
                "run.json",
                "result.json",
                "authority.json",
                "events.jsonl",
                "structured-log.jsonl",
                "hashes.sha256",
            ];
            for name in required {
                validate_private_file(&run.join(name))?;
            }
            let manifest: Value = read_private_json(&run.join("run.json"))?;
            let result: Value = read_private_json(&run.join("result.json"))?;
            let authority: Value = read_private_json(&run.join("authority.json"))?;
            let captures_artifacts = manifest
                .pointer("/request/captureArtifacts")
                .and_then(Value::as_bool)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "Installed live diagnostic evidence has an invalid capture policy."
                    )
                })?;
            validate_run_metadata(
                &manifest,
                &run_id,
                &self.release_manifest_sha256,
                "manifest",
            )?;
            validate_run_metadata(&result, &run_id, &self.release_manifest_sha256, "result")?;
            if result.get("commercialAuthority").and_then(Value::as_str)
                != Some("normal-installed-product-gates")
            {
                anyhow::bail!(
                    "Installed live diagnostic evidence does not prove normal product authority."
                );
            }
            let boundary = authority
                .get("commercialBoundary")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    anyhow::anyhow!("Installed live diagnostic authority evidence is invalid.")
                })?;
            if !boundary.contains("grants no session") || !boundary.contains("provider") {
                anyhow::bail!(
                    "Installed live diagnostic evidence does not prove the no-bypass boundary."
                );
            }
            if captures_artifacts {
                for name in [
                    "animation.json",
                    "accessibility/started.json",
                    "accessibility/finished.json",
                    "icons/started.json",
                    "icons/finished.json",
                    "layout/started-windows.json",
                    "layout/finished-windows.json",
                    "layout/started.json",
                    "layout/finished.json",
                    "screenshots/started.png",
                    "screenshots/finished.png",
                    "snapshots/started.json",
                    "snapshots/finished.json",
                    "visible-copy/started.json",
                    "visible-copy/finished.json",
                ] {
                    validate_private_file(&run.join(name))?;
                }
            }
            let event_summaries = self.event_summaries(&run_id, 0, MAX_EVENT_LINES)?;
            if event_summaries
                .first()
                .and_then(|event| event.get("name"))
                .and_then(Value::as_str)
                != Some("run.started")
                || !event_summaries
                    .iter()
                    .any(|event| event.get("name").and_then(Value::as_str) == Some("run.finished"))
            {
                anyhow::bail!(
                    "Installed live diagnostic evidence is missing complete start/finish events."
                );
            }
            let hashes = verify_hash_manifest(&run)?;
            Ok(serde_json::json!({
                "ok": true,
                "protocol": LIVE_PROTOCOL,
                "sessionID": self.id,
                "runID": run_id,
                "releaseManifestSha256": self.release_manifest_sha256,
                "eventCount": event_summaries.len(),
                "evidence": hashes,
                "result": result.get("result").cloned().unwrap_or(Value::Null),
                "visibleState": result.get("visibleState").cloned().unwrap_or(Value::Null),
            }))
        }

        fn event_summaries(
            &self,
            run_id: &str,
            since: u64,
            limit: usize,
        ) -> anyhow::Result<Vec<Value>> {
            let summaries = self
                .verified_run_events(run_id)?
                .into_iter()
                .map(|event| {
                    serde_json::json!({
                        "sequence": event.sequence,
                        "name": event.name,
                        "timestampMs": event.timestamp_ms,
                    })
                })
                .collect();
            Ok(select_tail_summaries(summaries, since, limit))
        }

        /// This view is intentionally local-only: it authenticates retained
        /// event envelopes, then drops every event field outside the fixed
        /// token-class allowlist below. It cannot launch or message the app.
        fn token_class_inspection(
            &self,
            run_id: &str,
            since: u64,
            limit: usize,
        ) -> anyhow::Result<Value> {
            let run_id = canonical_uuid(run_id)?;
            token_class_inspection_report(
                &run_id,
                &self.verified_provider_usage_events(&run_id)?,
                since,
                limit,
            )
        }

        /// Provider cost values remain unaggregated. The local evidence has
        /// no safe basis for currency conversion, settlement, or billing
        /// inference, so this only exposes each signed provider report.
        fn cost_inspection(&self, run_id: &str, since: u64, limit: usize) -> anyhow::Result<Value> {
            let run_id = canonical_uuid(run_id)?;
            cost_inspection_report(
                &run_id,
                &self.verified_provider_usage_events(&run_id)?,
                since,
                limit,
            )
        }

        /// This completed-run view uses only retained, signed local evidence.
        /// It validates the complete bundle before it reads the manifest and
        /// final authority snapshot, then projects fixed policy fields while
        /// dropping prompts, targets, identifiers, errors, and raw authority
        /// values. It cannot launch or message the installed app.
        fn policy_inspection(
            &self,
            run_id: &str,
            since: u64,
            limit: usize,
        ) -> anyhow::Result<Value> {
            let run_id = canonical_uuid(run_id)?;
            let _ = self.assert_run(&run_id)?;
            let run = self.run_directory(&run_id)?;
            let manifest: Value = read_private_json(&run.join("run.json"))?;
            let authority: Value = read_private_json(&run.join("authority.json"))?;
            validate_run_metadata(
                &manifest,
                &run_id,
                &self.release_manifest_sha256,
                "manifest",
            )?;
            let policy_input = policy_input_projection(&manifest)?;
            let authority = authority_projection(&authority)?;
            let policy_results =
                policy_result_inspection(&self.verified_run_events(&run_id)?, since, limit)?;
            // Recheck the manifest after all of the local reads above so the
            // returned projection remains bound to the exact completed bundle.
            verify_hash_manifest(&run)?;
            Ok(serde_json::json!({
                "ok": true,
                "protocol": LIVE_PROTOCOL,
                "runID": run_id,
                "policyInput": policy_input,
                "authority": authority,
                "since": since,
                "policyResultEventCount": policy_results.event_count,
                "returnedResultCount": policy_results.records.len(),
                "truncated": policy_results.truncated,
                "nextSince": policy_results.next_since,
                "policyResults": policy_results.records,
                "commercialBoundary": "read-only signed completed-run evidence; fixed redacted policy and authority projections only; no installed-app launch, command, provider request, account mutation, confirmation, Usage, billing, or authority grant",
            }))
        }

        /// This completed-run view only opens the optional task-usage artifact
        /// after full bundle verification. The Swift host has already made the
        /// authenticated task-detail request while finalizing the normal run;
        /// this route only projects its retained hash-bound output and cannot
        /// create a reservation, settle Usage, or contact a provider.
        fn usage_receipt_inspection(&self, run_id: &str, limit: usize) -> anyhow::Result<Value> {
            validate_usage_receipt_inspection_limit(limit)?;
            let run_id = canonical_uuid(run_id)?;
            let _ = self.assert_run(&run_id)?;
            let run = self.run_directory(&run_id)?;
            let artifact_path = run.join("task-usage.json");
            let usage = match fs::symlink_metadata(&artifact_path) {
                Ok(_) => {
                    validate_private_file(&artifact_path)?;
                    let artifact: Value = read_private_json(&artifact_path)?;
                    usage_receipt_projection(&artifact, limit)?
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    no_usage_receipt_projection(limit)?
                }
                Err(_) => anyhow::bail!("Installed live diagnostic Usage evidence is unavailable."),
            };
            // Keep the returned projection bound to the exact completed bundle
            // after the optional artifact read above.
            verify_hash_manifest(&run)?;
            Ok(serde_json::json!({
                "ok": true,
                "protocol": LIVE_PROTOCOL,
                "runID": run_id,
                "usage": usage,
                "commercialBoundary": "read-only signed completed-run Usage evidence; fixed reservation, settlement, and receipt projections only; no installed-app launch, command, provider request, account mutation, reservation, settlement, billing, or authority grant",
            }))
        }

        fn verified_provider_usage_events(
            &self,
            run_id: &str,
        ) -> anyhow::Result<Vec<VerifiedProviderUsage>> {
            self.verified_run_events(run_id)?
                .iter()
                .filter(|event| event.name == "provider.usage")
                .map(VerifiedProviderUsage::from_event)
                .collect()
        }

        fn verified_run_events(&self, run_id: &str) -> anyhow::Result<Vec<VerifiedRunEvent>> {
            let run_id = canonical_uuid(run_id)?;
            let run = self.run_directory(&run_id)?;
            let path = run.join("events.jsonl");
            validate_private_file(&path)?;
            let bytes = read_private_bytes(&path, MAX_EVIDENCE_BYTES as usize)?;
            let mut events = Vec::new();
            let mut event_count = 0_usize;
            let mut last_sequence = 0_u64;
            for line in bytes
                .split(|byte| *byte == b'\n')
                .filter(|line| !line.is_empty())
            {
                event_count += 1;
                if event_count > MAX_EVENT_LINES {
                    anyhow::bail!("Installed live diagnostic event evidence is too large.");
                }
                let envelope: SignedEnvelope = serde_json::from_slice(line).map_err(|_| {
                    anyhow::anyhow!("Installed live diagnostic event evidence is invalid.")
                })?;
                if envelope.sequence <= last_sequence {
                    anyhow::bail!("Installed live diagnostic event evidence is invalid.");
                }
                last_sequence = envelope.sequence;
                let payload =
                    self.verify_envelope(&envelope, Some("run-event"), None, None, true)?;
                let event: Value = serde_json::from_slice(&payload).map_err(|_| {
                    anyhow::anyhow!("Installed live diagnostic event evidence is invalid.")
                })?;
                if event.get("runID").and_then(Value::as_str) != Some(run_id.as_str())
                    || event
                        .get("hiddenReasoningCaptured")
                        .and_then(Value::as_bool)
                        != Some(false)
                {
                    anyhow::bail!("Installed live diagnostic event evidence is invalid.");
                }
                let sequence = event
                    .get("sequence")
                    .and_then(Value::as_u64)
                    .filter(|sequence| *sequence == envelope.sequence)
                    .ok_or_else(|| {
                        anyhow::anyhow!("Installed live diagnostic event evidence is invalid.")
                    })?;
                let timestamp_ms = event
                    .get("timestampMs")
                    .and_then(Value::as_i64)
                    .filter(|timestamp_ms| *timestamp_ms >= 0)
                    .ok_or_else(|| {
                        anyhow::anyhow!("Installed live diagnostic event evidence is invalid.")
                    })?;
                let name = event
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|name| {
                        *name == envelope.name.as_str() && !name.is_empty() && name.len() <= 256
                    })
                    .ok_or_else(|| {
                        anyhow::anyhow!("Installed live diagnostic event evidence is invalid.")
                    })?;
                events.push(VerifiedRunEvent {
                    sequence,
                    timestamp_ms,
                    name: name.to_string(),
                    fields: event.get("fields").cloned().unwrap_or(Value::Null),
                });
            }
            Ok(events)
        }

        fn run_directory(&self, run_id: &str) -> anyhow::Result<PathBuf> {
            let run_id = canonical_uuid(run_id)?;
            let runs = self.directory.join("runs");
            validate_private_directory(&runs)?;
            let run = runs.join(&run_id);
            if run.parent() != Some(runs.as_path()) {
                anyhow::bail!("Installed live diagnostic run escaped its owner-only session.");
            }
            validate_private_directory(&run)?;
            Ok(run)
        }
    }

    impl VerifiedProviderUsage {
        fn from_event(event: &VerifiedRunEvent) -> anyhow::Result<Self> {
            let fields = event.fields.as_object().ok_or_else(|| {
                anyhow::anyhow!("Installed live diagnostic provider-usage evidence is invalid.")
            })?;
            let mut token_classes = Vec::new();
            for key in PROVIDER_USAGE_TOKEN_CLASSES {
                if let Some(value) = optional_provider_usage_token(fields, key)? {
                    token_classes.push((key, value));
                }
            }
            Ok(Self {
                sequence: event.sequence,
                timestamp_ms: event.timestamp_ms,
                token_classes,
                provider_reported_cost: optional_provider_reported_cost(fields)?,
                cost_details_present: fields
                    .get("costDetails")
                    .is_some_and(|value| !value.is_null()),
            })
        }
    }

    fn optional_provider_usage_token(
        fields: &Map<String, Value>,
        key: &str,
    ) -> anyhow::Result<Option<u64>> {
        let Some(value) = fields.get(key) else {
            return Ok(None);
        };
        let count = value
            .as_u64()
            .filter(|count| *count <= MAX_PROVIDER_USAGE_TOKENS)
            .ok_or_else(|| {
                anyhow::anyhow!("Installed live diagnostic provider-usage evidence is invalid.")
            })?;
        Ok(Some(count))
    }

    fn optional_provider_reported_cost(fields: &Map<String, Value>) -> anyhow::Result<Option<f64>> {
        let Some(value) = fields.get("cost") else {
            return Ok(None);
        };
        let cost = value
            .as_f64()
            .filter(|cost| cost.is_finite() && (0.0..=MAX_PROVIDER_REPORTED_COST).contains(cost))
            .ok_or_else(|| {
                anyhow::anyhow!("Installed live diagnostic provider-usage evidence is invalid.")
            })?;
        Ok(Some(cost))
    }

    fn validate_usage_inspection_limit(limit: usize) -> anyhow::Result<()> {
        if !(1..=MAX_USAGE_INSPECTION_RECORDS).contains(&limit) {
            anyhow::bail!("Installed live diagnostic token/cost inspection limit is invalid.");
        }
        Ok(())
    }

    fn usage_inspection_boundary() -> &'static str {
        "read-only signed owner-only run evidence; unaggregated provider reports only; no installed-app launch, command, provider request, account mutation, settlement, billing, or currency inference"
    }

    fn token_class_inspection_report(
        run_id: &str,
        events: &[VerifiedProviderUsage],
        since: u64,
        limit: usize,
    ) -> anyhow::Result<Value> {
        validate_usage_inspection_limit(limit)?;
        let matching: Vec<_> = events
            .iter()
            .filter(|event| event.sequence > since && !event.token_classes.is_empty())
            .collect();
        let returned: Vec<_> = matching.iter().take(limit).collect();
        let next_since = returned
            .last()
            .map(|event| Value::from(event.sequence))
            .unwrap_or(Value::Null);
        let records = returned
            .iter()
            .map(|event| {
                let token_classes = event
                    .token_classes
                    .iter()
                    .map(|(name, count)| ((*name).to_string(), Value::from(*count)))
                    .collect();
                serde_json::json!({
                    "sequence": event.sequence,
                    "timestampMs": event.timestamp_ms,
                    "tokenClasses": Value::Object(token_classes),
                })
            })
            .collect::<Vec<_>>();
        Ok(serde_json::json!({
            "ok": true,
            "protocol": LIVE_PROTOCOL,
            "runID": run_id,
            "since": since,
            "knownTokenClasses": PROVIDER_USAGE_TOKEN_CLASSES,
            "tokenClassEventCount": matching.len(),
            "returnedRecordCount": records.len(),
            "truncated": matching.len() > records.len(),
            "nextSince": next_since,
            "records": records,
            "commercialBoundary": usage_inspection_boundary(),
        }))
    }

    fn cost_inspection_report(
        run_id: &str,
        events: &[VerifiedProviderUsage],
        since: u64,
        limit: usize,
    ) -> anyhow::Result<Value> {
        validate_usage_inspection_limit(limit)?;
        let matching: Vec<_> = events
            .iter()
            .filter(|event| event.sequence > since)
            .collect();
        let returned: Vec<_> = matching.iter().take(limit).collect();
        let next_since = returned
            .last()
            .map(|event| Value::from(event.sequence))
            .unwrap_or(Value::Null);
        let records = returned
            .iter()
            .map(|event| {
                let provider_reported_cost = event
                    .provider_reported_cost
                    .and_then(serde_json::Number::from_f64)
                    .map(Value::Number)
                    .unwrap_or(Value::Null);
                serde_json::json!({
                    "sequence": event.sequence,
                    "timestampMs": event.timestamp_ms,
                    "providerReportedCost": provider_reported_cost,
                    "costDetailsPresent": event.cost_details_present,
                })
            })
            .collect::<Vec<_>>();
        let reported_cost_event_count = matching
            .iter()
            .filter(|event| event.provider_reported_cost.is_some())
            .count();
        Ok(serde_json::json!({
            "ok": true,
            "protocol": LIVE_PROTOCOL,
            "runID": run_id,
            "since": since,
            "providerUsageEventCount": matching.len(),
            "reportedCostEventCount": reported_cost_event_count,
            "returnedRecordCount": records.len(),
            "truncated": matching.len() > records.len(),
            "nextSince": next_since,
            "records": records,
            "commercialBoundary": usage_inspection_boundary(),
        }))
    }

    fn policy_input_projection(manifest: &Value) -> anyhow::Result<Value> {
        let request = policy_object(
            manifest
                .get("request")
                .ok_or_else(policy_evidence_invalid)?,
        )?;
        let route =
            policy_required_allowed_string(request, "route", &["normal", "native-computer-use"])?;
        let selected_source_count = policy_required_u64_at_most(request, "selectedSourceCount", 3)?;
        let selected_sources = request
            .get("selectedSources")
            .and_then(Value::as_array)
            .ok_or_else(policy_evidence_invalid)?;
        if selected_sources.len() != usize::try_from(selected_source_count).unwrap_or(usize::MAX)
            || selected_sources.iter().any(|source| !source.is_object())
        {
            anyhow::bail!("Installed live diagnostic policy evidence is invalid.");
        }
        let selected_plugin_id = match request.get("selectedPlugin") {
            Some(Value::Null) => Value::Null,
            Some(value) => {
                let plugin = policy_object(value)?;
                Value::String(
                    policy_required_allowed_string(
                        plugin,
                        "id",
                        &[
                            "browser",
                            "chrome",
                            "computer_use",
                            "documents",
                            "pdf",
                            "spreadsheets",
                            "presentations",
                        ],
                    )?
                    .to_string(),
                )
            }
            None => anyhow::bail!("Installed live diagnostic policy evidence is invalid."),
        };
        let computer_use_efficiency_cohort = policy_optional_allowed_string(
            request,
            "computerUseEfficiencyCohort",
            &["semantic", "visual-loop"],
        )?
        .map(|value| Value::String(value.to_string()))
        .unwrap_or(Value::Null);
        let requested_model = match request.get("requestedModelID") {
            None => false,
            Some(value) => {
                policy_bounded_opaque_string(value, 256)?;
                true
            }
        };
        policy_bounded_opaque_string(
            request
                .get("resolvedModelID")
                .ok_or_else(policy_evidence_invalid)?,
            256,
        )?;
        Ok(serde_json::json!({
            "route": route,
            "usesScreen": policy_required_bool(request, "usesScreen")?,
            "computerUse": policy_required_bool(request, "computerUse")?,
            "newChat": policy_required_bool(request, "newChat")?,
            "captureArtifacts": policy_required_bool(request, "captureArtifacts")?,
            "requestedModel": requested_model,
            "selectedSourceCount": selected_source_count,
            "selectedPluginID": selected_plugin_id,
            "computerUseEfficiencyCohort": computer_use_efficiency_cohort,
        }))
    }

    fn authority_projection(authority: &Value) -> anyhow::Result<Value> {
        let authority = policy_object(authority)?;
        let commercial_boundary = authority
            .get("commercialBoundary")
            .and_then(Value::as_str)
            .filter(|value| value.contains("grants no session") && value.contains("provider"))
            .ok_or_else(policy_evidence_invalid)?;
        if commercial_boundary.len() > 1_024 {
            anyhow::bail!("Installed live diagnostic policy evidence is invalid.");
        }
        let computer_use = policy_object(
            authority
                .get("computerUse")
                .ok_or_else(policy_evidence_invalid)?,
        )?;
        let capabilities = policy_object(
            computer_use
                .get("capabilities")
                .ok_or_else(policy_evidence_invalid)?,
        )?;
        let authority_current = policy_required_bool(authority, "authorityCurrent")?;
        let account_boundary_changed = policy_optional_bool(authority, "accountBoundaryChanged")?
            .unwrap_or(!authority_current);
        Ok(serde_json::json!({
            "signedIn": policy_required_bool(authority, "signedIn")?,
            "authorityCurrent": authority_current,
            "accountBoundaryChanged": account_boundary_changed,
            "computerUse": {
                "snapshotCurrent": policy_required_bool(computer_use, "snapshotCurrent")?,
                "masterEnabled": policy_required_bool(computer_use, "masterEnabled")?,
                "serverPolicyUsable": policy_required_bool(computer_use, "serverPolicyUsable")?,
                "nativeAppsEnabled": policy_required_bool(computer_use, "nativeAppsEnabled")?,
                "nativeAppsEffective": policy_required_bool(computer_use, "nativeAppsEffective")?,
                "helperReady": policy_required_bool(computer_use, "helperReady")?,
                "capabilities": {
                    "computerUse": policy_required_bool(capabilities, "computerUse")?,
                    "nativeApps": policy_required_bool(capabilities, "nativeApps")?,
                    "existingBrowser": policy_required_bool(capabilities, "existingBrowser")?,
                    "spawnedBrowser": policy_required_bool(capabilities, "spawnedBrowser")?,
                    "localFiles": policy_required_bool(capabilities, "localFiles")?,
                },
            },
        }))
    }

    fn policy_result_inspection(
        events: &[VerifiedRunEvent],
        since: u64,
        limit: usize,
    ) -> anyhow::Result<PolicyResultInspection> {
        validate_policy_inspection_limit(limit)?;
        let mut records = Vec::new();
        for event in events {
            if let Some(record) = policy_event_projection(event)? {
                if record.sequence > since {
                    records.push(record);
                }
            }
        }
        let event_count = records.len();
        let truncated = event_count > limit;
        records.truncate(limit);
        let next_since = records
            .last()
            .map(|record| Value::from(record.sequence))
            .unwrap_or(Value::Null);
        Ok(PolicyResultInspection {
            event_count,
            records: records.into_iter().map(|record| record.record).collect(),
            truncated,
            next_since,
        })
    }

    fn policy_event_projection(
        event: &VerifiedRunEvent,
    ) -> anyhow::Result<Option<VerifiedPolicyResult>> {
        if !matches!(
            event.name.as_str(),
            "request.session-authority"
                | "computer-use.admission"
                | "computer-use.application-approval.started"
                | "computer-use.application-policy.denied"
                | "computer-use.policy-target-resolution"
                | "computer-use.tool.finished"
                | "computer-use.runtime-authority.failed"
        ) {
            return Ok(None);
        }
        let fields = policy_object(&event.fields)?;
        let record = match event.name.as_str() {
            "request.session-authority" => {
                let admitted = policy_required_bool(fields, "admitted")?;
                let durable = policy_optional_bool(fields, "durable")?;
                if admitted && durable.is_none() {
                    anyhow::bail!("Installed live diagnostic policy evidence is invalid.");
                }
                Some(serde_json::json!({
                    "kind": "session-authority",
                    "admitted": admitted,
                    "durable": durable,
                }))
            }
            "computer-use.admission" => Some(serde_json::json!({
                "kind": "computer-use-admission",
                "route": policy_required_allowed_string(
                    fields,
                    "route",
                    &["native-route-lock", "resume", "automatic-or-explicit"],
                )?,
                "requestAllowsComputerUse": policy_required_bool(fields, "requestAllowsComputerUse")?,
                "effectivePolicyEnabled": policy_required_bool(fields, "effectivePolicyEnabled")?,
                "masterEnabled": policy_required_bool(fields, "masterEnabled")?,
                "nativeAppsEffective": policy_required_bool(fields, "nativeAppsEffective")?,
                "serverPolicyUsable": policy_required_bool(fields, "serverPolicyUsable")?,
                "helperReady": policy_required_bool(fields, "helperReady")?,
                "runtimeDisableLatch": policy_required_bool(fields, "runtimeDisableLatch")?,
                "tierEligible": policy_required_bool(fields, "tierEligible")?,
                "admitted": policy_required_bool(fields, "admitted")?,
            })),
            "computer-use.application-approval.started" => Some(serde_json::json!({
                "kind": "application-approval-started",
                "finalConfirmationRequired": policy_required_bool(fields, "finalConfirmationRequired")?,
            })),
            "computer-use.application-policy.denied" => Some(serde_json::json!({
                "kind": "application-policy-denied",
                "denial": policy_application_denial_category(
                    fields.get("decision").ok_or_else(policy_evidence_invalid)?,
                )?,
            })),
            "computer-use.policy-target-resolution" => {
                let attempt =
                    policy_required_u64_at_most(fields, "attempt", MAX_POLICY_RETRY_COUNT + 1)?;
                let maximum_retries =
                    policy_required_u64_at_most(fields, "maximumRetries", MAX_POLICY_RETRY_COUNT)?;
                if attempt == 0 || attempt > maximum_retries.saturating_add(1) {
                    anyhow::bail!("Installed live diagnostic policy evidence is invalid.");
                }
                Some(serde_json::json!({
                    "kind": "policy-target-resolution",
                    "attempt": attempt,
                    "maximumRetries": maximum_retries,
                    "reason": policy_reason_category(
                        fields.get("reasonCode").ok_or_else(policy_evidence_invalid)?,
                    )?,
                    "willRetry": policy_required_bool(fields, "willRetry")?,
                }))
            }
            "computer-use.tool.finished" => match (
                fields.get("policyDisposition"),
                fields.get("policyReasonCode"),
            ) {
                (None, None) => None,
                (Some(disposition), Some(reason)) => Some(serde_json::json!({
                    "kind": "computer-use-tool-policy-boundary",
                    "succeeded": policy_required_bool(fields, "succeeded")?,
                    "verified": policy_required_bool(fields, "verified")?,
                    "disposition": policy_disposition(disposition)?,
                    "reason": policy_reason_category(reason)?,
                })),
                _ => anyhow::bail!("Installed live diagnostic policy evidence is invalid."),
            },
            "computer-use.runtime-authority.failed" => Some(serde_json::json!({
                "kind": "runtime-authority-failed",
            })),
            _ => None,
        };
        Ok(record.map(|record| VerifiedPolicyResult {
            sequence: event.sequence,
            record: serde_json::json!({
                "sequence": event.sequence,
                "timestampMs": event.timestamp_ms,
                "result": record,
            }),
        }))
    }

    fn validate_policy_inspection_limit(limit: usize) -> anyhow::Result<()> {
        if !(1..=MAX_POLICY_INSPECTION_RECORDS).contains(&limit) {
            anyhow::bail!("Installed live diagnostic policy inspection limit is invalid.");
        }
        Ok(())
    }

    fn policy_object(value: &Value) -> anyhow::Result<&Map<String, Value>> {
        value.as_object().ok_or_else(policy_evidence_invalid)
    }

    fn policy_required_bool(fields: &Map<String, Value>, key: &str) -> anyhow::Result<bool> {
        fields
            .get(key)
            .and_then(Value::as_bool)
            .ok_or_else(policy_evidence_invalid)
    }

    fn policy_optional_bool(
        fields: &Map<String, Value>,
        key: &str,
    ) -> anyhow::Result<Option<bool>> {
        match fields.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(value) => value
                .as_bool()
                .map(Some)
                .ok_or_else(policy_evidence_invalid),
        }
    }

    fn policy_required_u64_at_most(
        fields: &Map<String, Value>,
        key: &str,
        maximum: u64,
    ) -> anyhow::Result<u64> {
        fields
            .get(key)
            .and_then(Value::as_u64)
            .filter(|value| *value <= maximum)
            .ok_or_else(policy_evidence_invalid)
    }

    fn policy_required_allowed_string<'a>(
        fields: &'a Map<String, Value>,
        key: &str,
        allowed: &[&str],
    ) -> anyhow::Result<&'a str> {
        let value = fields
            .get(key)
            .and_then(Value::as_str)
            .ok_or_else(policy_evidence_invalid)?;
        if allowed.contains(&value) {
            Ok(value)
        } else {
            anyhow::bail!("Installed live diagnostic policy evidence is invalid.");
        }
    }

    fn policy_optional_allowed_string<'a>(
        fields: &'a Map<String, Value>,
        key: &str,
        allowed: &[&str],
    ) -> anyhow::Result<Option<&'a str>> {
        match fields.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(_) => policy_required_allowed_string(fields, key, allowed).map(Some),
        }
    }

    fn policy_bounded_opaque_string(value: &Value, maximum: usize) -> anyhow::Result<()> {
        let value = value.as_str().ok_or_else(policy_evidence_invalid)?;
        if value.is_empty() || value.len() > maximum || value.chars().any(char::is_control) {
            anyhow::bail!("Installed live diagnostic policy evidence is invalid.");
        }
        Ok(())
    }

    fn policy_application_denial_category(value: &Value) -> anyhow::Result<&'static str> {
        policy_bounded_opaque_string(value, 64)?;
        Ok(if value.as_str() == Some("forbidden") {
            "forbidden"
        } else {
            "not-allowed"
        })
    }

    fn policy_disposition(value: &Value) -> anyhow::Result<&'static str> {
        match value.as_str() {
            Some("allow") => Ok("allow"),
            Some("final_confirmation_required") => Ok("final-confirmation-required"),
            Some("material_clarification_required") => Ok("material-clarification-required"),
            Some("unavailable") => Ok("unavailable"),
            _ => anyhow::bail!("Installed live diagnostic policy evidence is invalid."),
        }
    }

    fn policy_reason_category(value: &Value) -> anyhow::Result<&'static str> {
        policy_bounded_opaque_string(value, 128)?;
        Ok(match value.as_str() {
            Some("ACCESSIBILITY_TARGET_UNAVAILABLE") => "accessibility-target-unavailable",
            Some("ACTION_UNAVAILABLE") => "action-unavailable",
            Some("AMBIGUOUS_STATE_CHANGING_CONTROL") => "ambiguous-state-changing-control",
            Some("AUTOMATIC_ACTION") => "automatic-action",
            Some("CONFIRMATION_STATE_CHANGED") => "confirmation-state-changed",
            Some("DECLARED_ACTION_MISMATCH") => "declared-action-mismatch",
            Some("EMPTY_TRASH_UNAVAILABLE") => "empty-trash-unavailable",
            Some("FINAL_CONFIRMATION_REQUIRED") => "final-confirmation-required",
            Some("MULTILINE_TEXT_NEWLINE") => "multiline-text-newline",
            Some("NATIVE_POLICY_CONTEXT_UNAVAILABLE") => "native-policy-context-unavailable",
            Some("NATIVE_POLICY_SESSION_AUTHORITY_MISMATCH") => {
                "native-policy-session-authority-mismatch"
            }
            Some("PERMANENT_DELETE_UNAVAILABLE") => "permanent-delete-unavailable",
            Some("SECURE_FIELD_INPUT_UNAVAILABLE") => "secure-field-input-unavailable",
            Some("TYPED_NEWLINE_MAY_COMMIT") => "typed-newline-may-commit",
            Some("UNCLASSIFIED_COMMIT_KEY_CHORD") => "unclassified-commit-key-chord",
            Some("UNCLASSIFIED_ENTER_KEY") => "unclassified-enter-key",
            _ => "other",
        })
    }

    fn policy_evidence_invalid() -> anyhow::Error {
        anyhow::anyhow!("Installed live diagnostic policy evidence is invalid.")
    }

    fn no_usage_receipt_projection(limit: usize) -> anyhow::Result<Value> {
        validate_usage_receipt_inspection_limit(limit)?;
        Ok(serde_json::json!({
            "artifactPresent": false,
            "artifactStatus": "not-captured",
            "taskCount": 0,
            "unavailableTaskCount": 0,
            "returnedTaskCount": 0,
            "truncated": false,
            "tasks": [],
        }))
    }

    fn usage_receipt_projection(artifact: &Value, limit: usize) -> anyhow::Result<Value> {
        validate_usage_receipt_inspection_limit(limit)?;
        let artifact = usage_object(artifact)?;
        if usage_required_allowed_string(artifact, "scope", &["contextual-task"])?
            != "contextual-task"
            || usage_required_allowed_string(
                artifact,
                "source",
                &["authenticated-runtime-task-detail"],
            )? != "authenticated-runtime-task-detail"
            || usage_required_allowed_string(
                artifact,
                "privacyProjection",
                &[
                    "task identity, state, reservation, components, lifecycle, and truncation flags only",
                ],
            )? != "task identity, state, reservation, components, lifecycle, and truncation flags only"
        {
            return Err(usage_evidence_invalid());
        }
        let status = usage_required_allowed_string(
            artifact,
            "status",
            &["complete", "partial", "unavailable"],
        )?
        .to_string();
        let _ = usage_required_nonnegative_i64(artifact, "generatedAtMs")?;
        let tasks = usage_required_array(artifact, "tasks")?;
        let unavailable = usage_required_array(artifact, "unavailable")?;
        if tasks.len() > MAX_USAGE_RECEIPT_TASKS || unavailable.len() > MAX_USAGE_RECEIPT_TASKS {
            return Err(usage_evidence_invalid());
        }
        for unavailable_task in unavailable {
            validate_unavailable_usage_task(unavailable_task)?;
        }
        match status.as_str() {
            "complete" if !unavailable.is_empty() => return Err(usage_evidence_invalid()),
            "partial" if tasks.is_empty() || unavailable.is_empty() => {
                return Err(usage_evidence_invalid());
            }
            "unavailable" if !tasks.is_empty() => return Err(usage_evidence_invalid()),
            _ => {}
        }
        let task_records = tasks
            .iter()
            .map(usage_task_projection)
            .collect::<anyhow::Result<Vec<_>>>()?;
        let task_count = task_records.len();
        let records = task_records.into_iter().take(limit).collect::<Vec<_>>();
        Ok(serde_json::json!({
            "artifactPresent": true,
            "artifactStatus": status,
            "taskCount": task_count,
            "unavailableTaskCount": unavailable.len(),
            "returnedTaskCount": records.len(),
            "truncated": task_count > records.len(),
            "tasks": records,
        }))
    }

    fn usage_task_projection(task: &Value) -> anyhow::Result<Value> {
        let task = usage_object(task)?;
        usage_required_bounded_identifier(task, "contractVersion", 128)?;
        usage_required_canonical_uuid(task, "taskID")?;
        usage_required_bounded_identifier(task, "taskState", 128)?;
        usage_optional_bounded_identifier(task.get("taskStateReasonCode"), 128)?;
        let _ = usage_required_u64_at_most(task, "taskVersion", MAX_USAGE_PROJECTION_UNITS)?;
        usage_required_bounded_identifier(task, "generatedAt", 128)?;
        let (reservation, settlement_recorded) = usage_reservation_projection(
            task.get("reservation").ok_or_else(usage_evidence_invalid)?,
        )?;
        let components = usage_required_array(task, "components")?;
        let lifecycle = usage_required_array(task, "lifecycle")?;
        if components.len() > MAX_USAGE_RECEIPT_COMPONENTS
            || lifecycle.len() > MAX_USAGE_RECEIPT_LIFECYCLE_EVENTS
        {
            return Err(usage_evidence_invalid());
        }
        let component_records = components
            .iter()
            .map(usage_component_projection)
            .collect::<anyhow::Result<Vec<_>>>()?;
        let lifecycle_records = lifecycle
            .iter()
            .map(usage_lifecycle_projection)
            .collect::<anyhow::Result<Vec<_>>>()?;
        let truncation = usage_object(task.get("truncation").ok_or_else(usage_evidence_invalid)?)?;
        let components_truncated = usage_required_bool(truncation, "components")?;
        let lifecycle_truncated = usage_required_bool(truncation, "lifecycle")?;
        Ok(serde_json::json!({
            "reservation": reservation,
            "settlement": {
                "settlementRecorded": settlement_recorded,
                "lifecycleEventCount": lifecycle_records.len(),
                "lifecycleTruncated": lifecycle_truncated,
                "lifecycle": lifecycle_records,
            },
            "receiptProjection": {
                "componentCount": component_records.len(),
                "componentsTruncated": components_truncated,
                "components": component_records,
            },
        }))
    }

    fn usage_reservation_projection(value: &Value) -> anyhow::Result<(Value, bool)> {
        if value.is_null() {
            return Ok((serde_json::json!({ "present": false }), false));
        }
        let reservation = usage_object(value)?;
        usage_required_canonical_uuid(reservation, "id")?;
        let state = usage_required_allowed_string(reservation, "state", &USAGE_RESERVATION_STATES)?;
        usage_required_bounded_identifier(reservation, "rateCardVersion", 256)?;
        let reserved_units =
            usage_required_u64_at_most(reservation, "reservedUnits", MAX_USAGE_PROJECTION_UNITS)?;
        let consumed_units =
            usage_required_u64_at_most(reservation, "consumedUnits", MAX_USAGE_PROJECTION_UNITS)?;
        if reserved_units == 0 || consumed_units > reserved_units {
            return Err(usage_evidence_invalid());
        }
        usage_required_bounded_identifier(reservation, "expiresAt", 128)?;
        usage_required_bounded_identifier(reservation, "createdAt", 128)?;
        usage_required_bounded_identifier(reservation, "updatedAt", 128)?;
        let settlement_recorded =
            usage_optional_bounded_identifier(reservation.get("settledAt"), 128)?;
        Ok((
            serde_json::json!({
                "present": true,
                "state": state,
                "reservedUnits": reserved_units,
                "consumedUnits": consumed_units,
                "settlementRecorded": settlement_recorded,
            }),
            settlement_recorded,
        ))
    }

    fn usage_component_projection(component: &Value) -> anyhow::Result<Value> {
        let component = usage_object(component)?;
        usage_required_decimal_identifier(component, "id")?;
        let kind = usage_required_allowed_string(
            component,
            "componentKind",
            &USAGE_RECEIPT_COMPONENT_KINDS,
        )?;
        let quantity =
            usage_required_u64_at_most(component, "quantity", MAX_USAGE_PROJECTION_UNITS)?;
        let normalized_units =
            usage_required_u64_at_most(component, "normalizedUnits", MAX_USAGE_PROJECTION_UNITS)?;
        usage_required_bounded_identifier(component, "rateCardVersion", 256)?;
        usage_required_bounded_identifier(component, "createdAt", 128)?;
        Ok(serde_json::json!({
            "kind": kind,
            "quantity": quantity,
            "normalizedUnits": normalized_units,
        }))
    }

    fn usage_lifecycle_projection(event: &Value) -> anyhow::Result<Value> {
        let event = usage_object(event)?;
        usage_required_decimal_identifier(event, "id")?;
        let kind = usage_required_allowed_string(event, "eventKind", &USAGE_LIFECYCLE_EVENT_KINDS)?;
        let quantity = usage_required_u64_at_most(event, "quantity", MAX_USAGE_PROJECTION_UNITS)?;
        let normalized_units =
            usage_required_u64_at_most(event, "normalizedUnits", MAX_USAGE_PROJECTION_UNITS)?;
        usage_optional_bounded_identifier(event.get("rateCardVersion"), 256)?;
        usage_required_bounded_identifier(event, "createdAt", 128)?;
        Ok(serde_json::json!({
            "kind": kind,
            "quantity": quantity,
            "normalizedUnits": normalized_units,
        }))
    }

    fn validate_unavailable_usage_task(value: &Value) -> anyhow::Result<()> {
        let unavailable = usage_object(value)?;
        usage_required_canonical_uuid(unavailable, "taskID")?;
        usage_required_bounded_identifier(unavailable, "reason", 128)
    }

    fn validate_usage_receipt_inspection_limit(limit: usize) -> anyhow::Result<()> {
        if !(1..=MAX_USAGE_RECEIPT_TASKS).contains(&limit) {
            return Err(usage_evidence_invalid());
        }
        Ok(())
    }

    fn usage_object(value: &Value) -> anyhow::Result<&Map<String, Value>> {
        value.as_object().ok_or_else(usage_evidence_invalid)
    }

    fn usage_required_array<'a>(
        fields: &'a Map<String, Value>,
        key: &str,
    ) -> anyhow::Result<&'a Vec<Value>> {
        fields
            .get(key)
            .and_then(Value::as_array)
            .ok_or_else(usage_evidence_invalid)
    }

    fn usage_required_bool(fields: &Map<String, Value>, key: &str) -> anyhow::Result<bool> {
        fields
            .get(key)
            .and_then(Value::as_bool)
            .ok_or_else(usage_evidence_invalid)
    }

    fn usage_required_nonnegative_i64(
        fields: &Map<String, Value>,
        key: &str,
    ) -> anyhow::Result<i64> {
        fields
            .get(key)
            .and_then(Value::as_i64)
            .filter(|value| *value >= 0)
            .ok_or_else(usage_evidence_invalid)
    }

    fn usage_required_u64_at_most(
        fields: &Map<String, Value>,
        key: &str,
        maximum: u64,
    ) -> anyhow::Result<u64> {
        fields
            .get(key)
            .and_then(Value::as_u64)
            .filter(|value| *value <= maximum)
            .ok_or_else(usage_evidence_invalid)
    }

    fn usage_required_allowed_string<'a>(
        fields: &'a Map<String, Value>,
        key: &str,
        allowed: &[&str],
    ) -> anyhow::Result<&'a str> {
        let value = fields
            .get(key)
            .and_then(Value::as_str)
            .ok_or_else(usage_evidence_invalid)?;
        if allowed.contains(&value) {
            Ok(value)
        } else {
            Err(usage_evidence_invalid())
        }
    }

    fn usage_required_canonical_uuid(fields: &Map<String, Value>, key: &str) -> anyhow::Result<()> {
        let value = fields
            .get(key)
            .and_then(Value::as_str)
            .ok_or_else(usage_evidence_invalid)?;
        if canonical_uuid(value).ok().as_deref() != Some(value) {
            return Err(usage_evidence_invalid());
        }
        Ok(())
    }

    fn usage_required_decimal_identifier(
        fields: &Map<String, Value>,
        key: &str,
    ) -> anyhow::Result<()> {
        let value = fields
            .get(key)
            .and_then(Value::as_str)
            .ok_or_else(usage_evidence_invalid)?;
        if value.is_empty() || value.len() > 32 || !value.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(usage_evidence_invalid());
        }
        Ok(())
    }

    fn usage_required_bounded_identifier(
        fields: &Map<String, Value>,
        key: &str,
        maximum: usize,
    ) -> anyhow::Result<()> {
        usage_bounded_identifier(fields.get(key).ok_or_else(usage_evidence_invalid)?, maximum)
    }

    fn usage_optional_bounded_identifier(
        value: Option<&Value>,
        maximum: usize,
    ) -> anyhow::Result<bool> {
        match value {
            None | Some(Value::Null) => Ok(false),
            Some(value) => {
                usage_bounded_identifier(value, maximum)?;
                Ok(true)
            }
        }
    }

    fn usage_bounded_identifier(value: &Value, maximum: usize) -> anyhow::Result<()> {
        let value = value.as_str().ok_or_else(usage_evidence_invalid)?;
        if value.is_empty() || value.len() > maximum || value.chars().any(char::is_control) {
            return Err(usage_evidence_invalid());
        }
        Ok(())
    }

    fn usage_evidence_invalid() -> anyhow::Error {
        anyhow::anyhow!("Installed live diagnostic Usage evidence is invalid.")
    }

    fn validate_prompt(prompt: &str) -> anyhow::Result<()> {
        if prompt.trim().is_empty() || prompt.len() > 128 * 1_024 {
            anyhow::bail!("Installed live diagnostic prompt must contain 1 byte through 128 KiB.");
        }
        Ok(())
    }

    fn validate_sources(sources: &[String]) -> anyhow::Result<()> {
        if sources.len() > 3
            || sources.iter().any(|value| {
                value.trim().is_empty() || value.len() > 240 || value.chars().any(char::is_control)
            })
        {
            anyhow::bail!("Choose one through three exact connected-source selectors.");
        }
        Ok(())
    }

    fn launch_installed_app(session_id: &str) -> anyhow::Result<()> {
        let session_id = canonical_uuid(session_id)?;
        let app = Path::new(APP_PATH);
        if !app.is_dir() || !app.join("Contents/Info.plist").is_file() {
            anyhow::bail!("The verified installed Whisply app is unavailable.");
        }
        let status = Command::new("/usr/bin/open")
            .args([
                "-a",
                APP_PATH,
                &format!("whisply://diagnostics?session={session_id}"),
            ])
            .status()
            .context("Unable to request the installed Whisply app.")?;
        if !status.success() {
            anyhow::bail!("Unable to request the verified installed Whisply app.");
        }
        Ok(())
    }

    /// Lists only self-contained, owner-only local session metadata. This
    /// intentionally never calls `LiveSession::load`, so it cannot validate a
    /// release, acquire a session secret, launch the app, or send a command.
    fn list_sessions(limit: usize) -> anyhow::Result<Value> {
        if !(1..=MAX_LISTED_SESSIONS).contains(&limit) {
            anyhow::bail!("Live diagnostic session list limit is invalid.");
        }
        let Some(root) = optional_existing_sessions_root()? else {
            return Ok(serde_json::json!({
                "ok": true,
                "protocol": LIVE_PROTOCOL,
                "currentSessionID": Value::Null,
                "retainedSessionLimit": MAX_LISTED_SESSIONS,
                "sessions": [],
                "commercialBoundary": "owner-only local metadata only; this command never launches or messages the installed app",
            }));
        };
        list_sessions_at(&root, limit, unix_ms()?)
    }

    fn list_sessions_at(root: &Path, limit: usize, now_ms: i64) -> anyhow::Result<Value> {
        if !(1..=MAX_LISTED_SESSIONS).contains(&limit) {
            anyhow::bail!("Live diagnostic session list limit is invalid.");
        }
        validate_private_directory(root)?;
        let current_session_id = listed_current_session_id(root);
        let mut sessions = Vec::new();
        for entry in fs::read_dir(root)
            .context("Unable to read the owner-only live diagnostic session inventory.")?
        {
            let Ok(entry) = entry else { continue };
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if !file_type.is_dir() || file_type.is_symlink() {
                continue;
            }
            let directory = entry.path();
            if validate_private_directory(&directory).is_err() {
                continue;
            }
            let Some(raw_id) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Ok(session_id) = canonical_uuid(&raw_id) else {
                continue;
            };
            let bootstrap: Bootstrap = match read_private_json(&directory.join("bootstrap.json")) {
                Ok(bootstrap) => bootstrap,
                Err(_) => continue,
            };
            if validate_bootstrap(&bootstrap, &session_id, true).is_err() {
                continue;
            }
            sessions.push(ListedLiveSession {
                current: current_session_id.as_deref() == Some(session_id.as_str()),
                expired: bootstrap.expires_at_ms <= now_ms,
                session_id,
                created_at_ms: bootstrap.created_at_ms,
                expires_at_ms: bootstrap.expires_at_ms,
            });
        }
        sessions.sort_by(|left, right| {
            right
                .created_at_ms
                .cmp(&left.created_at_ms)
                .then_with(|| left.session_id.cmp(&right.session_id))
        });
        sessions.truncate(limit);
        let current_session_id = current_session_id.filter(|current| {
            sessions
                .iter()
                .any(|session| session.session_id == *current)
        });
        Ok(serde_json::json!({
            "ok": true,
            "protocol": LIVE_PROTOCOL,
            "currentSessionID": current_session_id,
            "retainedSessionLimit": MAX_LISTED_SESSIONS,
            "sessions": sessions,
            "commercialBoundary": "owner-only local metadata only; this command never launches or messages the installed app",
        }))
    }

    fn listed_current_session_id(root: &Path) -> Option<String> {
        let path = current_session_path(root).ok()?;
        let pointer: CurrentSession = read_private_json(&path).ok()?;
        if pointer.protocol != LIVE_PROTOCOL {
            return None;
        }
        let id = canonical_uuid(&pointer.session_id).ok()?;
        if !pointer.session_directory.is_empty()
            && Path::new(&pointer.session_directory) != root.join(&id)
        {
            return None;
        }
        Some(id)
    }

    fn doctor(requested_session: Option<&str>) -> anyhow::Result<Value> {
        let app = Path::new(APP_PATH);
        let app_present = app.join("Contents/Info.plist").is_file();
        let (code_signature_valid, code_signature_detail) = code_signature_status(app, app_present);
        let (session_state, session_error) = match LiveSession::load(requested_session) {
            Ok(session) => match session.state_payload() {
                Ok(state) => (state.unwrap_or(Value::Null), Value::Null),
                Err(error) => (Value::Null, Value::String(doctor_error_detail(&error))),
            },
            Err(error) => (Value::Null, Value::String(doctor_error_detail(&error))),
        };
        Ok(serde_json::json!({
            "ok": app_present && code_signature_valid,
            "protocol": LIVE_PROTOCOL,
            "installedApp": APP_PATH,
            "appPresent": app_present,
            "codeSignatureValid": code_signature_valid,
            "codeSignatureDetail": code_signature_detail,
            "runningProcessIDs": running_process_ids(),
            "sessionState": session_state,
            "sessionError": session_error,
            "commercialBoundary": "live diagnostics uses the normal installed-product authority path and cannot grant paid access",
        }))
    }

    fn code_signature_status(app: &Path, app_present: bool) -> (bool, String) {
        if !app_present {
            return (false, String::new());
        }
        match Command::new("/usr/bin/codesign")
            .args(["--verify", "--deep", "--strict"])
            .arg(app)
            .output()
        {
            Ok(output) => {
                let detail = if output.stderr.is_empty() {
                    bounded_normalized_detail(&output.stdout)
                } else {
                    bounded_normalized_detail(&output.stderr)
                };
                (output.status.success(), detail)
            }
            Err(_) => (
                false,
                "Unable to verify the installed app signature.".to_string(),
            ),
        }
    }

    fn running_process_ids() -> Vec<u32> {
        match Command::new("/usr/bin/pgrep")
            .args(["-x", "Whisply"])
            .output()
        {
            Ok(output) => parse_running_process_ids(&output.stdout),
            Err(_) => Vec::new(),
        }
    }

    fn parse_running_process_ids(bytes: &[u8]) -> Vec<u32> {
        let mut ids = String::from_utf8_lossy(bytes)
            .lines()
            .filter_map(|line| line.trim().parse::<u32>().ok())
            .filter(|id| *id > 1)
            .collect::<Vec<_>>();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    fn select_tail_summaries(mut summaries: Vec<Value>, since: u64, limit: usize) -> Vec<Value> {
        summaries.retain(|summary| {
            summary
                .get("sequence")
                .and_then(Value::as_u64)
                .is_some_and(|sequence| sequence > since)
        });
        let start = summaries.len().saturating_sub(limit);
        summaries.split_off(start)
    }

    fn bounded_normalized_detail(bytes: &[u8]) -> String {
        const MAX_DETAIL_CHARS: usize = 4_096;
        let normalized = String::from_utf8_lossy(bytes)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        normalized.chars().take(MAX_DETAIL_CHARS).collect()
    }

    fn doctor_error_detail(error: &anyhow::Error) -> String {
        bounded_normalized_detail(error.to_string().as_bytes())
    }

    fn sessions_root() -> anyhow::Result<PathBuf> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute() && path != Path::new("/"))
            .ok_or_else(|| anyhow::anyhow!("The current user's home directory is unavailable."))?;
        let logs = home.join("Library").join("Logs");
        fs::create_dir_all(&logs)
            .context("Unable to prepare the current user's diagnostic root.")?;
        let product_root = logs.join("Whisply");
        ensure_private_directory(&product_root)?;
        let diagnostic_root = product_root.join("LiveDiagnostics");
        ensure_private_directory(&diagnostic_root)?;
        Ok(diagnostic_root.join("Sessions"))
    }

    fn existing_sessions_root() -> anyhow::Result<PathBuf> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute() && path != Path::new("/"))
            .ok_or_else(|| anyhow::anyhow!("The current user's home directory is unavailable."))?;
        let root = home
            .join("Library")
            .join("Logs")
            .join("Whisply")
            .join("LiveDiagnostics")
            .join("Sessions");
        validate_private_directory(&root)?;
        Ok(root)
    }

    /// Finds the retained session root without creating directories. Listing is
    /// deliberately observational: an absent diagnostics root is an empty
    /// inventory, not a reason to prepare persistent state.
    fn optional_existing_sessions_root() -> anyhow::Result<Option<PathBuf>> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute() && path != Path::new("/"))
            .ok_or_else(|| anyhow::anyhow!("The current user's home directory is unavailable."))?;
        let root = home
            .join("Library")
            .join("Logs")
            .join("Whisply")
            .join("LiveDiagnostics")
            .join("Sessions");
        match fs::symlink_metadata(&root) {
            Ok(_) => {
                validate_private_directory(&root)?;
                Ok(Some(root))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => anyhow::bail!("The owner-only live diagnostic directory is unavailable."),
        }
    }

    fn current_session_path(root: &Path) -> anyhow::Result<PathBuf> {
        let parent = root
            .parent()
            .ok_or_else(|| anyhow::anyhow!("The owner-only live diagnostic root is invalid."))?;
        validate_private_directory(parent)?;
        Ok(parent.join("current.json"))
    }

    fn installed_release_manifest_sha256() -> anyhow::Result<String> {
        let path = Path::new(APP_PATH).join(RUNTIME_MANIFEST_RELATIVE_PATH);
        let metadata = fs::symlink_metadata(&path)
            .context("The verified installed Whisply runtime manifest is unavailable.")?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.len() > MAX_EVIDENCE_BYTES
        {
            anyhow::bail!("The verified installed Whisply runtime manifest is unavailable.");
        }
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
            .context("The verified installed Whisply runtime manifest is unavailable.")?;
        let mut digest = Sha256::new();
        let mut buffer = [0_u8; 64 * 1_024];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            digest.update(&buffer[..read]);
        }
        Ok(hex_encode(&digest.finalize()))
    }

    fn validate_bootstrap(
        bootstrap: &Bootstrap,
        expected_id: &str,
        allow_expired: bool,
    ) -> anyhow::Result<()> {
        let now_ms = unix_ms()?;
        if bootstrap.protocol_name != LIVE_PROTOCOL
            || bootstrap.session_id != expected_id
            || !valid_lowercase_sha256(&bootstrap.release_manifest_sha256)
            || bootstrap.purpose != "installed-live-product-path"
            || bootstrap.controller_pid <= 1
            || bootstrap.created_at_ms > now_ms.saturating_add(30_000)
            || bootstrap.expires_at_ms <= bootstrap.created_at_ms
            || bootstrap
                .expires_at_ms
                .saturating_sub(bootstrap.created_at_ms)
                > MAX_SESSION_LIFETIME_MS
            || (!allow_expired && bootstrap.expires_at_ms <= now_ms)
        {
            anyhow::bail!("The owner-only live diagnostic session is invalid or expired.");
        }
        Ok(())
    }

    fn ensure_private_directory(path: &Path) -> anyhow::Result<()> {
        if path.exists() {
            return validate_private_directory(path);
        }
        let parent = path.parent().ok_or_else(|| {
            anyhow::anyhow!("Unable to prepare the owner-only live diagnostic root.")
        })?;
        if !parent.is_dir() || fs::symlink_metadata(parent)?.file_type().is_symlink() {
            anyhow::bail!("Unable to prepare the owner-only live diagnostic root.");
        }
        fs::create_dir(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        validate_private_directory(path)
    }

    fn create_new_private_directory(path: &Path) -> anyhow::Result<()> {
        fs::create_dir(path).map_err(|_| {
            anyhow::anyhow!("Unable to create an owner-only live diagnostic directory.")
        })?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        validate_private_directory(path)
    }

    fn validate_private_directory(path: &Path) -> anyhow::Result<()> {
        let metadata = fs::symlink_metadata(path).map_err(|_| {
            anyhow::anyhow!("The owner-only live diagnostic directory is unavailable.")
        })?;
        if metadata.file_type().is_symlink()
            || !metadata.is_dir()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o077 != 0
        {
            anyhow::bail!("The owner-only live diagnostic directory is unsafe.");
        }
        Ok(())
    }

    fn validate_private_file(path: &Path) -> anyhow::Result<()> {
        let metadata = fs::symlink_metadata(path)
            .map_err(|_| anyhow::anyhow!("The owner-only live diagnostic file is unavailable."))?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o077 != 0
        {
            anyhow::bail!("The owner-only live diagnostic file is unsafe.");
        }
        Ok(())
    }

    fn write_new_json<T: Serialize>(path: &Path, value: &T) -> anyhow::Result<()> {
        let mut bytes = serde_json::to_vec(value)?;
        bytes.push(b'\n');
        write_new_private(path, &bytes)
    }

    fn write_replace_json<T: Serialize>(path: &Path, value: &T) -> anyhow::Result<()> {
        let mut bytes = serde_json::to_vec(value)?;
        bytes.push(b'\n');
        write_replace_private(path, &bytes)
    }

    fn write_new_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
        let parent = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("The owner-only live diagnostic path is invalid."))?;
        validate_private_directory(parent)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .map_err(|_| anyhow::anyhow!("Unable to create an owner-only live diagnostic file."))?;
        file.write_all(bytes)?;
        file.sync_all()?;
        validate_private_file(path)
    }

    fn write_replace_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
        let parent = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("The owner-only live diagnostic path is invalid."))?;
        validate_private_directory(parent)?;
        let temporary = parent.join(format!(
            ".{}.{}.tmp",
            path.file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("diagnostic"),
            Uuid::new_v4()
        ));
        write_new_private(&temporary, bytes)?;
        fs::rename(&temporary, path)?;
        validate_private_file(path)
    }

    fn read_private_json<T: for<'de> Deserialize<'de>>(path: &Path) -> anyhow::Result<T> {
        let bytes = read_private_bytes(path, MAX_JSON_BYTES)?;
        serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("The owner-only live diagnostic JSON is invalid."))
    }

    fn read_private_bytes(path: &Path, limit: usize) -> anyhow::Result<Vec<u8>> {
        validate_private_file(path)?;
        let metadata = fs::symlink_metadata(path)?;
        if metadata.len() > u64::try_from(limit).unwrap_or(u64::MAX) {
            anyhow::bail!("The owner-only live diagnostic file is too large.");
        }
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        file.read_to_end(&mut bytes)?;
        if bytes.len() > limit {
            anyhow::bail!("The owner-only live diagnostic file is too large.");
        }
        Ok(bytes)
    }

    fn verify_hash_manifest(root: &Path) -> anyhow::Result<Value> {
        let manifest = root.join("hashes.sha256");
        let bytes = read_private_bytes(&manifest, MAX_EVIDENCE_BYTES as usize)?;
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| anyhow::anyhow!("Installed live diagnostic hash evidence is invalid."))?;
        let mut expected = BTreeMap::new();
        for line in text.lines().filter(|line| !line.is_empty()) {
            let (hash, relative) = line.split_once("  ").ok_or_else(|| {
                anyhow::anyhow!("Installed live diagnostic hash evidence is invalid.")
            })?;
            if !valid_lowercase_sha256(hash)
                || !valid_relative_path(relative)
                || expected
                    .insert(relative.to_string(), hash.to_string())
                    .is_some()
            {
                anyhow::bail!("Installed live diagnostic hash evidence is invalid.");
            }
        }
        let mut actual = BTreeMap::new();
        let mut total_bytes = 0_u64;
        collect_regular_file_hashes(root, root, &mut actual, &mut total_bytes)?;
        if expected != actual || total_bytes > MAX_EVIDENCE_BYTES {
            anyhow::bail!(
                "Installed live diagnostic hash evidence does not match its owner-only files."
            );
        }
        Ok(serde_json::json!({
            "fileCount": actual.len(),
            "totalBytes": total_bytes,
            "hashManifestSha256": sha256_file(&manifest)?,
        }))
    }

    fn collect_regular_file_hashes(
        root: &Path,
        directory: &Path,
        output: &mut BTreeMap<String, String>,
        total_bytes: &mut u64,
    ) -> anyhow::Result<()> {
        validate_private_directory(directory)?;
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                anyhow::bail!("Installed live diagnostic evidence contains a symlink.");
            }
            if metadata.is_dir() {
                collect_regular_file_hashes(root, &path, output, total_bytes)?;
            } else if metadata.is_file() {
                validate_private_file(&path)?;
                let relative = path
                    .strip_prefix(root)
                    .map_err(|_| {
                        anyhow::anyhow!("Installed live diagnostic evidence escaped its root.")
                    })?
                    .to_string_lossy()
                    .replace('\\', "/");
                if relative == "hashes.sha256" {
                    continue;
                }
                *total_bytes = total_bytes.saturating_add(metadata.len());
                output.insert(relative, sha256_file(&path)?);
            } else {
                anyhow::bail!(
                    "Installed live diagnostic evidence contains an unsupported file type."
                );
            }
        }
        Ok(())
    }

    fn sha256_file(path: &Path) -> anyhow::Result<String> {
        validate_private_file(path)?;
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        let mut digest = Sha256::new();
        let mut buffer = [0_u8; 64 * 1_024];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            digest.update(&buffer[..read]);
        }
        Ok(hex_encode(&digest.finalize()))
    }

    fn validate_run_metadata(
        value: &Value,
        run_id: &str,
        release_hash: &str,
        kind: &str,
    ) -> anyhow::Result<()> {
        if value.get("protocol").and_then(Value::as_str) != Some(LIVE_PROTOCOL)
            || value.get("runID").and_then(Value::as_str) != Some(run_id)
            || value.get("releaseManifestSha256").and_then(Value::as_str) != Some(release_hash)
            || value
                .get("hiddenReasoningCaptured")
                .and_then(Value::as_bool)
                != Some(false)
        {
            anyhow::bail!("Installed live diagnostic {kind} evidence is invalid.");
        }
        Ok(())
    }

    fn envelope_signing_material(envelope: &SignedEnvelope) -> Vec<u8> {
        [
            envelope.protocol_name.as_str(),
            envelope.session_id.as_str(),
            envelope.envelope_id.as_str(),
            &envelope.sequence.to_string(),
            &envelope.issued_at_ms.to_string(),
            &envelope.expires_at_ms.to_string(),
            envelope.kind.as_str(),
            envelope.name.as_str(),
            envelope.payload_base64.as_str(),
        ]
        .join("\n")
        .into_bytes()
    }

    fn random_secret() -> Vec<u8> {
        let mut secret = Vec::with_capacity(32);
        secret.extend_from_slice(Uuid::new_v4().as_bytes());
        secret.extend_from_slice(Uuid::new_v4().as_bytes());
        secret
    }

    fn canonical_uuid(value: &str) -> anyhow::Result<String> {
        let parsed = Uuid::parse_str(value)
            .map_err(|_| anyhow::anyhow!("Live diagnostic identifiers must be canonical UUIDs."))?;
        let canonical = parsed.to_string().to_ascii_lowercase();
        if value != canonical {
            anyhow::bail!("Live diagnostic identifiers must be canonical UUIDs.");
        }
        Ok(canonical)
    }

    /// Accepts an ordinary conversation identifier and nothing that could
    /// reach past this account's own chats.
    ///
    /// Deliberately narrower than "any string the app might accept": this
    /// selector travels to the installed app, and a path, URL, or control
    /// character in it has no legitimate reading as a conversation.
    fn canonical_conversation_id(value: &str) -> anyhow::Result<String> {
        let trimmed = value.trim();
        anyhow::ensure!(
            !trimmed.is_empty()
                && trimmed.len() <= 128
                && trimmed
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')),
            "A conversation id is letters, digits, dashes, and underscores."
        );
        Ok(trimmed.to_string())
    }

    fn valid_command_name(value: &str) -> bool {
        matches!(
            value,
            "status"
                | "run"
                | "stop"
                | "snapshot"
                | "runtime-recovery"
                | "account-projection"
                | "compaction"
                | "close"
        )
    }

    fn runtime_recovery_payload(cancel_superseded_computer_use_setup: bool) -> Value {
        serde_json::json!({
            "cancelSupersededComputerUseSetup": cancel_superseded_computer_use_setup,
        })
    }

    fn valid_lowercase_sha256(value: &str) -> bool {
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }

    fn valid_relative_path(value: &str) -> bool {
        let path = Path::new(value);
        !value.is_empty()
            && !path.is_absolute()
            && path
                .components()
                .all(|component| matches!(component, Component::Normal(_)))
    }

    fn decode_hex(value: &str) -> Option<Vec<u8>> {
        if value.len() % 2 != 0
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return None;
        }
        value
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| {
                let high = hex_digit(pair[0])?;
                let low = hex_digit(pair[1])?;
                Some((high << 4) | low)
            })
            .collect()
    }

    fn hex_digit(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            _ => None,
        }
    }

    fn hex_encode(bytes: &[u8]) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut output = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            output.push(HEX[usize::from(byte >> 4)] as char);
            output.push(HEX[usize::from(byte & 0x0f)] as char);
        }
        output
    }

    fn unix_ms() -> anyhow::Result<i64> {
        let elapsed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("System clock is before the Unix epoch.")?
            .as_millis();
        i64::try_from(elapsed).context("System clock cannot be represented in milliseconds.")
    }

    fn print_json(value: &Value) -> anyhow::Result<()> {
        println!("{}", serde_json::to_string_pretty(value)?);
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn write_listed_session(root: &Path, id: &str, created_at_ms: i64, expires_at_ms: i64) {
            let directory = root.join(id);
            create_new_private_directory(&directory).expect("create listed-session directory");
            write_new_json(
                &directory.join("bootstrap.json"),
                &Bootstrap {
                    protocol_name: LIVE_PROTOCOL.to_string(),
                    session_id: id.to_string(),
                    release_manifest_sha256: "a".repeat(64),
                    created_at_ms,
                    expires_at_ms,
                    secret_base64: base64::engine::general_purpose::STANDARD.encode([7_u8; 32]),
                    controller_pid: 2,
                    purpose: "installed-live-product-path".to_string(),
                },
            )
            .expect("write listed-session bootstrap");
        }

        #[test]
        fn signing_material_matches_the_installed_swift_protocol() {
            let envelope = SignedEnvelope {
                protocol_name: LIVE_PROTOCOL.to_string(),
                session_id: "00000000-0000-4000-8000-000000000000".to_string(),
                envelope_id: "11111111-1111-4111-8111-111111111111".to_string(),
                sequence: 7,
                issued_at_ms: 100,
                expires_at_ms: 200,
                kind: "command".to_string(),
                name: "status".to_string(),
                payload_base64: "e30=".to_string(),
                hmac_sha256: String::new(),
            };
            assert_eq!(
                String::from_utf8(envelope_signing_material(&envelope)).expect("utf8"),
                "whisply.live-diagnostics.v1\n00000000-0000-4000-8000-000000000000\n11111111-1111-4111-8111-111111111111\n7\n100\n200\ncommand\nstatus\ne30="
            );
        }

        #[test]
        fn run_argument_policy_preserves_browser_chrome_computer_use_and_connector_separation() {
            assert!(matches!(
                InstalledLivePlugin::Browser.protocol_id(),
                "browser"
            ));
            assert!(matches!(
                InstalledLivePlugin::Chrome.protocol_id(),
                "chrome"
            ));
            assert!(matches!(
                InstalledLivePlugin::ComputerUse.protocol_id(),
                "computer_use"
            ));
            assert_eq!(
                InstalledLiveRoute::NativeComputerUse.protocol_id(),
                "native-computer-use"
            );
            assert_eq!(
                InstalledLiveComputerUseCohort::VisualLoop.protocol_id(),
                "visual-loop"
            );
        }

        #[test]
        fn canonical_identifiers_and_evidence_paths_fail_closed() {
            assert!(canonical_uuid("00000000-0000-4000-8000-000000000000").is_ok());
            assert!(canonical_uuid("00000000-0000-4000-8000-000000000000 ").is_err());
            assert!(valid_relative_path("screenshots/finished.png"));
            assert!(!valid_relative_path("../outside"));
            assert!(!valid_relative_path("/outside"));
        }

        #[test]
        fn a_conversation_selector_cannot_be_a_path_or_a_command() {
            assert!(canonical_conversation_id("session-1_A").is_ok());
            assert_eq!(
                canonical_conversation_id("  session-1  ").ok(),
                Some("session-1".to_string()),
                "surrounding space is a typing artifact, not a different chat"
            );
            for hostile in [
                "",
                "   ",
                "../../other-account",
                "/Applications/Whisply.app",
                "https://example.test/thread",
                "session 1",
                "session\n1",
                &"a".repeat(129),
            ] {
                assert!(
                    canonical_conversation_id(hostile).is_err(),
                    "this selector travels to the installed app and has no \
                     legitimate reading as a conversation: {hostile:?}"
                );
            }
        }

        #[test]
        fn the_compaction_question_is_one_the_installed_app_will_answer() {
            assert!(
                valid_command_name("compaction"),
                "a command the CLI can send but the response reader rejects \
                 would look like an app that never replied"
            );
        }

        #[test]
        fn runtime_recovery_has_one_host_enforced_cancellation_switch() {
            assert!(valid_command_name("runtime-recovery"));
            assert!(!valid_command_name("runtime-recovery/cancel-all"));
            assert_eq!(
                runtime_recovery_payload(false),
                serde_json::json!({ "cancelSupersededComputerUseSetup": false })
            );
            assert_eq!(
                runtime_recovery_payload(true),
                serde_json::json!({ "cancelSupersededComputerUseSetup": true })
            );
        }

        #[test]
        fn doctor_stays_out_of_the_live_command_channel_and_sanitizes_local_output() {
            assert!(
                !valid_command_name("doctor"),
                "doctor reads only local app/session state and must never reach the installed-app command channel"
            );
            assert_eq!(
                parse_running_process_ids(b"5\ninvalid\n1\n7\n5\n0\n"),
                vec![5, 7]
            );
            assert_eq!(
                bounded_normalized_detail(b" one\n two\tthree "),
                "one two three"
            );
            assert_eq!(
                bounded_normalized_detail(&vec![b'x'; 4_097])
                    .chars()
                    .count(),
                4_096
            );
        }

        #[test]
        fn session_list_is_bounded_redacted_and_never_requires_app_authority() {
            let temp = tempfile::tempdir().expect("temporary owner root");
            fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700))
                .expect("secure temporary owner root");
            let root = temp.path().join("Sessions");
            create_new_private_directory(&root).expect("create session inventory");

            let now_ms = unix_ms().expect("current time");
            let older = "00000000-0000-4000-8000-000000000000";
            let current = "11111111-1111-4111-8111-111111111111";
            write_listed_session(
                &root,
                older,
                now_ms.saturating_sub(2_000),
                now_ms.saturating_sub(1_000),
            );
            write_listed_session(
                &root,
                current,
                now_ms.saturating_sub(500),
                now_ms.saturating_add(1_000),
            );
            create_new_private_directory(&root.join("not-a-canonical-session"))
                .expect("create ignored inventory entry");
            write_new_json(
                &current_session_path(&root).expect("current-session path"),
                &CurrentSession {
                    protocol: LIVE_PROTOCOL.to_string(),
                    session_id: current.to_string(),
                    session_directory: root.join(current).to_string_lossy().into_owned(),
                    updated_at_ms: now_ms,
                },
            )
            .expect("write current-session pointer");

            let report = list_sessions_at(&root, MAX_LISTED_SESSIONS, now_ms)
                .expect("list local retained sessions");
            assert_eq!(report["ok"], Value::Bool(true));
            assert_eq!(report["protocol"], Value::String(LIVE_PROTOCOL.to_string()));
            assert_eq!(
                report["currentSessionID"],
                Value::String(current.to_string())
            );
            let sessions = report["sessions"].as_array().expect("session summaries");
            assert_eq!(sessions.len(), 2);
            assert_eq!(sessions[0]["sessionID"], current);
            assert_eq!(sessions[0]["current"], Value::Bool(true));
            assert_eq!(sessions[0]["expired"], Value::Bool(false));
            assert_eq!(sessions[1]["sessionID"], older);
            assert_eq!(sessions[1]["current"], Value::Bool(false));
            assert_eq!(sessions[1]["expired"], Value::Bool(true));

            let serialized = serde_json::to_string(&report).expect("redacted JSON");
            let temporary_root = temp.path().to_string_lossy().into_owned();
            for forbidden in [
                "secretBase64",
                "sessionDirectory",
                "controllerPID",
                "releaseManifestSha256",
                temporary_root.as_str(),
            ] {
                assert!(
                    !serialized.contains(forbidden),
                    "the read-only inventory must not disclose {forbidden:?}"
                );
            }

            let one = list_sessions_at(&root, 1, now_ms).expect("bounded inventory");
            assert_eq!(one["sessions"].as_array().map(Vec::len), Some(1));
            assert_eq!(one["sessions"][0]["sessionID"], current);
            assert!(list_sessions_at(&root, MAX_LISTED_SESSIONS + 1, now_ms).is_err());
        }

        #[test]
        fn tail_since_is_a_local_evidence_cursor() {
            let summaries = vec![
                serde_json::json!({ "sequence": 4, "name": "run.started" }),
                serde_json::json!({ "sequence": 5, "name": "run.progress" }),
                serde_json::json!({ "sequence": 6, "name": "run.progress" }),
                serde_json::json!({ "sequence": 7, "name": "run.finished" }),
            ];
            assert_eq!(
                select_tail_summaries(summaries.clone(), 5, 1),
                vec![serde_json::json!({ "sequence": 7, "name": "run.finished" })]
            );
            assert_eq!(
                select_tail_summaries(summaries, 5, 10),
                vec![
                    serde_json::json!({ "sequence": 6, "name": "run.progress" }),
                    serde_json::json!({ "sequence": 7, "name": "run.finished" }),
                ]
            );
        }

        fn provider_usage_event(
            sequence: u64,
            timestamp_ms: i64,
            fields: Value,
        ) -> VerifiedRunEvent {
            VerifiedRunEvent {
                sequence,
                timestamp_ms,
                name: "provider.usage".to_string(),
                fields,
            }
        }

        fn policy_event(
            sequence: u64,
            timestamp_ms: i64,
            name: &str,
            fields: Value,
        ) -> VerifiedRunEvent {
            VerifiedRunEvent {
                sequence,
                timestamp_ms,
                name: name.to_string(),
                fields,
            }
        }

        fn completed_policy_manifest() -> Value {
            serde_json::json!({
                "protocol": LIVE_PROTOCOL,
                "runID": "00000000-0000-4000-8000-000000000000",
                "releaseManifestSha256": "a".repeat(64),
                "hiddenReasoningCaptured": false,
                "request": {
                    "prompt": "private user prompt",
                    "requestedModelID": "private-model-selection",
                    "resolvedModelID": "private-resolved-model",
                    "usesScreen": false,
                    "computerUse": true,
                    "route": "native-computer-use",
                    "newChat": true,
                    "captureArtifacts": true,
                    "selectedSourceCount": 1,
                    "selectedSources": [{
                        "provider": "private-source",
                        "connectionIDHash": "private-connection-hash",
                    }],
                    "selectedPlugin": {
                        "id": "computer_use",
                        "stablePluginID": "private-stable-plugin-id",
                        "version": "private-plugin-version",
                    },
                    "computerUseEfficiencyCohort": "semantic",
                },
            })
        }

        fn completed_authority_snapshot() -> Value {
            serde_json::json!({
                "signedIn": true,
                "authState": "private-auth-state",
                "accountIDHash": "private-account-hash",
                "authorityCurrent": true,
                "subscription": {
                    "plan": "private-plan",
                    "isActive": true,
                },
                "computerUse": {
                    "snapshotCurrent": true,
                    "masterEnabled": true,
                    "serverPolicyUsable": true,
                    "serverPolicyVersion": "private-policy-version",
                    "acceptedRevision": "private-policy-revision",
                    "nativeAppsEnabled": true,
                    "nativeAppsEffective": true,
                    "helperReady": true,
                    "helperDetail": "private-helper-detail",
                    "capabilities": {
                        "computerUse": true,
                        "nativeApps": true,
                        "existingBrowser": true,
                        "spawnedBrowser": false,
                        "localFiles": false,
                    },
                },
                "commercialBoundary": "observes normal product gates and grants no session, provider, or policy authority",
            })
        }

        fn completed_task_usage_artifact() -> Value {
            serde_json::json!({
                "scope": "contextual-task",
                "source": "authenticated-runtime-task-detail",
                "status": "complete",
                "generatedAtMs": 1_000,
                "tasks": [{
                    "contractVersion": "private-contract-version",
                    "taskID": "22222222-2222-4222-8222-222222222222",
                    "taskState": "private-task-state",
                    "taskStateReasonCode": "private-task-reason",
                    "taskVersion": 3,
                    "generatedAt": "private-generated-at",
                    "reservation": {
                        "id": "33333333-3333-4333-8333-333333333333",
                        "state": "settled",
                        "rateCardVersion": "private-rate-card",
                        "reservedUnits": 100,
                        "consumedUnits": 12,
                        "expiresAt": "private-expires-at",
                        "createdAt": "private-created-at",
                        "updatedAt": "private-updated-at",
                        "settledAt": "private-settled-at",
                    },
                    "components": [
                        {
                            "id": "1001",
                            "componentKind": "model.input_token",
                            "quantity": 10,
                            "normalizedUnits": 7,
                            "rateCardVersion": "private-component-rate-card",
                            "createdAt": "private-component-created-at",
                        },
                        {
                            "id": "1002",
                            "componentKind": "computer_use.native_step",
                            "quantity": 2,
                            "normalizedUnits": 5,
                            "rateCardVersion": "private-component-rate-card",
                            "createdAt": "private-component-created-at",
                        },
                    ],
                    "lifecycle": [
                        {
                            "id": "1003",
                            "eventKind": "reserved",
                            "quantity": 100,
                            "normalizedUnits": 100,
                            "rateCardVersion": "private-lifecycle-rate-card",
                            "createdAt": "private-lifecycle-created-at",
                        },
                        {
                            "id": "1004",
                            "eventKind": "settled",
                            "quantity": 12,
                            "normalizedUnits": 12,
                            "rateCardVersion": "private-lifecycle-rate-card",
                            "createdAt": "private-lifecycle-created-at",
                        },
                    ],
                    "truncation": { "components": false, "lifecycle": false },
                }],
                "unavailable": [],
                "privacyProjection": "task identity, state, reservation, components, lifecycle, and truncation flags only",
            })
        }

        #[test]
        fn usage_receipt_inspection_is_completed_run_only_bounded_and_redacted() {
            let report = usage_receipt_projection(&completed_task_usage_artifact(), 1)
                .expect("redacted Usage receipt projection");
            assert_eq!(report["artifactPresent"], Value::Bool(true));
            assert_eq!(report["artifactStatus"], "complete");
            assert_eq!(report["taskCount"], 1);
            assert_eq!(report["returnedTaskCount"], 1);
            assert_eq!(report["tasks"][0]["reservation"]["state"], "settled");
            assert_eq!(
                report["tasks"][0]["reservation"]["settlementRecorded"],
                Value::Bool(true)
            );
            assert_eq!(report["tasks"][0]["receiptProjection"]["componentCount"], 2);
            assert_eq!(
                report["tasks"][0]["receiptProjection"]["components"][1]["kind"],
                "computer_use.native_step"
            );
            assert_eq!(
                report["tasks"][0]["settlement"]["lifecycle"][1]["kind"],
                "settled"
            );

            let absent = no_usage_receipt_projection(1).expect("not-captured Usage projection");
            assert_eq!(absent["artifactPresent"], Value::Bool(false));
            assert_eq!(absent["artifactStatus"], "not-captured");

            let serialized = serde_json::to_string(&report).expect("redacted Usage receipt JSON");
            for forbidden in [
                "private-contract-version",
                "22222222-2222-4222-8222-222222222222",
                "private-task-state",
                "private-task-reason",
                "private-generated-at",
                "33333333-3333-4333-8333-333333333333",
                "private-rate-card",
                "private-expires-at",
                "private-created-at",
                "private-updated-at",
                "private-settled-at",
                "private-component-rate-card",
                "private-component-created-at",
                "private-lifecycle-rate-card",
                "private-lifecycle-created-at",
                "\"taskID\"",
                "\"rateCardVersion\"",
                "\"createdAt\"",
            ] {
                assert!(
                    !serialized.contains(forbidden),
                    "Usage projection must not expose {forbidden:?}"
                );
            }
        }

        #[test]
        fn usage_receipt_inspection_rejects_malformed_or_unbounded_evidence() {
            let mut malformed_component = completed_task_usage_artifact();
            malformed_component["tasks"][0]["components"][0]["componentKind"] =
                Value::String("private-component-kind".to_string());
            assert!(usage_receipt_projection(&malformed_component, 1).is_err());

            let mut malformed_reservation = completed_task_usage_artifact();
            malformed_reservation["tasks"][0]["reservation"]["consumedUnits"] = 101.into();
            assert!(usage_receipt_projection(&malformed_reservation, 1).is_err());

            let mut malformed_status = completed_task_usage_artifact();
            malformed_status["status"] = Value::String("partial".to_string());
            assert!(usage_receipt_projection(&malformed_status, 1).is_err());

            assert!(validate_usage_receipt_inspection_limit(0).is_err());
            assert!(validate_usage_receipt_inspection_limit(MAX_USAGE_RECEIPT_TASKS + 1).is_err());
        }

        #[test]
        fn token_and_cost_inspection_is_bounded_redacted_and_unaggregated() {
            let run_id = "00000000-0000-4000-8000-000000000000";
            let events = vec![
                provider_usage_event(
                    3,
                    1_000,
                    serde_json::json!({
                        "provider": "private-provider-name",
                        "model": "private-model-name",
                        "responseIDHash": "private-response-hash",
                        "inputTokens": 12,
                        "outputTokens": 4,
                        "cachedInputTokens": 3,
                        "cost": 0.045,
                        "costDetails": {
                            "authorization": "private-cost-detail",
                        },
                    }),
                ),
                provider_usage_event(
                    6,
                    2_000,
                    serde_json::json!({
                        "reasoningTokens": 9,
                        "audioOutputTokens": 2,
                    }),
                ),
                provider_usage_event(8, 3_000, serde_json::json!({})),
            ]
            .iter()
            .map(VerifiedProviderUsage::from_event)
            .collect::<anyhow::Result<Vec<_>>>()
            .expect("valid provider-usage events");

            let tokens = token_class_inspection_report(run_id, &events, 0, 1)
                .expect("bounded token-class report");
            assert_eq!(tokens["tokenClassEventCount"], 2);
            assert_eq!(tokens["returnedRecordCount"], 1);
            assert_eq!(tokens["truncated"], Value::Bool(true));
            assert_eq!(tokens["nextSince"], 3);
            assert_eq!(tokens["records"][0]["tokenClasses"]["inputTokens"], 12);
            assert_eq!(tokens["records"][0]["tokenClasses"]["cachedInputTokens"], 3);
            assert!(
                tokens["commercialBoundary"]
                    .as_str()
                    .is_some_and(|boundary| boundary.contains("unaggregated"))
            );

            let cost = cost_inspection_report(run_id, &events, 0, 2).expect("bounded cost report");
            assert_eq!(cost["providerUsageEventCount"], 3);
            assert_eq!(cost["reportedCostEventCount"], 1);
            assert_eq!(cost["returnedRecordCount"], 2);
            assert_eq!(cost["truncated"], Value::Bool(true));
            assert_eq!(cost["nextSince"], 6);
            assert_eq!(cost["records"][0]["providerReportedCost"], 0.045);
            assert_eq!(cost["records"][0]["costDetailsPresent"], Value::Bool(true));
            assert_eq!(cost["records"][1]["providerReportedCost"], Value::Null);

            let serialized = serde_json::to_string(&serde_json::json!({
                "tokens": tokens,
                "cost": cost,
            }))
            .expect("redacted inspection JSON");
            for forbidden in [
                "private-provider-name",
                "private-model-name",
                "private-response-hash",
                "private-cost-detail",
                "responseIDHash",
                "costDetails\"",
            ] {
                assert!(
                    !serialized.contains(forbidden),
                    "inspection output must not expose {forbidden:?}"
                );
            }
        }

        #[test]
        fn token_and_cost_inspection_rejects_invalid_counts_and_limits() {
            let malformed = provider_usage_event(
                3,
                1_000,
                serde_json::json!({
                    "inputTokens": -1,
                    "cost": -0.01,
                }),
            );
            assert!(VerifiedProviderUsage::from_event(&malformed).is_err());
            assert!(validate_usage_inspection_limit(0).is_err());
            assert!(validate_usage_inspection_limit(MAX_USAGE_INSPECTION_RECORDS + 1).is_err());
        }

        #[test]
        fn policy_inspection_is_completed_run_only_bounded_and_redacted() {
            let input = policy_input_projection(&completed_policy_manifest())
                .expect("redacted policy input");
            assert_eq!(input["route"], "native-computer-use");
            assert_eq!(input["requestedModel"], Value::Bool(true));
            assert_eq!(input["selectedSourceCount"], 1);
            assert_eq!(input["selectedPluginID"], "computer_use");
            assert_eq!(input["computerUseEfficiencyCohort"], "semantic");

            let authority = authority_projection(&completed_authority_snapshot())
                .expect("redacted authority projection");
            assert_eq!(authority["authorityCurrent"], Value::Bool(true));
            assert_eq!(authority["computerUse"]["helperReady"], Value::Bool(true));
            assert_eq!(
                authority["computerUse"]["capabilities"]["existingBrowser"],
                Value::Bool(true)
            );

            let events = vec![
                policy_event(
                    2,
                    1_000,
                    "request.session-authority",
                    serde_json::json!({ "admitted": true, "durable": true, "error": "private authority error" }),
                ),
                policy_event(
                    4,
                    1_100,
                    "computer-use.admission",
                    serde_json::json!({
                        "route": "automatic-or-explicit",
                        "requestAllowsComputerUse": true,
                        "effectivePolicyEnabled": true,
                        "masterEnabled": true,
                        "nativeAppsEffective": true,
                        "serverPolicyUsable": true,
                        "helperReady": true,
                        "runtimeDisableLatch": false,
                        "tierEligible": true,
                        "admitted": true,
                        "tier": "private-tier",
                    }),
                ),
                policy_event(
                    6,
                    1_200,
                    "computer-use.application-policy.denied",
                    serde_json::json!({
                        "decision": "forbidden",
                        "targetStableID": "private-app-target",
                    }),
                ),
                policy_event(
                    8,
                    1_300,
                    "computer-use.tool.finished",
                    serde_json::json!({
                        "succeeded": false,
                        "verified": false,
                        "policyDisposition": "material_clarification_required",
                        "policyReasonCode": "ACCESSIBILITY_TARGET_UNAVAILABLE",
                        "tool": "private-tool-name",
                        "activityDetail": "private tool detail",
                    }),
                ),
                policy_event(
                    10,
                    1_400,
                    "computer-use.runtime-authority.failed",
                    serde_json::json!({
                        "actionType": "private-action",
                        "error": "private runtime error",
                    }),
                ),
                policy_event(
                    12,
                    1_500,
                    "chat.event",
                    serde_json::json!({ "private": "ordinary event detail" }),
                ),
            ];
            let report = policy_result_inspection(&events, 0, 2)
                .expect("bounded redacted policy result report");
            assert_eq!(report.event_count, 5);
            assert!(report.truncated);
            assert_eq!(report.next_since, 4);
            assert_eq!(report.records.len(), 2);
            assert_eq!(report.records[0]["result"]["kind"], "session-authority");
            assert_eq!(
                report.records[1]["result"]["kind"],
                "computer-use-admission"
            );

            let all_results = policy_result_inspection(&events, 4, MAX_POLICY_INSPECTION_RECORDS)
                .expect("cursored redacted policy result report");
            assert_eq!(all_results.event_count, 3);
            assert_eq!(all_results.records[0]["result"]["denial"], "forbidden");
            assert_eq!(
                all_results.records[1]["result"]["kind"],
                "computer-use-tool-policy-boundary"
            );
            assert_eq!(
                all_results.records[1]["result"]["reason"],
                "accessibility-target-unavailable"
            );

            let serialized = serde_json::to_string(&serde_json::json!({
                "input": input,
                "authority": authority,
                "results": all_results.records,
            }))
            .expect("redacted policy JSON");
            for forbidden in [
                "private user prompt",
                "private-model-selection",
                "private-resolved-model",
                "private-source",
                "private-connection-hash",
                "private-stable-plugin-id",
                "private-plugin-version",
                "private-auth-state",
                "private-account-hash",
                "private-plan",
                "private-policy-version",
                "private-policy-revision",
                "private-helper-detail",
                "private authority error",
                "private-tier",
                "private-app-target",
                "private-tool-name",
                "private tool detail",
                "private-action",
                "private runtime error",
            ] {
                assert!(
                    !serialized.contains(forbidden),
                    "policy inspection must not expose {forbidden:?}"
                );
            }
        }

        #[test]
        fn policy_inspection_rejects_malformed_fixed_projections() {
            let mut malformed_manifest = completed_policy_manifest();
            malformed_manifest["request"]["route"] = Value::String("unexpected-route".to_string());
            assert!(policy_input_projection(&malformed_manifest).is_err());

            let mut malformed_authority = completed_authority_snapshot();
            malformed_authority["computerUse"]["capabilities"]["nativeApps"] =
                Value::String("true".to_string());
            assert!(authority_projection(&malformed_authority).is_err());

            let incomplete = policy_event(
                3,
                1_000,
                "computer-use.tool.finished",
                serde_json::json!({
                    "succeeded": false,
                    "verified": false,
                    "policyDisposition": "unavailable",
                }),
            );
            assert!(policy_result_inspection(&[incomplete], 0, 1).is_err());
            assert!(validate_policy_inspection_limit(0).is_err());
            assert!(validate_policy_inspection_limit(MAX_POLICY_INSPECTION_RECORDS + 1).is_err());
        }

        #[test]
        fn hex_decoder_rejects_noncanonical_values() {
            assert_eq!(decode_hex("00ff"), Some(vec![0, 255]));
            assert_eq!(decode_hex("00FF"), None);
            assert_eq!(decode_hex("0"), None);
        }
    }
}

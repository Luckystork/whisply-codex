//! Public, hard-separated Whisply diagnostics lanes.
//!
//! Fixture and replay commands deliberately avoid `WHISPLY_HOME`, broker
//! descriptors, Keychain, Usage, providers, tools, and normal persistence.
//! The separately named integration probe validates brokered authority first
//! and refuses to invent a test host when none is bundled.

mod installed_live;

use std::fs;
use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use anyhow::Context;
use clap::Args;
use codex_whisply::ContractReplayRequest;
use codex_whisply::DEFAULT_FIXTURE_SESSION_TTL_MS;
use codex_whisply::DIAGNOSTIC_PROTOCOL_SCHEMA_VERSION;
use codex_whisply::DiagnosticLane;
use codex_whisply::DiagnosticLaunchManifest;
use codex_whisply::DiagnosticTerminalStatus;
use codex_whisply::FixtureAppearance;
use codex_whisply::FixtureCapturePoint;
use codex_whisply::FixtureScenario;
use codex_whisply::ForceUiRequest;
use codex_whisply::NativeBrokerClient;
use codex_whisply::OwnerOnlyTemporaryRoot;
use codex_whisply::PRESENTATION_FIXTURE_MARKER;
use codex_whisply::PRODUCT_NAME;
use codex_whisply::RealIntegrationAccount;
use codex_whisply::RealIntegrationRequest;
use codex_whisply::WHISPLY_RUNTIME_VERSION;
use codex_whisply::release_manifest_sha256_from_environment;
use serde::Serialize;
use serde_json::Value;

const MAX_REPLAY_BUNDLE_BYTES: usize = 512 * 1024;
const DIAGNOSTIC_EXPORT_SCHEMA_VERSION: u16 = 1;

#[derive(Debug, Args)]
pub(crate) struct DiagnosticsCommand {
    #[command(subcommand)]
    action: DiagnosticsSubcommand,
}

#[derive(Debug, clap::Subcommand)]
enum DiagnosticsSubcommand {
    /// Run a local zero-authority report, or a brokered integration-host availability probe.
    Run(DiagnosticsRunArgs),
    /// Export the bounded redacted local diagnostics report to a new file.
    Export(DiagnosticsExportArgs),
    /// Validate a bounded redacted replay bundle in an owner-only temporary root.
    Replay(DiagnosticsReplayArgs),
    /// Request a deterministic zero-authority Swift presentation fixture.
    ForceUi(ForceUiArgs),
    /// Explicitly control the verified installed app's normal live diagnostic path.
    /// This may use the signed-in account's ordinary policy and confirmation flow.
    Live(installed_live::InstalledLiveCommand),
}

#[derive(Debug, Args)]
struct DiagnosticsRunArgs {
    /// Probe a separately named real-integration host after brokered account verification.
    #[arg(long, value_name = "TEST_ID")]
    integration: Option<String>,

    /// `current` or `alias:<approved-name>`; only valid with --integration.
    #[arg(long, default_value = "current", requires = "integration")]
    account: String,

    /// Emit the bounded report as JSON. The normal report is JSON as well; this preserves a stable flag.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct DiagnosticsExportArgs {
    /// New destination file. Existing files are never overwritten.
    #[arg(value_name = "FILE")]
    output: PathBuf,
}

#[derive(Debug, Args)]
struct DiagnosticsReplayArgs {
    /// Bounded, redacted JSON replay bundle. It is only validated in this release.
    #[arg(value_name = "BUNDLE")]
    bundle: PathBuf,
}

#[derive(Debug, Args)]
struct ForceUiArgs {
    /// Registered lowercase fixture surface id (for example `chat.overlay`).
    #[arg(value_name = "SURFACE")]
    surface: String,

    /// Registered lowercase fixture state id.
    #[arg(long, value_name = "STATE")]
    state: String,

    /// Deterministic fixture seed.
    #[arg(long, default_value_t = 0)]
    seed: u64,

    /// Requested fixture content width (1-8192).
    #[arg(long, default_value_t = 430, value_parser = clap::value_parser!(u16).range(1..=8192))]
    width: u16,

    /// Requested fixture content height (1-8192).
    #[arg(long, default_value_t = 760, value_parser = clap::value_parser!(u16).range(1..=8192))]
    height: u16,

    #[arg(long, value_enum, default_value_t = FixtureAppearanceArg::Auto)]
    appearance: FixtureAppearanceArg,

    /// Capture before rendering, after a named fixture event, or at terminal state.
    #[arg(long, value_enum, default_value_t = FixtureCaptureArg::Initial)]
    capture: FixtureCaptureArg,

    /// Registered event id for a named-event capture.
    #[arg(long, value_name = "EVENT")]
    capture_event: Option<String>,
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum FixtureAppearanceArg {
    Light,
    Dark,
    Auto,
}

impl From<FixtureAppearanceArg> for FixtureAppearance {
    fn from(value: FixtureAppearanceArg) -> Self {
        match value {
            FixtureAppearanceArg::Light => Self::Light,
            FixtureAppearanceArg::Dark => Self::Dark,
            FixtureAppearanceArg::Auto => Self::Auto,
        }
    }
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum FixtureCaptureArg {
    Initial,
    Terminal,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DiagnosticReport {
    schema_version: u16,
    product: &'static str,
    runtime_version: &'static str,
    protocol_schema_version: u16,
    local_report: bool,
    release_manifest_present: bool,
    fixture_lane: DiagnosticLaneProjection,
    replay_lane: DiagnosticLaneProjection,
    real_integration: &'static str,
    fixture_host: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DiagnosticLaneProjection {
    authority: &'static str,
    network: &'static str,
    persistence: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DiagnosticTerminalReport {
    schema_version: u16,
    lane: DiagnosticLane,
    status: DiagnosticTerminalStatus,
    message: &'static str,
    presentation_fixture: bool,
    authority: &'static str,
    network: &'static str,
}

pub(crate) fn run(command: DiagnosticsCommand) -> anyhow::Result<()> {
    match command.action {
        DiagnosticsSubcommand::Run(args) => run_diagnostics(args),
        DiagnosticsSubcommand::Export(args) => export_diagnostics(args),
        DiagnosticsSubcommand::Replay(args) => replay_bundle(args),
        DiagnosticsSubcommand::ForceUi(args) => force_ui(args),
        DiagnosticsSubcommand::Live(command) => installed_live::run(command),
    }
}

fn run_diagnostics(args: DiagnosticsRunArgs) -> anyhow::Result<()> {
    if let Some(test_id) = args.integration {
        return probe_real_integration(&test_id, &args.account);
    }
    let _ = args.json;
    print_report(&local_report())
}

fn export_diagnostics(args: DiagnosticsExportArgs) -> anyhow::Result<()> {
    let bytes = serde_json::to_vec_pretty(&local_report())?;
    write_new_private(&args.output, &bytes)?;
    println!(
        "Exported bounded redacted diagnostics to {}.",
        args.output.display()
    );
    Ok(())
}

fn replay_bundle(args: DiagnosticsReplayArgs) -> anyhow::Result<()> {
    let root = OwnerOnlyTemporaryRoot::new("whisply-replay")
        .map_err(|_| anyhow::anyhow!("Unable to create an owner-only replay root."))?;
    let input = root
        .child("input.json")
        .map_err(|_| anyhow::anyhow!("Unable to reserve replay input."))?;
    let output = root
        .child("output.json")
        .map_err(|_| anyhow::anyhow!("Unable to reserve replay output."))?;
    let request = ContractReplayRequest {
        replay_id: "cli.replay".to_string(),
        root: root.path().to_path_buf(),
        input: input.clone(),
        output: output.clone(),
        simulated: true,
        billable: false,
        authority: "none".to_string(),
    };
    request
        .validate()
        .map_err(|_| anyhow::anyhow!("Replay request did not meet the zero-authority contract."))?;
    let bytes = read_safe_bundle(&args.bundle)?;
    let value: Value = serde_json::from_slice(&bytes)
        .context("Replay bundle must be a bounded redacted JSON document.")?;
    validate_redacted_json(&value, 0)?;
    write_new_private(&input, &bytes)?;
    write_new_private(
        &output,
        br#"{"schemaVersion":1,"status":"validated","authority":"none","network":"denied"}"#,
    )?;
    let report = DiagnosticTerminalReport {
        schema_version: DIAGNOSTIC_EXPORT_SCHEMA_VERSION,
        lane: DiagnosticLane::ContractReplay,
        status: DiagnosticTerminalStatus::Unavailable,
        message: "Replay bundle validated in a zero-authority root; no bundled reducer/view replay host is installed.",
        presentation_fixture: false,
        authority: "none",
        network: "denied",
    };
    print_report(&report)?;
    anyhow::bail!("Contract replay execution is unavailable in this installed runtime.");
}

fn force_ui(args: ForceUiArgs) -> anyhow::Result<()> {
    let root = OwnerOnlyTemporaryRoot::new("whisply-fixture")
        .map_err(|_| anyhow::anyhow!("Unable to create an owner-only fixture root."))?;
    let release_manifest_sha256 = release_manifest_sha256_from_environment().ok_or_else(|| {
        anyhow::anyhow!("The verified release manifest is unavailable for this fixture request.")
    })?;
    let capture = match args.capture_event {
        Some(event_id) => FixtureCapturePoint::NamedEvent { event_id },
        None => match args.capture {
            FixtureCaptureArg::Initial => FixtureCapturePoint::Initial,
            FixtureCaptureArg::Terminal => FixtureCapturePoint::Terminal,
        },
    };
    let scenario = FixtureScenario {
        fixture_id: format!("fixture-{}", args.seed),
        surface_id: args.surface,
        state_id: args.state,
        seed: args.seed,
        lane: DiagnosticLane::PresentationFixture,
        fixture_root: root.path().to_path_buf(),
        simulated: true,
        billable: false,
        authority: "none".to_string(),
    };
    let request = ForceUiRequest {
        scenario,
        width: args.width,
        height: args.height,
        appearance: args.appearance.into(),
        capture,
        watermark: PRESENTATION_FIXTURE_MARKER.to_string(),
    };
    request.validate().map_err(|_| {
        anyhow::anyhow!("Fixture request did not meet the zero-authority contract.")
    })?;
    let now_ms = unix_ms()?;
    let manifest = DiagnosticLaunchManifest {
        schema_version: DIAGNOSTIC_PROTOCOL_SCHEMA_VERSION,
        lane: DiagnosticLane::PresentationFixture,
        release_manifest_sha256,
        fixture_root: root.path().to_path_buf(),
        socket_path: root
            .child("fixture.sock")
            .map_err(|_| anyhow::anyhow!("Unable to reserve fixture control socket."))?,
        expires_at_ms: now_ms.saturating_add(DEFAULT_FIXTURE_SESSION_TTL_MS),
    };
    manifest.validate(now_ms).map_err(|_| {
        anyhow::anyhow!("Fixture launch manifest did not meet the zero-authority contract.")
    })?;

    // There is deliberately no shell/process fallback. A verified Swift fixture
    // host must be bundled by the native app and authenticate this manifest over
    // its own IPC boundary. The current runtime has no such host, so fail closed
    // after validating all fixture-only invariants and before any authority is
    // initialized.
    let report = DiagnosticTerminalReport {
        schema_version: DIAGNOSTIC_EXPORT_SCHEMA_VERSION,
        lane: DiagnosticLane::PresentationFixture,
        status: DiagnosticTerminalStatus::Unavailable,
        message: "No verified Swift presentation fixture host is installed; no UI was rendered.",
        presentation_fixture: true,
        authority: "none",
        network: "denied",
    };
    print_report(&report)?;
    anyhow::bail!("Presentation fixture host is unavailable in this installed runtime.");
}

fn probe_real_integration(test_id: &str, account: &str) -> anyhow::Result<()> {
    let account = parse_real_integration_account(account)?;
    RealIntegrationRequest {
        test_id: test_id.to_string(),
        account,
        operation: "availability".to_string(),
    }
    .validate()
    .map_err(|_| anyhow::anyhow!("Invalid real-integration selector."))?;
    let broker = NativeBrokerClient::from_environment()
        .map_err(|_| anyhow::anyhow!("Managed broker descriptors are invalid."))?
        .ok_or_else(|| {
            anyhow::anyhow!("Managed broker is unavailable. Start Whisply from the installed app.")
        })?;
    broker
        .hello()
        .map_err(|_| anyhow::anyhow!("Managed broker rejected the integration probe."))?;
    let status = broker
        .status()
        .map_err(|_| anyhow::anyhow!("Managed broker could not verify the integration account."))?;
    if !status.authenticated || status.account_epoch.is_none() {
        anyhow::bail!("Real integration requires an authenticated managed account.");
    }
    let report = DiagnosticTerminalReport {
        schema_version: DIAGNOSTIC_EXPORT_SCHEMA_VERSION,
        lane: DiagnosticLane::RealIntegration,
        status: DiagnosticTerminalStatus::Unavailable,
        message: "Brokered account authentication verified; entitlement, Usage, and permission checks require a bundled integration host.",
        presentation_fixture: false,
        authority: "managed-native-broker",
        network: "normal-policy-bound",
    };
    print_report(&report)?;
    anyhow::bail!("Real integration execution is unavailable in this installed runtime.");
}

fn parse_real_integration_account(value: &str) -> anyhow::Result<RealIntegrationAccount> {
    if value == "current" {
        return Ok(RealIntegrationAccount::Current);
    }
    let alias = value.strip_prefix("alias:").ok_or_else(|| {
        anyhow::anyhow!("Integration account must be `current` or `alias:<approved-name>`.")
    })?;
    if !valid_identifier(alias) {
        anyhow::bail!("Integration account aliases must be bounded lowercase identifiers.");
    }
    Ok(RealIntegrationAccount::AuthenticatedAlias(
        alias.to_string(),
    ))
}

fn local_report() -> DiagnosticReport {
    DiagnosticReport {
        schema_version: DIAGNOSTIC_EXPORT_SCHEMA_VERSION,
        product: PRODUCT_NAME,
        runtime_version: WHISPLY_RUNTIME_VERSION,
        protocol_schema_version: DIAGNOSTIC_PROTOCOL_SCHEMA_VERSION,
        local_report: true,
        release_manifest_present: release_manifest_sha256_from_environment().is_some(),
        fixture_lane: DiagnosticLaneProjection {
            authority: "none",
            network: "denied",
            persistence: "owner-only-temporary",
        },
        replay_lane: DiagnosticLaneProjection {
            authority: "none",
            network: "denied",
            persistence: "owner-only-temporary",
        },
        real_integration: "requires a separately named brokered host",
        fixture_host: "not-installed",
    }
}

fn print_report<T: Serialize>(report: &T) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(report)?);
    Ok(())
}

fn read_safe_bundle(path: &Path) -> anyhow::Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| "Replay bundle does not exist or cannot be inspected.")?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_REPLAY_BUNDLE_BYTES as u64
    {
        anyhow::bail!("Replay bundle must be a bounded regular file, not a link.");
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(path)?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    Read::by_ref(&mut file)
        .take((MAX_REPLAY_BUNDLE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_REPLAY_BUNDLE_BYTES {
        anyhow::bail!("Replay bundle exceeds its public command size limit.");
    }
    Ok(bytes)
}

fn validate_redacted_json(value: &Value, depth: usize) -> anyhow::Result<()> {
    if depth > 12 {
        anyhow::bail!("Replay bundle exceeds the permitted JSON depth.");
    }
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => Ok(()),
        Value::String(value) if value.len() <= 12 * 1024 && !looks_sensitive(value) => Ok(()),
        Value::String(_) => anyhow::bail!("Replay bundle contains oversized or sensitive text."),
        Value::Array(values) if values.len() <= 256 => values
            .iter()
            .try_for_each(|value| validate_redacted_json(value, depth + 1)),
        Value::Array(_) => anyhow::bail!("Replay bundle contains too many array elements."),
        Value::Object(values) if values.len() <= 64 => {
            values.iter().try_for_each(|(key, value)| {
                if key.len() > 1024 || sensitive_key(key) {
                    anyhow::bail!("Replay bundle contains a disallowed sensitive field.");
                }
                validate_redacted_json(value, depth + 1)
            })
        }
        Value::Object(_) => anyhow::bail!("Replay bundle contains too many object fields."),
    }
}

fn sensitive_key(key: &str) -> bool {
    let normalized = key.to_ascii_lowercase().replace('_', "");
    [
        "token",
        "secret",
        "password",
        "authorization",
        "cookie",
        "credential",
        "callbackurl",
        "bearer",
        "prompt",
        "reasoning",
        "privatefile",
    ]
    .iter()
    .any(|needle| normalized.contains(needle))
}

fn looks_sensitive(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.contains("authorization:")
        || lower.contains("bearer ")
        || lower.contains("access_token")
        || lower.contains("refresh_token")
        || lower.contains("client_secret")
        || value.split_whitespace().any(|word| {
            word.starts_with("sk-")
                || word.starts_with("ghp_")
                || word.starts_with("xox")
                || word.starts_with("eyJ")
        })
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
}

fn unix_ms() -> anyhow::Result<i64> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("System clock is before the Unix epoch.")?
        .as_millis();
    i64::try_from(elapsed).context("System clock cannot be represented in milliseconds.")
}

fn write_new_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Diagnostic export path has no parent directory."))?;
    if parent == Path::new("/") || !parent.is_dir() {
        anyhow::bail!("Diagnostic export directory does not exist.");
    }
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_rejects_credentials_and_hidden_prompts() {
        assert!(validate_redacted_json(&serde_json::json!({ "event": "ready" }), 0).is_ok());
        assert!(
            validate_redacted_json(&serde_json::json!({ "access_token": "private" }), 0).is_err()
        );
        assert!(validate_redacted_json(&serde_json::json!({ "prompt": "private" }), 0).is_err());
    }

    #[test]
    fn integration_selector_rejects_raw_account_identifiers() {
        assert!(matches!(
            parse_real_integration_account("current"),
            Ok(RealIntegrationAccount::Current)
        ));
        assert!(parse_real_integration_account("alias:test-tenant").is_ok());
        assert!(parse_real_integration_account("user@example.com").is_err());
    }

    #[test]
    fn local_report_has_no_account_or_usage_projection() -> anyhow::Result<()> {
        let encoded = serde_json::to_string(&local_report())?;
        assert!(!encoded.contains("accountEpoch"));
        assert!(!encoded.contains("usage"));
        assert!(encoded.contains("not-installed"));
        Ok(())
    }
}

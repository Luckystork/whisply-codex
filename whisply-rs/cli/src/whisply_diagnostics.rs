//! Public, hard-separated Whisply diagnostics lanes.
//!
//! Fixture and replay commands deliberately avoid `WHISPLY_HOME`, broker
//! descriptors, Keychain, Usage, providers, tools, and normal persistence.
//! The separately named integration probe validates brokered authority first
//! and refuses to invent a test host when none is bundled.

mod installed_live;
mod scenario_registry;
#[cfg(test)]
mod scenario_registry_tests;

use std::fs;
use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use anyhow::Context;
use clap::Args;
use codex_whisply::ContractReplayRequest;
use codex_whisply::DEFAULT_FIXTURE_SESSION_TTL_MS;
use codex_whisply::DIAGNOSTIC_PROTOCOL_SCHEMA_VERSION;
use codex_whisply::DiagnosticFixtureReplayControl;
use codex_whisply::DiagnosticLane;
use codex_whisply::DiagnosticLaunchManifest;
use codex_whisply::DiagnosticTerminalStatus;
use codex_whisply::FixtureAppearance;
use codex_whisply::FixtureCapturePoint;
use codex_whisply::FixtureDisplay;
use codex_whisply::FixtureFontScale;
use codex_whisply::FixtureLayoutDirection;
use codex_whisply::FixturePresentation;
use codex_whisply::FixtureScenario;
use codex_whisply::FixtureTheme;
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
    /// Validate a bounded redacted replay bundle and execute only its rebuilt catalog fixture.
    Replay(DiagnosticsReplayArgs),
    /// Create one owner-only redacted catalog replay bundle for later standalone replay.
    ExportCatalogReplay(DiagnosticCatalogReplayExportArgs),
    /// Request a deterministic zero-authority Swift presentation fixture.
    ForceUi(ForceUiArgs),
    /// Verify one retained zero-authority fixture archive without launching an app.
    VerifyFixtureArchive(DiagnosticFixtureArchiveVerifyArgs),
    /// Verify one retained canonical XCTest fixture evidence directory without executing it.
    VerifyFixtureBundle(DiagnosticFixtureBundleVerifyArgs),
    /// Export a bounded redacted projection of one verified canonical XCTest fixture evidence directory.
    ExportFixtureBundle(DiagnosticFixtureBundleExportArgs),
    /// Compare two already-verified canonical XCTest fixture evidence directories without exposing their contents.
    CompareFixtureBundles(DiagnosticFixtureBundleCompareArgs),
    /// Read the immutable deterministic scenario catalog; this never executes a fixture.
    Scenario(scenario_registry::DiagnosticScenarioCommand),
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
    /// Bounded redacted catalog-replay bundle, pinned to this installed
    /// scenario registry and schema. Its only mutable content is the closed
    /// replay-control sequence; the fixture request is rebuilt from the
    /// immutable catalog after validation.
    #[arg(value_name = "BUNDLE")]
    bundle: PathBuf,

    /// Existing owner-only directory that receives a new retained replay
    /// fixture archive. The replay never writes into the source bundle.
    #[arg(long, value_name = "DIRECTORY", value_hint = clap::ValueHint::DirPath)]
    output_dir: PathBuf,
}

/// Produces the exact redacted bundle accepted by diagnostics replay. The
/// caller can provide only one registered scenario and closed reducer controls;
/// hashes, versions, and all fixture state come from the compiled catalog.
#[derive(Debug, Args)]
struct DiagnosticCatalogReplayExportArgs {
    /// Registered deterministic scenario id.
    #[arg(value_name = "SCENARIO")]
    scenario: String,

    /// Ordered replay control: pause, resume, step, seek:N, or
    /// speed:half|normal|double|quadruple.
    #[arg(long = "control", value_name = "CONTROL")]
    controls: Vec<scenario_registry::DiagnosticScenarioReplayControlArg>,

    /// New JSON bundle inside an existing owner-only directory.
    #[arg(long, value_name = "FILE", value_hint = clap::ValueHint::FilePath)]
    output: PathBuf,
}

/// Read and verify one archive previously retained by the native force-ui
/// controller. The archive must already be an owner-only directory; this
/// command never launches a fixture or invokes an ordinary app process.
#[derive(Debug, Args)]
struct DiagnosticFixtureArchiveVerifyArgs {
    /// Existing owner-only fixture archive emitted by `diagnostics force-ui` or `diagnostics scenario execute`.
    #[arg(value_name = "ARCHIVE", value_hint = clap::ValueHint::DirPath)]
    archive: PathBuf,
}

/// Read and verify one completed legacy XCTest fixture evidence directory.
/// The directory must already be owner-only and tied to the immutable
/// compiled scenario catalog; verification never launches, replays, or
/// attaches to a fixture or ordinary Whisply app.
#[derive(Debug, Args)]
struct DiagnosticFixtureBundleVerifyArgs {
    /// Existing owner-only canonical XCTest fixture evidence directory.
    #[arg(value_name = "BUNDLE", value_hint = clap::ValueHint::DirPath)]
    bundle: PathBuf,
}

/// Export an already-completed canonical XCTest fixture evidence directory.
/// The source bundle is strictly verified first; the new archive contains
/// only a bounded redacted summary and integrity manifest, never raw event,
/// log, screenshot, accessibility, layout, icon, or visible-copy evidence.
#[derive(Debug, Args)]
struct DiagnosticFixtureBundleExportArgs {
    /// Existing owner-only canonical XCTest fixture evidence directory.
    #[arg(value_name = "BUNDLE", value_hint = clap::ValueHint::DirPath)]
    bundle: PathBuf,

    /// New `.zip` file inside an existing owner-only directory. Existing files are never overwritten.
    #[arg(value_name = "OUTPUT", value_hint = clap::ValueHint::FilePath)]
    output: PathBuf,
}

/// Compare two completed legacy XCTest fixture evidence directories. Both
/// directories must independently pass strict catalog-attested verification;
/// the report exposes only a fixed set of changed categories and redacted
/// evidence summaries. It never launches, replays, attaches to, or commands
/// a fixture or ordinary Whisply app.
#[derive(Debug, Args)]
struct DiagnosticFixtureBundleCompareArgs {
    /// Existing owner-only canonical XCTest fixture evidence directory used as the baseline.
    #[arg(value_name = "BASELINE", value_hint = clap::ValueHint::DirPath)]
    baseline: PathBuf,

    /// Existing owner-only canonical XCTest fixture evidence directory to compare.
    #[arg(value_name = "CANDIDATE", value_hint = clap::ValueHint::DirPath)]
    candidate: PathBuf,
}

#[derive(Debug, Args)]
pub(crate) struct ForceUiArgs {
    /// Registered lowercase fixture surface id (for example `chat.overlay`).
    #[arg(value_name = "SURFACE")]
    surface: String,

    /// Registered lowercase fixture state id.
    #[arg(long, value_name = "STATE")]
    state: String,

    /// Deterministic fixture seed.
    #[arg(long, default_value_t = 0)]
    seed: u64,

    /// Requested fixture content width (1-8192; preferences/general, preferences/usage, preferences/account, and subscription.gate/starter need at least 880).
    #[arg(long, default_value_t = 430, value_parser = clap::value_parser!(u16).range(1..=8192))]
    width: u16,

    /// Requested fixture content height (1-8192; preferences/general, preferences/usage, and preferences/account need at least 600; subscription.gate/starter needs at least 760).
    #[arg(long, default_value_t = 760, value_parser = clap::value_parser!(u16).range(1..=8192))]
    height: u16,

    #[arg(long, value_enum, default_value_t = FixtureAppearanceArg::Auto)]
    appearance: FixtureAppearanceArg,

    /// Built-in Whisply theme for the isolated fixture. Custom and saved user
    /// themes are intentionally unavailable to diagnostics.
    #[arg(long, value_enum, default_value_t = FixtureThemeArg::System)]
    theme: FixtureThemeArg,

    /// Fixture-only locale identifier (for example `en_US` or `fr_CA`).
    /// This is passed only to the isolated native fixture process.
    #[arg(long, default_value = "en_US", value_name = "LOCALE")]
    locale: String,

    /// Fixture-only layout direction. This changes only the isolated SwiftUI
    /// fixture hierarchy; it never changes the person's macOS language or
    /// layout preferences.
    #[arg(long, value_enum, default_value_t = FixtureLayoutDirectionArg::LeftToRight)]
    layout_direction: FixtureLayoutDirectionArg,

    /// Fixture-only bounded Dynamic Type scale. This changes only the
    /// isolated SwiftUI fixture hierarchy and never reads or writes the
    /// person's text-size preference.
    #[arg(long, value_enum, default_value_t = FixtureFontScaleArg::Standard)]
    font_scale: FixtureFontScaleArg,

    /// Fixture-only motion rendering. `reduced` disables reviewed optional
    /// fixture motion; this never changes the person's macOS setting.
    #[arg(long, value_enum, default_value_t = FixtureMotionArg::Reduced)]
    motion: FixtureMotionArg,

    /// Fixture-only contrast rendering. `increased` is propagated only to
    /// reviewed fixture components and never changes the person's macOS setting.
    #[arg(long, value_enum, default_value_t = FixtureContrastArg::Standard)]
    contrast: FixtureContrastArg,

    /// Optional attached display ID for the isolated fixture window. This
    /// selects a temporary fixture window only; it never changes displays or
    /// moves ordinary Whisply windows.
    #[arg(long, value_name = "DISPLAY_ID", value_parser = clap::value_parser!(u32).range(1..))]
    display: Option<u32>,

    /// Required backing scale for the selected fixture display (for example
    /// `1.0`, `1.5`, or `2.0`). The fixture fails closed if that display does
    /// not have the requested actual scale.
    #[arg(long, value_name = "SCALE", requires = "display")]
    backing_scale: Option<FixtureBackingScaleArg>,

    /// Capture before rendering, after a named fixture event, or at terminal state.
    #[arg(long, value_enum, default_value_t = FixtureCaptureArg::Initial)]
    capture: FixtureCaptureArg,

    /// Registered event id for a named-event capture. Direct static fixtures
    /// accept only `chat-overlay-streaming` for the inert Chat Overlay stream.
    #[arg(long, value_name = "EVENT")]
    capture_event: Option<String>,

    /// Existing owner-only directory that receives a new retained fixture archive.
    #[arg(long, value_name = "DIRECTORY", value_hint = clap::ValueHint::DirPath)]
    output_dir: PathBuf,
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
enum FixtureThemeArg {
    System,
    Light,
    Dark,
    Midnight,
    Graphite,
    Meadow,
    Sunset,
    Dawn,
    Linen,
    Rainbow,
    Galaxy,
    Aurora,
    Ocean,
    Ember,
    Sol,
}

impl From<FixtureThemeArg> for FixtureTheme {
    fn from(value: FixtureThemeArg) -> Self {
        match value {
            FixtureThemeArg::System => Self::System,
            FixtureThemeArg::Light => Self::Light,
            FixtureThemeArg::Dark => Self::Dark,
            FixtureThemeArg::Midnight => Self::Midnight,
            FixtureThemeArg::Graphite => Self::Graphite,
            FixtureThemeArg::Meadow => Self::Meadow,
            FixtureThemeArg::Sunset => Self::Sunset,
            FixtureThemeArg::Dawn => Self::Dawn,
            FixtureThemeArg::Linen => Self::Linen,
            FixtureThemeArg::Rainbow => Self::Rainbow,
            FixtureThemeArg::Galaxy => Self::Galaxy,
            FixtureThemeArg::Aurora => Self::Aurora,
            FixtureThemeArg::Ocean => Self::Ocean,
            FixtureThemeArg::Ember => Self::Ember,
            FixtureThemeArg::Sol => Self::Sol,
        }
    }
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum FixtureLayoutDirectionArg {
    LeftToRight,
    RightToLeft,
}

impl From<FixtureLayoutDirectionArg> for FixtureLayoutDirection {
    fn from(value: FixtureLayoutDirectionArg) -> Self {
        match value {
            FixtureLayoutDirectionArg::LeftToRight => Self::LeftToRight,
            FixtureLayoutDirectionArg::RightToLeft => Self::RightToLeft,
        }
    }
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum FixtureFontScaleArg {
    Small,
    Standard,
    Large,
}

impl From<FixtureFontScaleArg> for FixtureFontScale {
    fn from(value: FixtureFontScaleArg) -> Self {
        match value {
            FixtureFontScaleArg::Small => Self::Small,
            FixtureFontScaleArg::Standard => Self::Standard,
            FixtureFontScaleArg::Large => Self::Large,
        }
    }
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum FixtureMotionArg {
    Reduced,
    Full,
}

impl FixtureMotionArg {
    fn reduce_motion(self) -> bool {
        matches!(self, Self::Reduced)
    }
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum FixtureContrastArg {
    Standard,
    Increased,
}

impl FixtureContrastArg {
    fn increased_contrast(self) -> bool {
        matches!(self, Self::Increased)
    }
}

#[derive(Clone, Copy, Debug)]
struct FixtureBackingScaleArg(u16);

impl FromStr for FixtureBackingScaleArg {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let scale = value
            .parse::<f64>()
            .map_err(|_| "backing scale must be a number from 0.5 through 8.0".to_string())?;
        if !scale.is_finite() || !(0.5..=8.0).contains(&scale) {
            return Err("backing scale must be a number from 0.5 through 8.0".to_string());
        }
        let milli = (scale * 1_000.0).round();
        if (scale * 1_000.0 - milli).abs() > 0.000_001 {
            return Err("backing scale may have at most three decimal places".to_string());
        }
        let milli = u16::try_from(milli as u32)
            .map_err(|_| "backing scale must be a number from 0.5 through 8.0".to_string())?;
        Ok(Self(milli))
    }
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum FixtureCaptureArg {
    Initial,
    NamedEvent,
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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CatalogReplayBundleExportReport {
    schema_version: u16,
    protocol: &'static str,
    scenario: String,
    control_count: usize,
    output_path: String,
    authority: &'static str,
    network: &'static str,
}

pub(crate) fn run(command: DiagnosticsCommand) -> anyhow::Result<()> {
    match command.action {
        DiagnosticsSubcommand::Run(args) => run_diagnostics(args),
        DiagnosticsSubcommand::Export(args) => export_diagnostics(args),
        DiagnosticsSubcommand::Replay(args) => replay_bundle(args),
        DiagnosticsSubcommand::ExportCatalogReplay(args) => export_catalog_replay_bundle(args),
        DiagnosticsSubcommand::ForceUi(args) => force_ui(args),
        DiagnosticsSubcommand::VerifyFixtureArchive(args) => verify_fixture_archive(args),
        DiagnosticsSubcommand::VerifyFixtureBundle(args) => verify_fixture_bundle(args),
        DiagnosticsSubcommand::ExportFixtureBundle(args) => export_fixture_bundle(args),
        DiagnosticsSubcommand::CompareFixtureBundles(args) => compare_fixture_bundles(args),
        DiagnosticsSubcommand::Scenario(command) => run_scenario(command),
        DiagnosticsSubcommand::Live(command) => installed_live::run(command),
    }
}

fn run_scenario(command: scenario_registry::DiagnosticScenarioCommand) -> anyhow::Result<()> {
    match command.action {
        scenario_registry::DiagnosticScenarioAction::Execute(args) => {
            let replay_controls = args.parsed_replay_controls()?;
            let capture = if replay_controls.is_empty() {
                Some(args.capture_point()?)
            } else {
                None
            };
            execute_catalog_fixture(&args.scenario, capture, &replay_controls, &args.output_dir)
        }
        action => print_report(&scenario_registry::run(
            scenario_registry::DiagnosticScenarioCommand { action },
        )?),
    }
}

#[cfg(target_os = "macos")]
fn execute_catalog_fixture(
    scenario_id: &str,
    capture: Option<FixtureCapturePoint>,
    replay_controls: &[DiagnosticFixtureReplayControl],
    output_dir: &Path,
) -> anyhow::Result<()> {
    let catalog = if replay_controls.is_empty() {
        scenario_registry::catalog_fixture_execution(scenario_id)?
    } else {
        scenario_registry::catalog_fixture_execution_with_replay(scenario_id, replay_controls)?
    };
    native_fixture::run_catalog(catalog, capture, output_dir)
}

#[cfg(not(target_os = "macos"))]
fn execute_catalog_fixture(
    scenario_id: &str,
    capture: Option<FixtureCapturePoint>,
    replay_controls: &[DiagnosticFixtureReplayControl],
    output_dir: &Path,
) -> anyhow::Result<()> {
    let _ = scenario_id;
    let _ = capture;
    let _ = replay_controls;
    let _ = output_dir;
    anyhow::bail!("Catalog-attested presentation fixtures are available only on macOS.");
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
    let catalog = validate_catalog_replay_bundle(&args.bundle)?;
    execute_catalog_replay(catalog, &args.output_dir)
}

fn export_catalog_replay_bundle(args: DiagnosticCatalogReplayExportArgs) -> anyhow::Result<()> {
    let bytes = scenario_registry::catalog_redacted_replay_bundle(&args.scenario, &args.controls)?;
    // Keep the persisted wire shape coupled to the exact installed consumer,
    // even if the pure catalog producer is later refactored.
    let _ = scenario_registry::catalog_fixture_execution_from_redacted_replay_bundle(&bytes)?;
    let output = write_new_owner_only_catalog_replay_bundle(&args.output, &bytes)?;
    print_report(&CatalogReplayBundleExportReport {
        schema_version: scenario_registry::CATALOG_REPLAY_BUNDLE_SCHEMA_VERSION,
        protocol: scenario_registry::CATALOG_REPLAY_BUNDLE_PROTOCOL,
        scenario: args.scenario,
        control_count: args.controls.len(),
        output_path: output.display().to_string(),
        authority: "none",
        network: "denied",
    })
}

/// Read a bounded replay description only long enough to prove it matches the
/// compiled catalog. The retained fixture request is regenerated after this
/// returns, so no caller-authored view, authority, account, endpoint, or
/// fixture state crosses into the native host.
fn validate_catalog_replay_bundle(
    bundle: &Path,
) -> anyhow::Result<scenario_registry::CatalogFixtureExecution> {
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
    let bytes = read_safe_bundle(bundle)?;
    let value: Value = serde_json::from_slice(&bytes)
        .context("Replay bundle must be a bounded redacted JSON document.")?;
    validate_redacted_json(&value, 0)?;
    let catalog = scenario_registry::catalog_fixture_execution_from_redacted_replay_bundle(&bytes)?;
    write_new_private(&input, &bytes)?;
    write_new_private(
        &output,
        br#"{"schemaVersion":1,"status":"validated","authority":"none","network":"denied"}"#,
    )?;
    Ok(catalog)
}

#[cfg(target_os = "macos")]
fn execute_catalog_replay(
    catalog: scenario_registry::CatalogFixtureExecution,
    output_dir: &Path,
) -> anyhow::Result<()> {
    native_fixture::run_catalog(catalog, None, output_dir)
}

#[cfg(not(target_os = "macos"))]
fn execute_catalog_replay(
    catalog: scenario_registry::CatalogFixtureExecution,
    output_dir: &Path,
) -> anyhow::Result<()> {
    let _ = catalog;
    let _ = output_dir;
    anyhow::bail!("Catalog-attested replay fixtures are available only on macOS.");
}

#[cfg(target_os = "macos")]
fn verify_fixture_archive(args: DiagnosticFixtureArchiveVerifyArgs) -> anyhow::Result<()> {
    native_fixture::verify_retained_fixture_archive(&args.archive)
}

#[cfg(not(target_os = "macos"))]
fn verify_fixture_archive(args: DiagnosticFixtureArchiveVerifyArgs) -> anyhow::Result<()> {
    let _ = args;
    anyhow::bail!("Retained native fixture archives are available only on macOS.");
}

#[cfg(target_os = "macos")]
fn verify_fixture_bundle(args: DiagnosticFixtureBundleVerifyArgs) -> anyhow::Result<()> {
    native_fixture::verify_retained_fixture_bundle(&args.bundle)
}

#[cfg(not(target_os = "macos"))]
fn verify_fixture_bundle(args: DiagnosticFixtureBundleVerifyArgs) -> anyhow::Result<()> {
    let _ = args;
    anyhow::bail!("Retained XCTest fixture evidence directories are available only on macOS.");
}

#[cfg(target_os = "macos")]
fn export_fixture_bundle(args: DiagnosticFixtureBundleExportArgs) -> anyhow::Result<()> {
    native_fixture::export_retained_fixture_bundle(&args.bundle, &args.output)
}

#[cfg(not(target_os = "macos"))]
fn export_fixture_bundle(args: DiagnosticFixtureBundleExportArgs) -> anyhow::Result<()> {
    let _ = args;
    anyhow::bail!("Retained XCTest fixture evidence export is available only on macOS.");
}

#[cfg(target_os = "macos")]
fn compare_fixture_bundles(args: DiagnosticFixtureBundleCompareArgs) -> anyhow::Result<()> {
    native_fixture::compare_retained_fixture_bundles(&args.baseline, &args.candidate)
}

#[cfg(not(target_os = "macos"))]
fn compare_fixture_bundles(args: DiagnosticFixtureBundleCompareArgs) -> anyhow::Result<()> {
    let _ = args;
    anyhow::bail!("Retained XCTest fixture evidence comparison is available only on macOS.");
}

#[cfg(target_os = "macos")]
pub(crate) fn force_ui(args: ForceUiArgs) -> anyhow::Result<()> {
    native_fixture::run(args)
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn force_ui(args: ForceUiArgs) -> anyhow::Result<()> {
    let _ = args;
    anyhow::bail!("Presentation fixtures are available only on macOS.");
}

/// macOS-only controller for the deliberately isolated Swift fixture host.
///
/// The host is the exact installed app binary, never a PATH lookup or shell
/// fallback. It must prove possession of an inherited one-time capability on
/// an owner-only Unix socket before it receives a request. The capability
/// itself never crosses argv, environment, filesystem, output, or logging.
#[cfg(target_os = "macos")]
mod native_fixture {
    use std::collections::BTreeMap;
    use std::collections::BTreeSet;
    use std::fs;
    use std::fs::File;
    use std::fs::OpenOptions;
    use std::io;
    use std::io::ErrorKind;
    use std::io::Read;
    use std::io::Write;
    use std::os::fd::AsRawFd;
    use std::os::fd::FromRawFd;
    use std::os::fd::OwnedFd;
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;
    use std::os::unix::net::UnixStream;
    use std::os::unix::process::CommandExt;
    use std::path::Path;
    use std::path::PathBuf;
    use std::process::Child;
    use std::process::Command;
    use std::thread;
    use std::time::Duration;
    use std::time::Instant;

    use anyhow::Context;
    use codex_whisply::fixture_capture_for_replay;
    use hmac::Hmac;
    use hmac::Mac as _;
    use serde::Deserialize;
    use sha2::Digest as _;
    use sha2::Sha256;
    use uuid::Uuid;
    use zip::ZipArchive;
    use zip::ZipWriter;
    use zip::write::SimpleFileOptions;

    use super::DEFAULT_FIXTURE_SESSION_TTL_MS;
    use super::DIAGNOSTIC_EXPORT_SCHEMA_VERSION;
    use super::DIAGNOSTIC_PROTOCOL_SCHEMA_VERSION;
    use super::DiagnosticLane;
    use super::DiagnosticLaunchManifest;
    use super::DiagnosticTerminalStatus;
    use super::FixtureAppearance;
    use super::FixtureCaptureArg;
    use super::FixtureCapturePoint;
    use super::FixtureDisplay;
    use super::FixtureFontScale;
    use super::FixtureLayoutDirection;
    use super::FixturePresentation;
    use super::FixtureScenario;
    use super::FixtureTheme;
    use super::ForceUiArgs;
    use super::ForceUiRequest;
    use super::OwnerOnlyTemporaryRoot;
    use super::PRESENTATION_FIXTURE_MARKER;
    use super::print_report;
    use super::release_manifest_sha256_from_environment;
    use super::scenario_registry::CatalogFixtureEvidenceContract;
    use super::scenario_registry::CatalogFixtureExecution;
    use super::unix_ms;
    use super::write_new_private;

    const INSTALLED_APP_PATH: &str = "/Applications/Whisply.app";
    const INSTALLED_APP_BINARY_RELATIVE_PATH: &str = "Contents/MacOS/Whisply";
    const RUNTIME_MANIFEST_RELATIVE_PATH: &str =
        "Contents/Resources/Runtime/whisply-runtime-release-manifest.json";
    const FIXTURE_LAUNCH_FILE: &str = "fixture-launch.json";
    const FIXTURE_CAPABILITY_FD: i32 = 57;
    const MAX_MANIFEST_BYTES: u64 = 64 * 1_024 * 1_024;
    const MAX_CAPTURE_BYTES: u64 = 32 * 1_024 * 1_024;
    // A catalog execution carries bounded plan/timeline/event/selector
    // projections. Keep the launch manifest and host response small, while
    // allowing every checked-in zero-authority catalog fixture to cross the
    // owner-only socket in one bounded request frame.
    const MAX_LAUNCH_MANIFEST_BYTES: usize = 16 * 1_024;
    const MAX_FIXTURE_REQUEST_BYTES: usize = 512 * 1_024;
    const MAX_FIXTURE_RESPONSE_BYTES: usize = 16 * 1_024;
    const SELF_CAPTURE_METHOD: &str = "appkit-content-view-cache";
    const SELF_CAPTURE_NOT_USED: &str = "not_used";
    const EXPORTED_CAPTURE_FILE: &str = "fixture-capture.png";
    const EXPORTED_ARTIFACT_MANIFEST_FILE: &str = "fixture-artifacts.json";
    const EXPORTED_HASH_MANIFEST_FILE: &str = "hashes.sha256";
    const LEGACY_FIXTURE_BUNDLE_PROTOCOL: &str = "whisply.diagnostics.v1";
    const LEGACY_FIXTURE_BUNDLE_SCHEMA_VERSION: u64 = 1;
    const MAX_LEGACY_FIXTURE_BUNDLE_BYTES: u64 = 64 * 1_024 * 1_024;
    const MAX_LEGACY_FIXTURE_TEXT_BYTES: u64 = 8 * 1_024 * 1_024;
    const REDACTED_FIXTURE_BUNDLE_EXPORT_SCHEMA_VERSION: u16 = 1;
    const MAX_REDACTED_FIXTURE_BUNDLE_EXPORT_BYTES: u64 = 512 * 1_024;
    const REDACTED_FIXTURE_BUNDLE_EXPORT_MANIFEST_FILE: &str = "export-manifest.json";
    const REDACTED_FIXTURE_BUNDLE_EXPORT_SUMMARY_FILE: &str = "summary.json";
    const LEGACY_FIXTURE_EVIDENCE_DIRECTORIES: [&str; 5] = [
        "screenshots",
        "accessibility",
        "layout",
        "visible-copy",
        "icons",
    ];
    const FIXTURE_UI_METADATA_SCHEMA_VERSION: u16 = 7;
    const FIXTURE_HOST_ACCESSIBILITY_IDENTIFIER: &str = "whisply.presentation.fixture.host";
    const MAX_EXPORT_DIRECTORY_ATTEMPTS: usize = 16;
    const HOST_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
    const HOST_IO_TIMEOUT: Duration = Duration::from_secs(10);
    const HOST_EXIT_TIMEOUT: Duration = Duration::from_secs(10);

    type HmacSha256 = Hmac<Sha256>;

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct FixtureHostResponse {
        schema_version: u16,
        fixture_id: String,
        terminal_status: DiagnosticTerminalStatus,
        presentation_fixture: bool,
        authority: String,
        network: String,
        capture_method: String,
        computer_use: String,
        screen_recording: String,
        capture_path: String,
        capture_sha256: String,
        ui_metadata: FixtureUiMetadata,
    }

    /// The fixture host is the authority for actual rendered state. The CLI
    /// nevertheless validates every deterministic field against its request
    /// before retaining it, so a stale or widened host response cannot become
    /// an accepted diagnostic artifact.
    #[derive(Debug, Clone, Deserialize, serde::Serialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct FixtureUiMetadata {
        schema_version: u16,
        requested: FixtureRequestedUiMetadata,
        effective: FixtureEffectiveUiMetadata,
    }

    #[derive(Debug, Clone, Deserialize, serde::Serialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct FixtureRequestedUiMetadata {
        fixture_id: String,
        surface_id: String,
        state_id: String,
        seed: u64,
        content_width_points: u16,
        content_height_points: u16,
        appearance: String,
        theme: String,
        locale: String,
        layout_direction: String,
        font_scale: String,
        reduce_motion: bool,
        increased_contrast: bool,
        display_identifier: Option<u32>,
        backing_scale_milli: Option<u16>,
        capture_kind: String,
        capture_event_id: Option<String>,
    }

    #[derive(Debug, Clone, Deserialize, serde::Serialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct FixtureEffectiveUiMetadata {
        fixture_id: String,
        surface_id: String,
        state_id: String,
        content_frame_points: FixtureContentFrame,
        capture_pixel_width: u32,
        capture_pixel_height: u32,
        appearance: String,
        theme: String,
        locale: String,
        layout_direction: String,
        font_scale: String,
        reduce_motion: bool,
        increased_contrast: bool,
        applied_presentation: FixtureAppliedPresentationMetadata,
        display_available: bool,
        display_identifier: Option<u32>,
        backing_scale_factor: f64,
        window_is_key: bool,
        window_is_main: bool,
        application_is_active: bool,
        accessibility_identifier: String,
        accessibility_label: String,
        window_metadata: FixtureWindowMetadata,
        rendered_icon_registry: FixtureRenderedIconRegistry,
    }

    /// The fixture's bounded render policy is attested separately from the
    /// observed host accessibility flags. This keeps a successful fixture from
    /// claiming it changed the person's macOS preferences.
    #[derive(Debug, Clone, Deserialize, serde::Serialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct FixtureAppliedPresentationMetadata {
        layout_direction: String,
        font_scale: String,
        dynamic_type_size: String,
        reduce_motion: bool,
        increased_contrast: bool,
        contrast_gain: f64,
    }

    /// A compact normal form for all icons rendered by a reviewed fixture.
    /// `rendered_icon_ids` preserves every occurrence in visual order while
    /// `registry` carries each repeated asset/hash/tint/size/fallback tuple
    /// exactly once, keeping a large typed fixture response bounded.
    #[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct FixtureRenderedIconRegistry {
        registry: Vec<FixtureRenderedIconRegistryEntry>,
        rendered_icon_ids: Vec<String>,
    }

    #[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct FixtureRenderedIconRegistryEntry {
        registry_id: String,
        asset: String,
        asset_sha256: String,
        tint: String,
        size_points: FixtureIconSize,
        fallback_state: String,
    }

    #[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct FixtureIconSize {
        width: f64,
        height: f64,
    }

    #[derive(Debug, Clone, Deserialize, serde::Serialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct FixtureContentFrame {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    }

    #[derive(Debug, Clone, Deserialize, serde::Serialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct FixtureWindowMetadata {
        level: FixtureWindowLevel,
        frame_points: FixtureWindowFrame,
        display_available: bool,
        display_identifier: Option<u32>,
        display_frame_points: Option<FixtureWindowFrame>,
        display_visible_frame_points: Option<FixtureWindowFrame>,
        collection_behavior: FixtureWindowCollectionBehavior,
        is_on_active_space: bool,
        is_visible: bool,
    }

    #[derive(Debug, Clone, Deserialize, serde::Serialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct FixtureWindowLevel {
        semantic: String,
        raw_value: i64,
    }

    #[derive(Debug, Clone, Deserialize, serde::Serialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct FixtureWindowCollectionBehavior {
        raw_value: u64,
        can_join_all_spaces: bool,
        move_to_active_space: bool,
        stationary: bool,
        full_screen_auxiliary: bool,
        ignores_cycle: bool,
    }

    #[derive(Debug, Clone, Deserialize, serde::Serialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct FixtureWindowFrame {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    }

    #[derive(Debug, serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct FixtureTerminalReport {
        schema_version: u16,
        lane: DiagnosticLane,
        status: DiagnosticTerminalStatus,
        message: String,
        presentation_fixture: bool,
        authority: &'static str,
        network: &'static str,
        capture_method: String,
        computer_use: String,
        screen_recording: String,
        screenshot_path: String,
        artifact_paths: Vec<String>,
    }

    #[derive(Debug)]
    struct ValidatedFixtureCapture {
        source_path: PathBuf,
        sha256: String,
    }

    #[derive(Debug)]
    struct RetainedFixtureArtifacts {
        screenshot_path: String,
        artifact_paths: Vec<String>,
    }

    #[derive(Debug, Deserialize, serde::Serialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct FixtureArtifactManifest {
        schema_version: u16,
        fixture_id: String,
        screenshot_path: String,
        artifact_paths: Vec<String>,
        screenshot_sha256: String,
        capture_method: String,
        computer_use: String,
        screen_recording: String,
        presentation_fixture: bool,
        authority: String,
        network: String,
        ui_metadata: FixtureUiMetadata,
    }

    #[derive(Debug)]
    struct VerifiedFixtureArchive {
        archive_path: String,
        fixture_id: String,
        screenshot_path: String,
        screenshot_sha256: String,
    }

    #[derive(Debug, serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct FixtureArchiveVerificationReport {
        schema_version: u16,
        lane: DiagnosticLane,
        status: DiagnosticTerminalStatus,
        message: String,
        archive_path: String,
        fixture_id: String,
        screenshot_path: String,
        screenshot_sha256: String,
        presentation_fixture: bool,
        authority: &'static str,
        network: &'static str,
        capture_method: &'static str,
        computer_use: &'static str,
        screen_recording: &'static str,
    }

    #[derive(Debug)]
    struct FixtureBundleFile {
        relative: String,
        path: PathBuf,
        bytes: u64,
    }

    #[derive(Debug)]
    struct VerifiedFixtureBundle {
        bundle_path: String,
        scenario_id: String,
        scenario_version: u32,
        event_count: usize,
        file_count: usize,
        total_bytes: u64,
        hash_manifest_sha256: String,
        logical_snapshot: FixtureBundleLogicalSnapshot,
    }

    /// Normalized, hash-only form matching the legacy Python comparison
    /// semantics. The values deliberately never retain or return source
    /// metadata, rendered copy, screenshots, or event titles.
    #[derive(Debug)]
    struct FixtureBundleLogicalSnapshot {
        run_configuration_sha256: String,
        result_sha256: String,
        authority_sha256: String,
        animation_sha256: String,
        event_transcript_sha256: String,
        structured_log_sha256: String,
        evidence_directory_sha256: BTreeMap<&'static str, String>,
    }

    #[derive(Debug)]
    struct FixtureBundleComparison {
        baseline: VerifiedFixtureBundle,
        candidate: VerifiedFixtureBundle,
        equivalent: bool,
        changed: Vec<FixtureBundleComparisonChange>,
    }

    #[derive(Debug, PartialEq, Eq, serde::Serialize)]
    #[serde(rename_all = "kebab-case")]
    enum FixtureBundleComparisonChange {
        RunConfiguration,
        Result,
        Authority,
        Animation,
        EventTranscript,
        StructuredLog,
        Screenshots,
        Accessibility,
        Layout,
        VisibleCopy,
        Icons,
    }

    /// Deliberately redacted report for a completed source-tree XCTest
    /// fixture bundle. It never returns source metadata, screenshot paths,
    /// visible copy, event titles, or raw evidence content.
    #[derive(Debug, serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct FixtureBundleVerificationReport {
        schema_version: u16,
        lane: DiagnosticLane,
        status: DiagnosticTerminalStatus,
        message: String,
        bundle_path: String,
        scenario_id: String,
        scenario_version: u32,
        event_count: usize,
        evidence: FixtureBundleEvidenceSummary,
        presentation_fixture: bool,
        authority: &'static str,
        network: &'static str,
    }

    #[derive(Debug, serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct FixtureBundleEvidenceSummary {
        file_count: usize,
        total_bytes: u64,
        hash_manifest_sha256: String,
    }

    /// Redacted result for a comparison of two independently verified
    /// completed fixture bundles. It reports only immutable identifiers,
    /// aggregate evidence facts, and fixed changed-category labels.
    #[derive(Debug, serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct FixtureBundleComparisonReport {
        schema_version: u16,
        lane: DiagnosticLane,
        status: DiagnosticTerminalStatus,
        message: &'static str,
        baseline: FixtureBundleComparisonIdentity,
        candidate: FixtureBundleComparisonIdentity,
        equivalent: bool,
        changed: Vec<FixtureBundleComparisonChange>,
        presentation_fixture: bool,
        authority: &'static str,
        network: &'static str,
    }

    #[derive(Debug, serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct FixtureBundleComparisonIdentity {
        scenario_id: String,
        scenario_version: u32,
        event_count: usize,
        evidence: FixtureBundleEvidenceSummary,
    }

    /// Internal result only: it retains the verified source metadata long
    /// enough to construct the public redacted report, but it is never
    /// serialized as raw source evidence.
    #[derive(Debug)]
    struct ExportedRedactedFixtureBundle {
        output_path: String,
        sha256: String,
        verified: VerifiedFixtureBundle,
    }

    /// The public export report identifies the newly created owner-only
    /// archive, not the private source bundle. Its contents are fixed to
    /// redacted aggregate facts and integrity hashes.
    #[derive(Debug, serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct FixtureBundleExportReport {
        schema_version: u16,
        lane: DiagnosticLane,
        status: DiagnosticTerminalStatus,
        message: &'static str,
        export_path: String,
        sha256: String,
        scenario_id: String,
        scenario_version: u32,
        event_count: usize,
        evidence: FixtureBundleEvidenceSummary,
        redacted: bool,
        exact_archive: bool,
        raw_evidence_exported: bool,
        presentation_fixture: bool,
        authority: &'static str,
        network: &'static str,
    }

    pub(super) fn run(args: ForceUiArgs) -> anyhow::Result<()> {
        let root = OwnerOnlyTemporaryRoot::new("whisply-fixture")
            .map_err(|_| anyhow::anyhow!("Unable to create an owner-only fixture root."))?;
        let output_dir = args.output_dir.clone();
        let request = build_request(args, &root)?;
        run_request(&root, request, &output_dir)
    }

    pub(super) fn run_catalog(
        catalog: CatalogFixtureExecution,
        capture: Option<FixtureCapturePoint>,
        output_dir: &Path,
    ) -> anyhow::Result<()> {
        let root = OwnerOnlyTemporaryRoot::new("whisply-fixture")
            .map_err(|_| anyhow::anyhow!("Unable to create an owner-only fixture root."))?;
        let request = build_catalog_request(catalog, &root, capture)?;
        run_request(&root, request, output_dir)
    }

    fn run_request(
        root: &OwnerOnlyTemporaryRoot,
        request: ForceUiRequest,
        output_dir: &Path,
    ) -> anyhow::Result<()> {
        let output_dir = resolve_owner_selected_export_directory(output_dir)?;
        let launcher_manifest_sha256 =
            release_manifest_sha256_from_environment().ok_or_else(|| {
                anyhow::anyhow!(
                    "The verified release manifest is unavailable for this fixture request."
                )
            })?;
        let installed_manifest_sha256 = installed_release_manifest_sha256()?;
        if launcher_manifest_sha256 != installed_manifest_sha256 {
            anyhow::bail!(
                "The installed presentation fixture host is not the same verified Whisply release."
            );
        }

        let now_ms = unix_ms()?;
        let manifest = DiagnosticLaunchManifest {
            schema_version: DIAGNOSTIC_PROTOCOL_SCHEMA_VERSION,
            lane: DiagnosticLane::PresentationFixture,
            release_manifest_sha256: installed_manifest_sha256,
            fixture_root: root.path().to_path_buf(),
            socket_path: root
                .child("fixture.sock")
                .map_err(|_| anyhow::anyhow!("Unable to reserve fixture control socket."))?,
            expires_at_ms: now_ms.saturating_add(DEFAULT_FIXTURE_SESSION_TTL_MS),
        };
        manifest.validate(now_ms).map_err(|_| {
            anyhow::anyhow!("Fixture launch manifest did not meet the zero-authority contract.")
        })?;
        write_launch_manifest(root, &manifest)?;

        let listener = bind_fixture_listener(&manifest)?;
        let capability = random_bytes();
        let capability_fd = one_time_capability_pipe(&capability)?;
        let mut child = launch_verified_fixture_host(root, &capability_fd)?;
        drop(capability_fd);

        let response = match exchange_fixture_request(&listener, &capability, &request) {
            Ok(response) => response,
            Err(error) => {
                stop_child(&mut child);
                return Err(error);
            }
        };
        if let Err(error) = wait_for_child(&mut child) {
            stop_child(&mut child);
            return Err(error);
        }
        let capture = validate_response(&response, &request, root)?;
        let artifacts = retain_verified_fixture_artifacts(&capture, &response, &output_dir)?;

        let report = FixtureTerminalReport {
            schema_version: DIAGNOSTIC_EXPORT_SCHEMA_VERSION,
            lane: DiagnosticLane::PresentationFixture,
            status: DiagnosticTerminalStatus::Succeeded,
            message: format!(
                "Retained verified zero-authority fixture screenshot at {}.",
                artifacts.screenshot_path
            ),
            presentation_fixture: true,
            authority: "none",
            network: "denied",
            capture_method: response.capture_method,
            computer_use: response.computer_use,
            screen_recording: response.screen_recording,
            screenshot_path: artifacts.screenshot_path,
            artifact_paths: artifacts.artifact_paths,
        };
        print_report(&report)
    }

    fn build_request(
        args: ForceUiArgs,
        root: &OwnerOnlyTemporaryRoot,
    ) -> anyhow::Result<ForceUiRequest> {
        // A direct static fixture can use only its host-reviewed capture
        // lifecycle. In particular, the sole named event selects the inert
        // real Chat Overlay streaming atom; callers cannot send an event
        // stream, timeline, or arbitrary event identity.
        let capture = match (args.capture, args.capture_event) {
            (FixtureCaptureArg::Initial, None) => FixtureCapturePoint::Initial,
            (FixtureCaptureArg::Terminal, None) => FixtureCapturePoint::Terminal,
            (FixtureCaptureArg::NamedEvent, Some(event_id)) => {
                FixtureCapturePoint::NamedEvent { event_id }
            }
            (FixtureCaptureArg::NamedEvent, None) => {
                anyhow::bail!("Named-event capture requires --capture-event <EVENT>.")
            }
            (_, Some(_)) => {
                anyhow::bail!("--capture-event is valid only with --capture named-event.")
            }
        };
        let display = FixtureDisplay {
            display_identifier: args.display,
            backing_scale_milli: args.backing_scale.map(|scale| scale.0),
        };
        let request = ForceUiRequest {
            scenario: FixtureScenario {
                fixture_id: format!("fixture-{}", args.seed),
                surface_id: args.surface,
                state_id: args.state,
                seed: args.seed,
                lane: DiagnosticLane::PresentationFixture,
                fixture_root: root.path().to_path_buf(),
                simulated: true,
                billable: false,
                authority: "none".to_string(),
            },
            width: args.width,
            height: args.height,
            appearance: args.appearance.into(),
            theme: args.theme.into(),
            // These values are fixture-local and cross the authenticated
            // owner-only request unchanged. The host records the real macOS
            // accessibility state separately instead of modifying it.
            presentation: FixturePresentation {
                locale: args.locale,
                layout_direction: args.layout_direction.into(),
                font_scale: args.font_scale.into(),
                reduce_motion: args.motion.reduce_motion(),
                increased_contrast: args.contrast.increased_contrast(),
            },
            display,
            capture,
            watermark: PRESENTATION_FIXTURE_MARKER.to_string(),
            fixture_thread: None,
            fixture_replay: None,
        };
        request.validate().map_err(|_| {
            anyhow::anyhow!("Fixture request did not meet the zero-authority contract.")
        })?;
        Ok(request)
    }

    fn build_catalog_request(
        catalog: CatalogFixtureExecution,
        root: &OwnerOnlyTemporaryRoot,
        capture: Option<FixtureCapturePoint>,
    ) -> anyhow::Result<ForceUiRequest> {
        let execution = catalog.execution;
        let replay = catalog.replay;
        let capture = match (capture, replay.as_ref()) {
            (Some(_), Some(_)) => anyhow::bail!(
                "Catalog fixture replay derives its capture point and cannot accept an explicit capture."
            ),
            (None, Some(replay)) => fixture_capture_for_replay(&execution, replay)
                .map_err(|_| anyhow::anyhow!("Catalog fixture replay state is invalid."))?,
            (Some(capture), None) => capture,
            (None, None) => anyhow::bail!("Catalog fixture capture is required."),
        };
        let state_id = match &capture {
            FixtureCapturePoint::Initial => "initial",
            FixtureCapturePoint::Terminal => "terminal",
            FixtureCapturePoint::NamedEvent { event_id } => {
                if !execution.supports_named_capture_event(event_id.as_str()) {
                    anyhow::bail!(
                        "Capture event is not present in the immutable catalog fixture stream."
                    )
                }
                "event"
            }
        };
        let request = ForceUiRequest {
            scenario: FixtureScenario {
                fixture_id: execution.plan.fixture_id.clone(),
                surface_id: "chat.activity".to_string(),
                state_id: state_id.to_string(),
                seed: u64::from(execution.plan.seed),
                lane: DiagnosticLane::PresentationFixture,
                fixture_root: root.path().to_path_buf(),
                simulated: true,
                billable: false,
                authority: "none".to_string(),
            },
            width: catalog.width,
            height: catalog.height,
            appearance: catalog.appearance,
            theme: catalog.theme,
            presentation: catalog.presentation,
            display: FixtureDisplay::default(),
            capture,
            watermark: PRESENTATION_FIXTURE_MARKER.to_string(),
            fixture_thread: Some(execution),
            fixture_replay: replay,
        };
        request.validate().map_err(|_| {
            anyhow::anyhow!("Catalog fixture request did not meet the zero-authority contract.")
        })?;
        Ok(request)
    }

    fn write_launch_manifest(
        root: &OwnerOnlyTemporaryRoot,
        manifest: &DiagnosticLaunchManifest,
    ) -> anyhow::Result<()> {
        let bytes = serde_json::to_vec(manifest)
            .context("Unable to encode the presentation fixture launch manifest.")?;
        if bytes.is_empty() || bytes.len() > MAX_LAUNCH_MANIFEST_BYTES {
            anyhow::bail!("Presentation fixture launch manifest is invalid.");
        }
        let path = root
            .child(FIXTURE_LAUNCH_FILE)
            .map_err(|_| anyhow::anyhow!("Unable to reserve the fixture launch manifest."))?;
        write_new_private(&path, &bytes)
            .context("Unable to write the owner-only fixture launch manifest.")
    }

    fn bind_fixture_listener(manifest: &DiagnosticLaunchManifest) -> anyhow::Result<UnixListener> {
        let listener = UnixListener::bind(&manifest.socket_path)
            .context("Unable to reserve the owner-only fixture control socket.")?;
        fs::set_permissions(&manifest.socket_path, fs::Permissions::from_mode(0o600))
            .context("Unable to secure the fixture control socket.")?;
        codex_whisply::validate_owner_only_diagnostic_socket(&manifest.socket_path)
            .map_err(|_| anyhow::anyhow!("Fixture control socket did not remain owner-only."))?;
        listener
            .set_nonblocking(true)
            .context("Unable to configure the fixture control socket.")?;
        Ok(listener)
    }

    fn one_time_capability_pipe(capability: &[u8; 32]) -> anyhow::Result<OwnedFd> {
        let mut fds = [-1_i32; 2];
        // `pipe` produces a private in-memory channel. The read end is moved
        // into the child with `dup2` below; the write end is closed before
        // launch, so the host can read exactly one 32-byte capability and EOF.
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error())
                .context("Unable to create the fixture capability pipe.");
        }
        // SAFETY: `pipe` returned two owned descriptors on success. The
        // wrappers take ownership exactly once and close them on all paths.
        let read_end = unsafe { OwnedFd::from_raw_fd(fds[0]) };
        // SAFETY: see the ownership note above for the read end.
        let mut write_end = unsafe { File::from_raw_fd(fds[1]) };
        if let Err(error) = write_end.write_all(capability) {
            return Err(error).context("Unable to write the fixture capability.");
        }
        drop(write_end);
        Ok(read_end)
    }

    fn launch_verified_fixture_host(
        root: &OwnerOnlyTemporaryRoot,
        capability_fd: &OwnedFd,
    ) -> anyhow::Result<Child> {
        let binary = verified_fixture_host_binary()?;
        let source_fd = capability_fd.as_raw_fd();
        let mut command = Command::new(binary);
        command
            .arg("--whisply-presentation-fixture")
            .arg(root.path())
            .env_clear()
            .env("HOME", root.path())
            .env("TMPDIR", root.path())
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .current_dir(root.path());
        // SAFETY: the pre-exec closure performs only `dup2` and `close`, both
        // async-signal-safe. It contains no allocation, locks, I/O, or Rust
        // runtime calls before `exec` enters the exact installed app binary.
        unsafe {
            command.pre_exec(move || {
                if libc::dup2(source_fd, FIXTURE_CAPABILITY_FD) == -1 {
                    return Err(io::Error::last_os_error());
                }
                if source_fd != FIXTURE_CAPABILITY_FD && libc::close(source_fd) == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        command
            .spawn()
            .context("Unable to launch the verified installed presentation fixture host.")
    }

    fn verified_fixture_host_binary() -> anyhow::Result<PathBuf> {
        let app = Path::new(INSTALLED_APP_PATH);
        let app_metadata = fs::symlink_metadata(app)
            .context("The verified installed Whisply app is unavailable.")?;
        if app_metadata.file_type().is_symlink() || !app_metadata.is_dir() {
            anyhow::bail!("The verified installed Whisply app is unavailable.");
        }
        let binary = app.join(INSTALLED_APP_BINARY_RELATIVE_PATH);
        let metadata = fs::symlink_metadata(&binary)
            .context("The verified installed presentation fixture host is unavailable.")?;
        if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.mode() & 0o111 == 0
        {
            anyhow::bail!("The verified installed presentation fixture host is unavailable.");
        }
        Ok(binary)
    }

    fn installed_release_manifest_sha256() -> anyhow::Result<String> {
        let path = Path::new(INSTALLED_APP_PATH).join(RUNTIME_MANIFEST_RELATIVE_PATH);
        let metadata = fs::symlink_metadata(&path)
            .context("The verified installed Whisply runtime manifest is unavailable.")?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.len() == 0
            || metadata.len() > MAX_MANIFEST_BYTES
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
        let hash = hex_encode(&digest.finalize());
        if !valid_lowercase_sha256(&hash) {
            anyhow::bail!("The verified installed Whisply runtime manifest is unavailable.");
        }
        Ok(hash)
    }

    fn exchange_fixture_request(
        listener: &UnixListener,
        capability: &[u8; 32],
        request: &ForceUiRequest,
    ) -> anyhow::Result<FixtureHostResponse> {
        let request_bytes = serde_json::to_vec(request)
            .context("Unable to encode the presentation fixture request.")?;
        if request_bytes.is_empty() || request_bytes.len() > MAX_FIXTURE_REQUEST_BYTES {
            anyhow::bail!("Presentation fixture request is invalid.");
        }
        let deadline = Instant::now() + HOST_CONNECT_TIMEOUT;
        while Instant::now() < deadline {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    if codex_whisply::validate_diagnostic_socket_peer(&stream).is_err() {
                        continue;
                    }
                    if stream.set_read_timeout(Some(HOST_IO_TIMEOUT)).is_err()
                        || stream.set_write_timeout(Some(HOST_IO_TIMEOUT)).is_err()
                    {
                        continue;
                    }
                    if let Ok(response) =
                        exchange_one_connection(&mut stream, capability, &request_bytes)
                    {
                        return Ok(response);
                    }
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(25));
                }
                Err(error) => {
                    return Err(error)
                        .context("Unable to accept the fixture host control connection.");
                }
            }
        }
        anyhow::bail!("The verified Swift presentation fixture host did not authenticate in time.");
    }

    fn exchange_one_connection(
        stream: &mut UnixStream,
        capability: &[u8; 32],
        request: &[u8],
    ) -> anyhow::Result<FixtureHostResponse> {
        let challenge = random_bytes();
        stream.write_all(&challenge)?;
        let mut proof = [0_u8; 32];
        stream.read_exact(&mut proof)?;
        let mut verifier = HmacSha256::new_from_slice(capability)
            .map_err(|_| anyhow::anyhow!("Fixture capability is invalid."))?;
        verifier.update(&challenge);
        verifier
            .verify_slice(&proof)
            .map_err(|_| anyhow::anyhow!("Fixture capability proof was rejected."))?;
        write_control_frame(stream, request, MAX_FIXTURE_REQUEST_BYTES)?;
        let response = read_control_frame(stream, MAX_FIXTURE_RESPONSE_BYTES)?;
        serde_json::from_slice(&response)
            .context("Presentation fixture host returned an invalid response.")
    }

    fn read_control_frame(
        stream: &mut UnixStream,
        maximum_bytes: usize,
    ) -> anyhow::Result<Vec<u8>> {
        let mut header = [0_u8; 4];
        stream.read_exact(&mut header)?;
        let length = u32::from_be_bytes(header) as usize;
        if length == 0 || length > maximum_bytes {
            anyhow::bail!("Presentation fixture host returned an invalid response.");
        }
        let mut frame = vec![0_u8; length];
        stream.read_exact(&mut frame)?;
        Ok(frame)
    }

    fn write_control_frame(
        stream: &mut UnixStream,
        bytes: &[u8],
        maximum_bytes: usize,
    ) -> anyhow::Result<()> {
        if bytes.is_empty() || bytes.len() > maximum_bytes {
            anyhow::bail!("Presentation fixture request is invalid.");
        }
        let length = u32::try_from(bytes.len())?.to_be_bytes();
        stream.write_all(&length)?;
        stream.write_all(bytes)?;
        Ok(())
    }

    fn validate_response(
        response: &FixtureHostResponse,
        request: &ForceUiRequest,
        root: &OwnerOnlyTemporaryRoot,
    ) -> anyhow::Result<ValidatedFixtureCapture> {
        if response.schema_version != DIAGNOSTIC_PROTOCOL_SCHEMA_VERSION
            || response.fixture_id != request.scenario.fixture_id
            || response.terminal_status != DiagnosticTerminalStatus::Succeeded
            || !response.presentation_fixture
            || response.authority != "none"
            || response.network != "denied"
            || response.capture_method != SELF_CAPTURE_METHOD
            || response.computer_use != SELF_CAPTURE_NOT_USED
            || response.screen_recording != SELF_CAPTURE_NOT_USED
            || response.capture_path != "fixture-capture.png"
            || !valid_lowercase_sha256(&response.capture_sha256)
            || !ui_metadata_matches_request(&response.ui_metadata, request)
        {
            anyhow::bail!("Presentation fixture host returned an invalid response.");
        }
        let capture = root
            .child(&response.capture_path)
            .map_err(|_| anyhow::anyhow!("Presentation fixture capture path is invalid."))?;
        let metadata = fs::symlink_metadata(&capture)
            .context("Presentation fixture capture is unavailable.")?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o077 != 0
            || metadata.len() == 0
            || metadata.len() > MAX_CAPTURE_BYTES
        {
            anyhow::bail!("Presentation fixture capture is invalid.");
        }
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&capture)
            .context("Presentation fixture capture is unavailable.")?;
        let mut digest = Sha256::new();
        let mut buffer = [0_u8; 64 * 1_024];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            digest.update(&buffer[..read]);
        }
        if hex_encode(&digest.finalize()) != response.capture_sha256 {
            anyhow::bail!("Presentation fixture capture integrity check failed.");
        }
        Ok(ValidatedFixtureCapture {
            source_path: capture,
            sha256: response.capture_sha256.clone(),
        })
    }

    fn ui_metadata_matches_request(metadata: &FixtureUiMetadata, request: &ForceUiRequest) -> bool {
        let requested = &metadata.requested;
        let effective = &metadata.effective;
        let requested_capture_matches = match &request.capture {
            FixtureCapturePoint::Initial => {
                requested.capture_kind == "initial" && requested.capture_event_id.is_none()
            }
            FixtureCapturePoint::Terminal => {
                requested.capture_kind == "terminal" && requested.capture_event_id.is_none()
            }
            FixtureCapturePoint::NamedEvent { event_id } => {
                requested.capture_kind == "named_event"
                    && requested.capture_event_id.as_deref() == Some(event_id)
            }
        };
        let requested_appearance = fixture_appearance_name(request.appearance);
        let requested_theme = fixture_theme_name(request.theme);
        let requested_layout_direction =
            fixture_layout_direction_name(request.presentation.layout_direction);
        let requested_font_scale = fixture_font_scale_name(request.presentation.font_scale);
        let requested_dynamic_type_size =
            fixture_dynamic_type_size_name(request.presentation.font_scale);
        let expected_width = f64::from(request.width);
        let expected_height = f64::from(request.height);
        let maximum_pixel_width = u32::from(request.width).saturating_mul(8);
        let maximum_pixel_height = u32::from(request.height).saturating_mul(8);
        let display_matches = if effective.display_available {
            effective
                .display_identifier
                .is_some_and(|identifier| identifier != 0)
        } else {
            effective.display_identifier.is_none()
        };
        let requested_display_matches =
            request.display.display_identifier.is_none_or(|identifier| {
                effective.display_available && effective.display_identifier == Some(identifier)
            });
        let requested_backing_scale_matches =
            request.display.backing_scale_milli.is_none_or(|expected| {
                requested_display_matches
                    && backing_scale_milli(effective.backing_scale_factor) == Some(expected)
            });

        metadata.schema_version == FIXTURE_UI_METADATA_SCHEMA_VERSION
            && requested.fixture_id == request.scenario.fixture_id
            && requested.surface_id == request.scenario.surface_id
            && requested.state_id == request.scenario.state_id
            && requested.seed == request.scenario.seed
            && requested.content_width_points == request.width
            && requested.content_height_points == request.height
            && requested.appearance == requested_appearance
            && requested.theme == requested_theme
            && requested.locale == request.presentation.locale
            && requested.layout_direction == requested_layout_direction
            && requested.font_scale == requested_font_scale
            && requested.reduce_motion == request.presentation.reduce_motion
            && requested.increased_contrast == request.presentation.increased_contrast
            && requested.display_identifier == request.display.display_identifier
            && requested.backing_scale_milli == request.display.backing_scale_milli
            && requested_capture_matches
            && effective.fixture_id == request.scenario.fixture_id
            && effective.surface_id == request.scenario.surface_id
            && effective.state_id == request.scenario.state_id
            && effective.content_frame_points.x.is_finite()
            && effective.content_frame_points.y.is_finite()
            && effective.content_frame_points.width.is_finite()
            && effective.content_frame_points.height.is_finite()
            && effective.content_frame_points.x == 0.0
            && effective.content_frame_points.y == 0.0
            && effective.content_frame_points.width == expected_width
            && effective.content_frame_points.height == expected_height
            && effective.capture_pixel_width >= u32::from(request.width)
            && effective.capture_pixel_width <= maximum_pixel_width
            && effective.capture_pixel_height >= u32::from(request.height)
            && effective.capture_pixel_height <= maximum_pixel_height
            && matches!(
                effective.appearance.as_str(),
                "light" | "dark" | "unresolved"
            )
            && fixture_appearance_matches(requested_appearance, effective.appearance.as_str())
            && effective.theme == requested_theme
            && effective.locale == request.presentation.locale
            && effective.layout_direction == requested_layout_direction
            && effective.font_scale == requested_font_scale
            && effective.applied_presentation.layout_direction == requested_layout_direction
            && effective.applied_presentation.font_scale == requested_font_scale
            && effective.applied_presentation.dynamic_type_size == requested_dynamic_type_size
            && effective.applied_presentation.reduce_motion == request.presentation.reduce_motion
            && effective.applied_presentation.increased_contrast
                == request.presentation.increased_contrast
            && effective.applied_presentation.contrast_gain
                == fixture_contrast_gain(request.presentation.increased_contrast)
            && display_matches
            && requested_display_matches
            && effective.backing_scale_factor.is_finite()
            && (0.5..=8.0).contains(&effective.backing_scale_factor)
            && requested_backing_scale_matches
            && effective.accessibility_identifier == FIXTURE_HOST_ACCESSIBILITY_IDENTIFIER
            && effective.accessibility_label == PRESENTATION_FIXTURE_MARKER
            && fixture_window_metadata_matches(
                &effective.window_metadata,
                effective,
                expected_width,
                expected_height,
            )
            && fixture_rendered_icon_registry_matches(&effective.rendered_icon_registry, request)
    }

    fn backing_scale_milli(scale: f64) -> Option<u16> {
        if !scale.is_finite() {
            return None;
        }
        let milli = (scale * 1_000.0).round();
        if (scale * 1_000.0 - milli).abs() > 0.001 {
            return None;
        }
        u16::try_from(milli as u32).ok()
    }

    fn fixture_appearance_name(appearance: FixtureAppearance) -> &'static str {
        match appearance {
            FixtureAppearance::Light => "light",
            FixtureAppearance::Dark => "dark",
            FixtureAppearance::Auto => "auto",
        }
    }

    fn fixture_appearance_matches(requested: &str, effective: &str) -> bool {
        match requested {
            "light" => effective == "light",
            "dark" => effective == "dark",
            "auto" => matches!(effective, "light" | "dark"),
            _ => false,
        }
    }

    fn fixture_theme_name(theme: FixtureTheme) -> &'static str {
        match theme {
            FixtureTheme::System => "system",
            FixtureTheme::Light => "light",
            FixtureTheme::Dark => "dark",
            FixtureTheme::Midnight => "midnight",
            FixtureTheme::Graphite => "graphite",
            FixtureTheme::Meadow => "meadow",
            FixtureTheme::Sunset => "sunset",
            FixtureTheme::Dawn => "dawn",
            FixtureTheme::Linen => "linen",
            FixtureTheme::Rainbow => "rainbow",
            FixtureTheme::Galaxy => "galaxy",
            FixtureTheme::Aurora => "aurora",
            FixtureTheme::Ocean => "ocean",
            FixtureTheme::Ember => "ember",
            FixtureTheme::Sol => "sol",
        }
    }

    fn fixture_layout_direction_name(direction: FixtureLayoutDirection) -> &'static str {
        match direction {
            FixtureLayoutDirection::LeftToRight => "left_to_right",
            FixtureLayoutDirection::RightToLeft => "right_to_left",
        }
    }

    fn fixture_font_scale_name(scale: FixtureFontScale) -> &'static str {
        match scale {
            FixtureFontScale::Small => "small",
            FixtureFontScale::Standard => "standard",
            FixtureFontScale::Large => "large",
        }
    }

    fn fixture_dynamic_type_size_name(scale: FixtureFontScale) -> &'static str {
        match scale {
            FixtureFontScale::Small => "small",
            FixtureFontScale::Standard => "large",
            FixtureFontScale::Large => "xxx_large",
        }
    }

    fn fixture_contrast_gain(increased_contrast: bool) -> f64 {
        if increased_contrast { 1.2 } else { 1.0 }
    }

    fn fixture_window_metadata_matches(
        metadata: &FixtureWindowMetadata,
        effective: &FixtureEffectiveUiMetadata,
        expected_width: f64,
        expected_height: f64,
    ) -> bool {
        let display_matches = if metadata.display_available {
            metadata
                .display_identifier
                .is_some_and(|identifier| identifier != 0)
                && metadata
                    .display_frame_points
                    .as_ref()
                    .is_some_and(valid_window_frame)
                && metadata
                    .display_visible_frame_points
                    .as_ref()
                    .is_some_and(valid_window_frame)
        } else {
            metadata.display_identifier.is_none()
                && metadata.display_frame_points.is_none()
                && metadata.display_visible_frame_points.is_none()
        };
        let frame_matches = valid_window_frame(&metadata.frame_points)
            && metadata.frame_points.width >= expected_width
            && metadata.frame_points.width <= expected_width * 4.0
            && metadata.frame_points.height >= expected_height
            && metadata.frame_points.height <= expected_height * 4.0;
        let active_space_is_coherent = !metadata.is_on_active_space || metadata.display_available;
        let visibility_is_coherent = !metadata.is_visible || metadata.display_available;

        metadata.level.semantic == "normal"
            && metadata.level.raw_value == 0
            && frame_matches
            && display_matches
            && metadata.display_available == effective.display_available
            && metadata.display_identifier == effective.display_identifier
            && collection_behavior_matches(&metadata.collection_behavior)
            && active_space_is_coherent
            && visibility_is_coherent
    }

    fn valid_window_frame(frame: &FixtureWindowFrame) -> bool {
        frame.x.is_finite()
            && frame.y.is_finite()
            && frame.width.is_finite()
            && frame.height.is_finite()
            && frame.width > 0.0
            && frame.height > 0.0
    }

    fn collection_behavior_matches(collection: &FixtureWindowCollectionBehavior) -> bool {
        const CAN_JOIN_ALL_SPACES: u64 = 1 << 0;
        const MOVE_TO_ACTIVE_SPACE: u64 = 1 << 1;
        const STATIONARY: u64 = 1 << 4;
        const IGNORES_CYCLE: u64 = 1 << 6;
        const FULL_SCREEN_AUXILIARY: u64 = 1 << 8;

        collection.can_join_all_spaces == (collection.raw_value & CAN_JOIN_ALL_SPACES != 0)
            && collection.move_to_active_space == (collection.raw_value & MOVE_TO_ACTIVE_SPACE != 0)
            && collection.stationary == (collection.raw_value & STATIONARY != 0)
            && collection.full_screen_auxiliary
                == (collection.raw_value & FULL_SCREEN_AUXILIARY != 0)
            && collection.ignores_cycle == (collection.raw_value & IGNORES_CYCLE != 0)
    }

    fn fixture_rendered_icon_registry_matches(
        actual: &FixtureRenderedIconRegistry,
        request: &ForceUiRequest,
    ) -> bool {
        let Some(mut expected) = expected_fixture_rendered_icon_registry(request) else {
            return false;
        };

        if request.scenario.surface_id == "subscription.gate"
            && request.scenario.state_id == "starter"
        {
            let Some(actual_emblem) = actual.registry.first() else {
                return false;
            };
            if !subscription_gate_emblem_matches(actual_emblem) {
                return false;
            }
            // The SVG can be absent from a development bundle. Preserve the
            // actual bundle digest when it is present, but require the same
            // registry position and all remaining exact entries/occurrences.
            expected.registry[0] = actual_emblem.clone();
        }

        actual == &expected
    }

    fn expected_fixture_rendered_icon_registry(
        request: &ForceUiRequest,
    ) -> Option<FixtureRenderedIconRegistry> {
        if request.fixture_thread.is_some() {
            return expected_fixture_thread_icon_registry(request);
        }

        match (
            request.scenario.surface_id.as_str(),
            request.scenario.state_id.as_str(),
        ) {
            ("chat.overlay", "empty" | "streaming") => Some(FixtureRenderedIconRegistry {
                registry: Vec::new(),
                rendered_icon_ids: Vec::new(),
            }),
            ("chat.overlay", "needs-input") => Some(chat_overlay_needs_input_icon_registry()),
            ("history", "empty") => Some(history_empty_icon_registry()),
            ("preferences", "general") => Some(preferences_navigation_icon_registry()),
            ("preferences", "usage") => Some(usage_icon_registry()),
            ("preferences", "account") => Some(account_icon_registry()),
            ("subscription.gate", "starter") => {
                Some(subscription_gate_icon_registry(fixture_system_icon(
                    "fixture.subscription.gate.emblem",
                    "bolt.circle.fill",
                    "accent",
                    22.0,
                    "bundle_asset_missing",
                )))
            }
            _ => None,
        }
    }

    fn history_empty_icon_registry() -> FixtureRenderedIconRegistry {
        let empty_state = fixture_system_icon(
            "fixture.history.empty.primary",
            "bubble.left.and.text.bubble.right",
            "accent-80",
            36.0,
            "none",
        );
        let new_chat = fixture_system_icon(
            "fixture.history.empty.new-chat",
            "plus",
            "prominent-button",
            13.0,
            "none",
        );
        FixtureRenderedIconRegistry {
            registry: vec![empty_state.clone(), new_chat.clone()],
            rendered_icon_ids: vec![empty_state.registry_id, new_chat.registry_id],
        }
    }

    fn chat_overlay_needs_input_icon_registry() -> FixtureRenderedIconRegistry {
        let waiting = fixture_system_icon(
            "fixture.chat.overlay.needs-input.primary",
            "questionmark.bubble.fill",
            "accent",
            12.0,
            "none",
        );
        FixtureRenderedIconRegistry {
            registry: vec![waiting.clone()],
            rendered_icon_ids: vec![waiting.registry_id],
        }
    }

    fn expected_fixture_thread_icon_registry(
        request: &ForceUiRequest,
    ) -> Option<FixtureRenderedIconRegistry> {
        let execution = request.fixture_thread.as_ref()?;
        let header_disclosure = fixture_system_icon(
            "fixture.chat.activity.header.disclosure",
            "chevron.right",
            "secondary",
            10.0,
            "none",
        );
        let mut registry = vec![header_disclosure.clone()];
        let mut rendered_icon_ids = Vec::new();

        let event_count = match &request.capture {
            FixtureCapturePoint::Initial => {
                if request.presentation.reduce_motion {
                    let running_state = fixture_system_icon(
                        "fixture.chat.activity.header.running-state",
                        "circle.dotted",
                        "accent",
                        14.0,
                        "none",
                    );
                    rendered_icon_ids.push(running_state.registry_id.clone());
                    registry.push(running_state);
                }
                rendered_icon_ids.push(header_disclosure.registry_id.clone());
                0
            }
            FixtureCapturePoint::Terminal => {
                rendered_icon_ids.push(header_disclosure.registry_id.clone());
                execution.events.events.len().saturating_sub(1)
            }
            FixtureCapturePoint::NamedEvent { event_id } => {
                rendered_icon_ids.push(header_disclosure.registry_id.clone());
                execution
                    .events
                    .events
                    .iter()
                    .position(|event| event.event_id == *event_id)?
            }
        };

        if event_count > 0 {
            let event_icon = fixture_system_icon(
                "fixture.chat.activity.event.icon",
                "wrench.and.screwdriver",
                "success",
                17.0,
                "none",
            );
            let event_state = fixture_system_icon(
                "fixture.chat.activity.event.state",
                "checkmark.circle.fill",
                "success",
                10.0,
                "none",
            );
            registry.push(event_icon.clone());
            registry.push(event_state.clone());
            for _ in 0..event_count {
                rendered_icon_ids.push(event_icon.registry_id.clone());
                rendered_icon_ids.push(event_state.registry_id.clone());
            }
        }

        Some(FixtureRenderedIconRegistry {
            registry,
            rendered_icon_ids,
        })
    }

    fn preferences_navigation_icon_registry() -> FixtureRenderedIconRegistry {
        let registry = vec![
            fixture_system_icon(
                "fixture.preferences.general.tab.general",
                "gearshape",
                "primary",
                16.0,
                "none",
            ),
            fixture_system_icon(
                "fixture.preferences.general.tab.integrations",
                "square.stack.3d.up",
                "secondary",
                16.0,
                "none",
            ),
            fixture_system_icon(
                "fixture.preferences.general.tab.connectors",
                "cable.connector",
                "secondary",
                16.0,
                "none",
            ),
            fixture_system_icon(
                "fixture.preferences.general.tab.modes",
                "square.stack.3d.up",
                "secondary",
                16.0,
                "none",
            ),
            fixture_system_icon(
                "fixture.preferences.general.tab.computer-use",
                "cursorarrow.motionlines",
                "secondary",
                16.0,
                "none",
            ),
            fixture_system_icon(
                "fixture.preferences.general.tab.exam",
                "shield",
                "secondary",
                16.0,
                "none",
            ),
            fixture_system_icon(
                "fixture.preferences.general.tab.customisation",
                "paintpalette",
                "secondary",
                16.0,
                "none",
            ),
            fixture_system_icon(
                "fixture.preferences.general.tab.shortcuts",
                "keyboard",
                "secondary",
                16.0,
                "none",
            ),
            fixture_system_icon(
                "fixture.preferences.general.tab.usage",
                "chart.bar.xaxis",
                "secondary",
                16.0,
                "none",
            ),
            fixture_system_icon(
                "fixture.preferences.general.tab.account",
                "person.crop.circle",
                "secondary",
                16.0,
                "none",
            ),
            fixture_system_icon(
                "fixture.preferences.general.tab.about",
                "info.circle",
                "secondary",
                16.0,
                "none",
            ),
            fixture_system_icon(
                "fixture.preferences.general.footer.log-out",
                "arrow.uturn.left",
                "secondary",
                12.0,
                "none",
            ),
            fixture_system_icon(
                "fixture.preferences.general.footer.quit",
                "power",
                "secondary",
                12.0,
                "none",
            ),
        ];
        let rendered_icon_ids = registry
            .iter()
            .map(|entry| entry.registry_id.clone())
            .collect();
        FixtureRenderedIconRegistry {
            registry,
            rendered_icon_ids,
        }
    }

    fn usage_icon_registry() -> FixtureRenderedIconRegistry {
        let registry = vec![
            fixture_system_icon(
                "fixture.preferences.usage.approaching-warning",
                "exclamationmark.triangle",
                "warning",
                10.5,
                "none",
            ),
            fixture_system_icon(
                "fixture.preferences.usage.explanation-disclosure",
                "chevron.down",
                "accent",
                9.0,
                "none",
            ),
            fixture_system_icon(
                "fixture.preferences.usage.refresh",
                "arrow.clockwise",
                "primary",
                11.0,
                "none",
            ),
        ];
        let rendered_icon_ids = registry
            .iter()
            .map(|entry| entry.registry_id.clone())
            .collect();
        FixtureRenderedIconRegistry {
            registry,
            rendered_icon_ids,
        }
    }

    fn account_icon_registry() -> FixtureRenderedIconRegistry {
        let registry = vec![
            fixture_system_icon(
                "fixture.preferences.account.destination.personalization",
                "brain.head.profile",
                "accent",
                15.0,
                "none",
            ),
            fixture_system_icon(
                "fixture.preferences.account.destination.connections",
                "link",
                "accent",
                15.0,
                "none",
            ),
            fixture_system_icon(
                "fixture.preferences.account.destination.plugins",
                "puzzlepiece.extension",
                "accent",
                15.0,
                "none",
            ),
            fixture_system_icon(
                "fixture.preferences.account.destination.privacy",
                "lock.doc",
                "accent",
                15.0,
                "none",
            ),
            fixture_system_icon(
                "fixture.preferences.account.destination.usage-billing",
                "chart.bar.xaxis",
                "accent",
                15.0,
                "none",
            ),
            fixture_system_icon(
                "fixture.preferences.account.destination.disclosure",
                "chevron.right",
                "tertiary",
                9.0,
                "none",
            ),
        ];
        let disclosure_id = registry
            .last()
            .expect("disclosure entry")
            .registry_id
            .clone();
        let mut rendered_icon_ids = Vec::with_capacity(10);
        for destination in &registry[..5] {
            rendered_icon_ids.push(destination.registry_id.clone());
            rendered_icon_ids.push(disclosure_id.clone());
        }
        FixtureRenderedIconRegistry {
            registry,
            rendered_icon_ids,
        }
    }

    fn subscription_gate_icon_registry(
        emblem: FixtureRenderedIconRegistryEntry,
    ) -> FixtureRenderedIconRegistry {
        let account_menu_disclosure = fixture_system_icon(
            "fixture.subscription.gate.account-menu-disclosure",
            "chevron.down",
            "secondary",
            9.0,
            "none",
        );
        let included_feature = fixture_system_icon(
            "fixture.subscription.gate.feature.included",
            "checkmark",
            "success",
            11.0,
            "none",
        );
        let excluded_feature = fixture_system_icon(
            "fixture.subscription.gate.feature.excluded",
            "xmark",
            "secondary",
            11.0,
            "none",
        );
        let compare = fixture_system_icon(
            "fixture.subscription.gate.compare",
            "arrow.up.right",
            "secondary",
            10.0,
            "none",
        );
        let registry = vec![
            emblem.clone(),
            account_menu_disclosure.clone(),
            included_feature.clone(),
            excluded_feature.clone(),
            compare.clone(),
        ];
        let mut rendered_icon_ids = vec![emblem.registry_id, account_menu_disclosure.registry_id];
        append_repeated_icon_ids(&mut rendered_icon_ids, &included_feature.registry_id, 3);
        append_repeated_icon_ids(&mut rendered_icon_ids, &excluded_feature.registry_id, 3);
        append_repeated_icon_ids(&mut rendered_icon_ids, &included_feature.registry_id, 2);
        append_repeated_icon_ids(&mut rendered_icon_ids, &included_feature.registry_id, 5);
        append_repeated_icon_ids(&mut rendered_icon_ids, &excluded_feature.registry_id, 2);
        append_repeated_icon_ids(&mut rendered_icon_ids, &included_feature.registry_id, 2);
        append_repeated_icon_ids(&mut rendered_icon_ids, &included_feature.registry_id, 9);
        append_repeated_icon_ids(&mut rendered_icon_ids, &included_feature.registry_id, 9);
        rendered_icon_ids.push(compare.registry_id);
        FixtureRenderedIconRegistry {
            registry,
            rendered_icon_ids,
        }
    }

    fn append_repeated_icon_ids(ids: &mut Vec<String>, registry_id: &str, count: usize) {
        ids.extend(std::iter::repeat_n(registry_id.to_string(), count));
    }

    fn subscription_gate_emblem_matches(entry: &FixtureRenderedIconRegistryEntry) -> bool {
        if entry.registry_id != "fixture.subscription.gate.emblem"
            || entry.size_points
                != (FixtureIconSize {
                    width: 22.0,
                    height: 22.0,
                })
        {
            return false;
        }
        if entry.asset == "bundle:UI/whisply_emblem.svg" {
            entry.tint == "original"
                && entry.fallback_state == "none"
                && valid_lowercase_sha256(&entry.asset_sha256)
        } else {
            entry
                == &fixture_system_icon(
                    "fixture.subscription.gate.emblem",
                    "bolt.circle.fill",
                    "accent",
                    22.0,
                    "bundle_asset_missing",
                )
        }
    }

    fn fixture_system_icon(
        registry_id: &str,
        symbol_name: &str,
        tint: &str,
        size_points: f64,
        fallback_state: &str,
    ) -> FixtureRenderedIconRegistryEntry {
        let asset = format!("sf-symbol:{symbol_name}");
        let asset_sha256 = hex_encode(Sha256::digest(asset.as_bytes()));
        FixtureRenderedIconRegistryEntry {
            registry_id: registry_id.to_string(),
            asset,
            asset_sha256,
            tint: tint.to_string(),
            size_points: FixtureIconSize {
                width: size_points,
                height: size_points,
            },
            fallback_state: fallback_state.to_string(),
        }
    }

    fn resolve_owner_selected_export_directory(output_dir: &Path) -> anyhow::Result<PathBuf> {
        let metadata =
            fs::symlink_metadata(output_dir).context("Fixture output directory is unavailable.")?;
        if metadata.file_type().is_symlink() {
            anyhow::bail!("Fixture output directory must not be a symlink.");
        }
        let canonical = output_dir
            .canonicalize()
            .context("Fixture output directory is unavailable.")?;
        if !canonical.is_absolute() || canonical == Path::new("/") {
            anyhow::bail!("Fixture output directory is invalid.");
        }
        validate_owner_only_export_directory(&canonical)?;
        Ok(canonical)
    }

    fn validate_owner_only_export_directory(path: &Path) -> anyhow::Result<()> {
        let metadata =
            fs::symlink_metadata(path).context("Fixture output directory is unavailable.")?;
        if metadata.file_type().is_symlink()
            || !metadata.is_dir()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o077 != 0
        {
            anyhow::bail!("Fixture output directory must be owner-only.");
        }
        Ok(())
    }

    fn validate_owner_only_export_file(path: &Path, maximum_bytes: u64) -> anyhow::Result<()> {
        let metadata = fs::symlink_metadata(path).context("Fixture artifact is unavailable.")?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o077 != 0
            || metadata.len() == 0
            || metadata.len() > maximum_bytes
        {
            anyhow::bail!("Fixture artifact is invalid.");
        }
        Ok(())
    }

    fn create_new_private_export_directory(output_dir: &Path) -> anyhow::Result<PathBuf> {
        validate_owner_only_export_directory(output_dir)?;
        for _ in 0..MAX_EXPORT_DIRECTORY_ATTEMPTS {
            let directory = output_dir.join(format!("whisply-fixture-{}", Uuid::new_v4()));
            match fs::create_dir(&directory) {
                Ok(()) => {
                    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
                        .context("Unable to secure the retained fixture archive.")?;
                    validate_owner_only_export_directory(&directory)?;
                    return directory
                        .canonicalize()
                        .context("Unable to resolve the retained fixture archive.");
                }
                Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(error).context("Unable to create the retained fixture archive.");
                }
            }
        }
        anyhow::bail!("Unable to reserve a new retained fixture archive.");
    }

    fn retain_verified_fixture_artifacts(
        capture: &ValidatedFixtureCapture,
        response: &FixtureHostResponse,
        output_dir: &Path,
    ) -> anyhow::Result<RetainedFixtureArtifacts> {
        let archive = create_new_private_export_directory(output_dir)?;
        let result = (|| {
            let screenshot = archive.join(EXPORTED_CAPTURE_FILE);
            let artifact_manifest = archive.join(EXPORTED_ARTIFACT_MANIFEST_FILE);
            let hash_manifest = archive.join(EXPORTED_HASH_MANIFEST_FILE);
            copy_verified_capture(capture, &screenshot)?;

            let screenshot_path = absolute_path_string(&screenshot)?;
            let artifact_manifest_path =
                absolute_direct_child_path_string(&archive, &artifact_manifest)?;
            let hash_manifest_path = absolute_direct_child_path_string(&archive, &hash_manifest)?;
            let artifact_paths = vec![
                screenshot_path.clone(),
                artifact_manifest_path.clone(),
                hash_manifest_path.clone(),
            ];
            let manifest = FixtureArtifactManifest {
                schema_version: DIAGNOSTIC_EXPORT_SCHEMA_VERSION,
                fixture_id: response.fixture_id.clone(),
                screenshot_path: screenshot_path.clone(),
                artifact_paths: artifact_paths.clone(),
                screenshot_sha256: capture.sha256.clone(),
                capture_method: response.capture_method.clone(),
                computer_use: response.computer_use.clone(),
                screen_recording: response.screen_recording.clone(),
                presentation_fixture: true,
                authority: "none".to_string(),
                network: "denied".to_string(),
                ui_metadata: response.ui_metadata.clone(),
            };
            let mut manifest_bytes = serde_json::to_vec_pretty(&manifest)
                .context("Unable to encode the retained fixture artifact manifest.")?;
            manifest_bytes.push(b'\n');
            if manifest_bytes.len() > MAX_FIXTURE_RESPONSE_BYTES {
                anyhow::bail!("Retained fixture artifact manifest is too large.");
            }
            write_new_private_export(&artifact_manifest, &manifest_bytes)?;
            if absolute_path_string(&artifact_manifest)? != artifact_manifest_path {
                anyhow::bail!("Retained fixture artifact manifest path is invalid.");
            }

            let screenshot_sha256 = sha256_private_export_file(&screenshot, MAX_CAPTURE_BYTES)?;
            if screenshot_sha256 != capture.sha256 {
                anyhow::bail!("Retained fixture screenshot integrity check failed.");
            }
            let artifact_manifest_sha256 =
                sha256_private_export_file(&artifact_manifest, MAX_FIXTURE_RESPONSE_BYTES as u64)?;
            let hash_manifest_bytes = format!(
                "{screenshot_sha256}  {EXPORTED_CAPTURE_FILE}\n{artifact_manifest_sha256}  {EXPORTED_ARTIFACT_MANIFEST_FILE}\n"
            );
            write_new_private_export(&hash_manifest, hash_manifest_bytes.as_bytes())?;
            validate_owner_only_export_file(&hash_manifest, MAX_FIXTURE_RESPONSE_BYTES as u64)?;
            if absolute_path_string(&hash_manifest)? != hash_manifest_path {
                anyhow::bail!("Retained fixture hash manifest path is invalid.");
            }

            Ok(RetainedFixtureArtifacts {
                screenshot_path,
                artifact_paths,
            })
        })();
        if result.is_err() {
            let _ = fs::remove_dir_all(&archive);
        }
        result
    }

    /// Export a bounded, redacted projection of an already-completed source-tree
    /// XCTest fixture evidence bundle. The source bundle passes the same strict
    /// catalog-attested verifier as the read-only command before this route
    /// creates a new private archive. It never copies raw events, structured
    /// logs, screenshots, accessibility, layout, icons, or visible-copy data;
    /// it also never launches, replays, attaches to, or commands an app.
    pub(super) fn export_retained_fixture_bundle(
        bundle_value: &Path,
        output_value: &Path,
    ) -> anyhow::Result<()> {
        let exported = export_retained_fixture_bundle_contract(bundle_value, output_value)?;
        let verified = exported.verified;
        let report = FixtureBundleExportReport {
            schema_version: DIAGNOSTIC_EXPORT_SCHEMA_VERSION,
            lane: DiagnosticLane::PresentationFixture,
            status: DiagnosticTerminalStatus::Succeeded,
            message: "Exported a bounded redacted canonical zero-authority fixture evidence summary.",
            export_path: exported.output_path,
            sha256: exported.sha256,
            scenario_id: verified.scenario_id,
            scenario_version: verified.scenario_version,
            event_count: verified.event_count,
            evidence: FixtureBundleEvidenceSummary {
                file_count: verified.file_count,
                total_bytes: verified.total_bytes,
                hash_manifest_sha256: verified.hash_manifest_sha256,
            },
            redacted: true,
            exact_archive: false,
            raw_evidence_exported: false,
            presentation_fixture: true,
            authority: "none",
            network: "forbidden",
        };
        print_report(&report)
    }

    /// Verify a retained source-tree XCTest fixture evidence directory without
    /// creating a temporary root, launching a fixture host, replaying a
    /// fixture, or reaching the normal app. The completed directory must
    /// attest exactly to the immutable installed catalog before its local
    /// rendered evidence is accepted.
    pub(super) fn verify_retained_fixture_bundle(bundle_value: &Path) -> anyhow::Result<()> {
        let verified = verify_retained_fixture_bundle_contract(bundle_value)?;
        let report = FixtureBundleVerificationReport {
            schema_version: DIAGNOSTIC_EXPORT_SCHEMA_VERSION,
            lane: DiagnosticLane::PresentationFixture,
            status: DiagnosticTerminalStatus::Succeeded,
            message: format!(
                "Verified retained canonical zero-authority fixture evidence bundle at {}.",
                verified.bundle_path
            ),
            bundle_path: verified.bundle_path,
            scenario_id: verified.scenario_id,
            scenario_version: verified.scenario_version,
            event_count: verified.event_count,
            evidence: FixtureBundleEvidenceSummary {
                file_count: verified.file_count,
                total_bytes: verified.total_bytes,
                hash_manifest_sha256: verified.hash_manifest_sha256,
            },
            presentation_fixture: true,
            authority: "none",
            network: "forbidden",
        };
        print_report(&report)
    }

    /// Compare two retained canonical XCTest fixture evidence directories only
    /// after each has independently passed the strict, catalog-attested
    /// verifier. The report intentionally reveals neither input path nor any
    /// rendered / event evidence.
    pub(super) fn compare_retained_fixture_bundles(
        baseline_value: &Path,
        candidate_value: &Path,
    ) -> anyhow::Result<()> {
        let comparison =
            compare_retained_fixture_bundles_contract(baseline_value, candidate_value)?;
        let report = FixtureBundleComparisonReport {
            schema_version: DIAGNOSTIC_EXPORT_SCHEMA_VERSION,
            lane: DiagnosticLane::PresentationFixture,
            status: DiagnosticTerminalStatus::Succeeded,
            message: "Compared independently verified canonical zero-authority fixture evidence bundles.",
            baseline: fixture_bundle_comparison_identity(&comparison.baseline),
            candidate: fixture_bundle_comparison_identity(&comparison.candidate),
            equivalent: comparison.equivalent,
            changed: comparison.changed,
            presentation_fixture: true,
            authority: "none",
            network: "forbidden",
        };
        print_report(&report)
    }

    fn fixture_bundle_comparison_identity(
        bundle: &VerifiedFixtureBundle,
    ) -> FixtureBundleComparisonIdentity {
        FixtureBundleComparisonIdentity {
            scenario_id: bundle.scenario_id.clone(),
            scenario_version: bundle.scenario_version,
            event_count: bundle.event_count,
            evidence: FixtureBundleEvidenceSummary {
                file_count: bundle.file_count,
                total_bytes: bundle.total_bytes,
                hash_manifest_sha256: bundle.hash_manifest_sha256.clone(),
            },
        }
    }

    fn compare_retained_fixture_bundles_contract(
        baseline_value: &Path,
        candidate_value: &Path,
    ) -> anyhow::Result<FixtureBundleComparison> {
        let baseline = verify_retained_fixture_bundle_contract(baseline_value)?;
        let candidate = verify_retained_fixture_bundle_contract(candidate_value)?;
        let left = &baseline.logical_snapshot;
        let right = &candidate.logical_snapshot;
        let mut changed = Vec::new();

        if left.run_configuration_sha256 != right.run_configuration_sha256 {
            changed.push(FixtureBundleComparisonChange::RunConfiguration);
        }
        if left.result_sha256 != right.result_sha256 {
            changed.push(FixtureBundleComparisonChange::Result);
        }
        if left.authority_sha256 != right.authority_sha256 {
            changed.push(FixtureBundleComparisonChange::Authority);
        }
        if left.animation_sha256 != right.animation_sha256 {
            changed.push(FixtureBundleComparisonChange::Animation);
        }
        if left.event_transcript_sha256 != right.event_transcript_sha256 {
            changed.push(FixtureBundleComparisonChange::EventTranscript);
        }
        if left.structured_log_sha256 != right.structured_log_sha256 {
            changed.push(FixtureBundleComparisonChange::StructuredLog);
        }
        // The legacy comparison walks its evidence directory map in sorted
        // order. Keep the externally visible changed-category order stable.
        for directory in [
            "accessibility",
            "icons",
            "layout",
            "screenshots",
            "visible-copy",
        ] {
            if left.evidence_directory_sha256.get(directory)
                != right.evidence_directory_sha256.get(directory)
            {
                let category = match directory {
                    "accessibility" => FixtureBundleComparisonChange::Accessibility,
                    "icons" => FixtureBundleComparisonChange::Icons,
                    "layout" => FixtureBundleComparisonChange::Layout,
                    "screenshots" => FixtureBundleComparisonChange::Screenshots,
                    "visible-copy" => FixtureBundleComparisonChange::VisibleCopy,
                    _ => unreachable!("fixed fixture evidence directory"),
                };
                changed.push(category);
            }
        }

        Ok(FixtureBundleComparison {
            equivalent: changed.is_empty(),
            baseline,
            candidate,
            changed,
        })
    }

    fn verify_retained_fixture_bundle_contract(
        bundle_value: &Path,
    ) -> anyhow::Result<VerifiedFixtureBundle> {
        let bundle = resolve_owner_selected_export_directory(bundle_value)?;
        let files = collect_retained_fixture_bundle_files(&bundle)?;
        let file_index = files
            .iter()
            .map(|file| (file.relative.as_str(), file))
            .collect::<BTreeMap<_, _>>();
        let manifest = read_retained_fixture_bundle_json(&file_index, "run.json")?;
        let manifest = exact_retained_fixture_bundle_object(
            &manifest,
            &[
                "schemaVersion",
                "protocol",
                "runnerVersion",
                "runID",
                "scenarioID",
                "scenarioVersion",
                "controlMode",
                "stopAfterStep",
                "source",
                "registrySha256",
                "schemaSha256",
                "resolvedOptions",
                "authority",
                "fixture",
                "requiredEvidence",
                "assertions",
                "execution",
            ],
            "Retained fixture run manifest",
        )?;
        let scenario_id = retained_fixture_bundle_string(manifest, "scenarioID", 128)?;
        let contract = super::scenario_registry::catalog_fixture_evidence_contract(scenario_id)?;
        validate_retained_fixture_manifest(manifest, &contract)?;

        if let Some(request) = file_index.get("run-request.json") {
            let request = read_retained_fixture_bundle_json(&file_index, &request.relative)?;
            validate_retained_fixture_request(&request, manifest, &contract)?;
        }

        let result = read_retained_fixture_bundle_json(&file_index, "result.json")?;
        validate_retained_fixture_result(&result, manifest, &contract)?;
        let authority = read_retained_fixture_bundle_json(&file_index, "authority.json")?;
        validate_retained_fixture_authority(&authority, &contract)?;
        let animation = read_retained_fixture_bundle_json(&file_index, "animation.json")?;
        validate_retained_fixture_animation(
            &animation,
            retained_fixture_bundle_value(manifest, "resolvedOptions")?,
        )?;
        let event_count = validate_retained_fixture_events(
            retained_fixture_bundle_file(&file_index, "events.jsonl")?,
            retained_fixture_bundle_file(&file_index, "structured-log.jsonl")?,
            retained_fixture_bundle_string(manifest, "runID", 64)?,
            &contract,
        )?;
        verify_retained_fixture_bundle_hashes(&files, &file_index)?;
        for file in &files {
            scan_retained_fixture_bundle_text(file)?;
        }
        let logical_snapshot = retained_fixture_bundle_logical_snapshot(&files, &file_index)?;

        Ok(VerifiedFixtureBundle {
            bundle_path: absolute_path_string(&bundle)?,
            scenario_id: contract.scenario_id,
            scenario_version: contract.scenario_version,
            event_count,
            file_count: files.len(),
            total_bytes: files.iter().map(|file| file.bytes).sum(),
            hash_manifest_sha256: sha256_private_export_file(
                retained_fixture_bundle_file(&file_index, EXPORTED_HASH_MANIFEST_FILE)?
                    .path
                    .as_path(),
                MAX_LEGACY_FIXTURE_TEXT_BYTES,
            )?,
            logical_snapshot,
        })
    }

    /// Construct a new redacted export only after the source bundle has
    /// passed every structural, catalog, integrity, and secret-marker check.
    /// Unlike the legacy Python archive command, this route deliberately
    /// carries no raw source member into the ZIP: the result is a bounded
    /// aggregate projection that is safe to inspect offline.
    fn export_retained_fixture_bundle_contract(
        bundle_value: &Path,
        output_value: &Path,
    ) -> anyhow::Result<ExportedRedactedFixtureBundle> {
        let verified = verify_retained_fixture_bundle_contract(bundle_value)?;
        let output = resolve_new_redacted_fixture_bundle_export(output_value)?;
        let entries = redacted_fixture_bundle_export_entries(&verified)?;

        let mut created = false;
        let archive_result = (|| {
            let file = create_new_private_export_file(&output)?;
            created = true;
            let mut archive = ZipWriter::new(file);
            let options = SimpleFileOptions::default();
            for (relative, bytes) in &entries {
                archive
                    .start_file(relative, options)
                    .context("Unable to create the redacted fixture bundle export.")?;
                archive
                    .write_all(bytes)
                    .context("Unable to create the redacted fixture bundle export.")?;
            }
            let file = archive
                .finish()
                .context("Unable to finalize the redacted fixture bundle export.")?;
            file.sync_all()
                .context("Unable to finalize the redacted fixture bundle export.")?;
            drop(file);
            validate_owner_only_export_file(&output, MAX_REDACTED_FIXTURE_BUNDLE_EXPORT_BYTES)?;
            assert_redacted_fixture_bundle_export(&output, &entries)
        })();
        if archive_result.is_err() && created {
            // This exact new path was reserved with `create_new`; deleting it
            // prevents a partial local archive from being mistaken for a
            // completed export without touching any pre-existing user file.
            let _ = fs::remove_file(&output);
        }
        archive_result?;

        Ok(ExportedRedactedFixtureBundle {
            output_path: absolute_path_string(&output)?,
            sha256: sha256_private_export_file(&output, MAX_REDACTED_FIXTURE_BUNDLE_EXPORT_BYTES)?,
            verified,
        })
    }

    fn resolve_new_redacted_fixture_bundle_export(output_value: &Path) -> anyhow::Result<PathBuf> {
        let leaf = output_value
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| anyhow::anyhow!("Redacted fixture bundle export path is invalid."))?;
        let Some(stem) = leaf.strip_suffix(".zip") else {
            anyhow::bail!("Redacted fixture bundle export must use a `.zip` filename.");
        };
        if stem.is_empty() || !valid_retained_fixture_bundle_leaf_name(leaf) {
            anyhow::bail!("Redacted fixture bundle export path is invalid.");
        }
        let requested_parent = output_value
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let parent = resolve_owner_selected_export_directory(requested_parent)?;
        let output = parent.join(leaf);
        if !output.is_absolute() || output.parent() != Some(parent.as_path()) {
            anyhow::bail!("Redacted fixture bundle export path is invalid.");
        }
        match fs::symlink_metadata(&output) {
            Ok(_) => anyhow::bail!("Redacted fixture bundle export destination already exists."),
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .context("Redacted fixture bundle export destination is unavailable.");
            }
        }
        Ok(output)
    }

    fn redacted_fixture_bundle_export_entries(
        bundle: &VerifiedFixtureBundle,
    ) -> anyhow::Result<BTreeMap<String, Vec<u8>>> {
        let evidence = serde_json::json!({
            "fileCount": bundle.file_count,
            "totalBytes": bundle.total_bytes,
            "hashManifestSha256": bundle.hash_manifest_sha256,
        });
        let summary = serde_json::json!({
            "schemaVersion": REDACTED_FIXTURE_BUNDLE_EXPORT_SCHEMA_VERSION,
            "scenarioID": bundle.scenario_id,
            "scenarioVersion": bundle.scenario_version,
            "eventCount": bundle.event_count,
            "evidence": evidence,
            "redacted": true,
            "exactArchive": false,
            "rawEvidenceExported": false,
            "presentationFixture": true,
            "authority": "none",
            "network": "forbidden",
        });
        let manifest = serde_json::json!({
            "schemaVersion": REDACTED_FIXTURE_BUNDLE_EXPORT_SCHEMA_VERSION,
            "exportKind": "redacted-fixture-bundle",
            "redacted": true,
            "exactArchive": false,
            "rawEvidenceExported": false,
            "summaryPath": REDACTED_FIXTURE_BUNDLE_EXPORT_SUMMARY_FILE,
            "hashManifestPath": EXPORTED_HASH_MANIFEST_FILE,
            "archiveMemberCount": 3,
            "presentationFixture": true,
            "authority": "none",
            "network": "forbidden",
        });
        let mut entries = BTreeMap::new();
        entries.insert(
            REDACTED_FIXTURE_BUNDLE_EXPORT_MANIFEST_FILE.to_string(),
            redacted_fixture_bundle_export_json(&manifest)?,
        );
        entries.insert(
            REDACTED_FIXTURE_BUNDLE_EXPORT_SUMMARY_FILE.to_string(),
            redacted_fixture_bundle_export_json(&summary)?,
        );
        entries.insert(
            EXPORTED_HASH_MANIFEST_FILE.to_string(),
            redacted_fixture_bundle_export_hash_manifest(&entries),
        );
        Ok(entries)
    }

    fn redacted_fixture_bundle_export_json(value: &serde_json::Value) -> anyhow::Result<Vec<u8>> {
        let mut bytes = serde_json::to_vec(value)
            .context("Unable to encode the redacted fixture bundle export.")?;
        bytes.push(b'\n');
        if u64::try_from(bytes.len())? > MAX_REDACTED_FIXTURE_BUNDLE_EXPORT_BYTES {
            anyhow::bail!("Redacted fixture bundle export is too large.");
        }
        Ok(bytes)
    }

    fn redacted_fixture_bundle_export_hash_manifest(
        entries: &BTreeMap<String, Vec<u8>>,
    ) -> Vec<u8> {
        let mut manifest = String::new();
        for (relative, bytes) in entries {
            manifest.push_str(&sha256_retained_fixture_bundle_bytes(bytes));
            manifest.push_str("  ");
            manifest.push_str(relative);
            manifest.push('\n');
        }
        manifest.into_bytes()
    }

    fn assert_redacted_fixture_bundle_export(
        output: &Path,
        entries: &BTreeMap<String, Vec<u8>>,
    ) -> anyhow::Result<()> {
        validate_owner_only_export_file(output, MAX_REDACTED_FIXTURE_BUNDLE_EXPORT_BYTES)?;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(output)
            .context("Redacted fixture bundle export is unavailable.")?;
        let mut archive = ZipArchive::new(file)
            .context("Redacted fixture bundle export is not a valid ZIP archive.")?;
        if archive.len() != entries.len() {
            anyhow::bail!("Redacted fixture bundle export has an invalid member set.");
        }

        let mut seen = BTreeSet::new();
        for index in 0..archive.len() {
            let mut member = archive
                .by_index(index)
                .context("Redacted fixture bundle export is invalid.")?;
            let relative = member.name().to_string();
            let expected = entries.get(&relative).ok_or_else(|| {
                anyhow::anyhow!("Redacted fixture bundle export has an invalid member set.")
            })?;
            if member.is_dir()
                || member.enclosed_name().is_none()
                || !seen.insert(relative)
                || member.size() != u64::try_from(expected.len())?
            {
                anyhow::bail!("Redacted fixture bundle export is invalid.");
            }

            let mut actual = Vec::with_capacity(expected.len());
            let mut buffer = [0_u8; 8 * 1_024];
            loop {
                let read = member
                    .read(&mut buffer)
                    .context("Redacted fixture bundle export is invalid.")?;
                if read == 0 {
                    break;
                }
                if actual.len().saturating_add(read) > expected.len() {
                    anyhow::bail!("Redacted fixture bundle export is invalid.");
                }
                actual.extend_from_slice(&buffer[..read]);
            }
            if &actual != expected {
                anyhow::bail!("Redacted fixture bundle export integrity check failed.");
            }
        }
        if seen.len() != entries.len() {
            anyhow::bail!("Redacted fixture bundle export has an invalid member set.");
        }
        Ok(())
    }

    fn collect_retained_fixture_bundle_files(
        bundle: &Path,
    ) -> anyhow::Result<Vec<FixtureBundleFile>> {
        const REQUIRED_FILES: [&str; 7] = [
            "run.json",
            "result.json",
            "events.jsonl",
            "structured-log.jsonl",
            "authority.json",
            "animation.json",
            EXPORTED_HASH_MANIFEST_FILE,
        ];
        const OPTIONAL_FILES: [&str; 2] = ["run-request.json", "runner.log"];
        let mut files = Vec::new();
        let mut names = BTreeSet::new();
        let mut total_bytes = 0_u64;
        for entry in fs::read_dir(bundle).context("Retained fixture bundle is unavailable.")? {
            let entry = entry.context("Retained fixture bundle is unavailable.")?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!("Retained fixture bundle has an invalid name."))?;
            if !names.insert(name.clone()) {
                anyhow::bail!("Retained fixture bundle has duplicate members.");
            }
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)
                .context("Retained fixture bundle member is unavailable.")?;
            if metadata.file_type().is_symlink() {
                anyhow::bail!("Retained fixture bundle must not contain symbolic links.");
            }
            if LEGACY_FIXTURE_EVIDENCE_DIRECTORIES.contains(&name.as_str()) {
                validate_owner_only_export_directory(&path)?;
                let mut child_count = 0_usize;
                for child in fs::read_dir(&path)
                    .context("Retained fixture bundle evidence directory is unavailable.")?
                {
                    let child = child
                        .context("Retained fixture bundle evidence directory is unavailable.")?;
                    let child_name = child.file_name().into_string().map_err(|_| {
                        anyhow::anyhow!("Retained fixture bundle has an invalid evidence name.")
                    })?;
                    if !valid_retained_fixture_bundle_leaf_name(&child_name) {
                        anyhow::bail!("Retained fixture bundle has an invalid evidence name.");
                    }
                    let child_path = child.path();
                    let child_metadata = fs::symlink_metadata(&child_path)
                        .context("Retained fixture evidence file is unavailable.")?;
                    if child_metadata.file_type().is_symlink() || !child_metadata.is_file() {
                        anyhow::bail!("Retained fixture evidence must be a direct private file.");
                    }
                    validate_owner_only_export_file(&child_path, MAX_LEGACY_FIXTURE_BUNDLE_BYTES)?;
                    total_bytes = total_bytes
                        .checked_add(child_metadata.len())
                        .ok_or_else(|| anyhow::anyhow!("Retained fixture bundle is too large."))?;
                    files.push(FixtureBundleFile {
                        relative: format!("{name}/{child_name}"),
                        path: child_path,
                        bytes: child_metadata.len(),
                    });
                    child_count += 1;
                }
                if child_count == 0 {
                    anyhow::bail!("Retained fixture bundle is missing required evidence.");
                }
                continue;
            }
            if !REQUIRED_FILES.contains(&name.as_str()) && !OPTIONAL_FILES.contains(&name.as_str())
            {
                anyhow::bail!("Retained fixture bundle contains an unexpected member.");
            }
            if !metadata.is_file() {
                anyhow::bail!("Retained fixture bundle member must be a private file.");
            }
            if name == "runner.log" && metadata.len() == 0 {
                if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0 {
                    anyhow::bail!("Retained fixture bundle member must be owner-only.");
                }
            } else {
                validate_owner_only_export_file(&path, MAX_LEGACY_FIXTURE_BUNDLE_BYTES)?;
            }
            total_bytes = total_bytes
                .checked_add(metadata.len())
                .ok_or_else(|| anyhow::anyhow!("Retained fixture bundle is too large."))?;
            files.push(FixtureBundleFile {
                relative: name,
                path,
                bytes: metadata.len(),
            });
        }
        if total_bytes > MAX_LEGACY_FIXTURE_BUNDLE_BYTES
            || REQUIRED_FILES.iter().any(|name| !names.contains(*name))
            || LEGACY_FIXTURE_EVIDENCE_DIRECTORIES
                .iter()
                .any(|name| !names.contains(*name))
        {
            anyhow::bail!("Retained fixture bundle is incomplete or too large.");
        }
        files.sort_by(|left, right| left.relative.cmp(&right.relative));
        Ok(files)
    }

    fn valid_retained_fixture_bundle_leaf_name(value: &str) -> bool {
        !value.is_empty()
            && value.len() <= 256
            && value.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_uppercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'.' | b'_' | b'-')
            })
    }

    fn retained_fixture_bundle_file<'a>(
        files: &'a BTreeMap<&str, &'a FixtureBundleFile>,
        relative: &str,
    ) -> anyhow::Result<&'a FixtureBundleFile> {
        files
            .get(relative)
            .copied()
            .ok_or_else(|| anyhow::anyhow!("Retained fixture bundle is incomplete."))
    }

    fn read_retained_fixture_bundle_json(
        files: &BTreeMap<&str, &FixtureBundleFile>,
        relative: &str,
    ) -> anyhow::Result<serde_json::Value> {
        let file = retained_fixture_bundle_file(files, relative)?;
        let text = read_retained_fixture_bundle_text(file)?;
        serde_json::from_str(&text)
            .map_err(|_| anyhow::anyhow!("Retained fixture bundle JSON is invalid."))
    }

    fn read_retained_fixture_bundle_text(file: &FixtureBundleFile) -> anyhow::Result<String> {
        if file.bytes > MAX_LEGACY_FIXTURE_TEXT_BYTES {
            anyhow::bail!("Retained fixture text evidence is too large.");
        }
        let bytes =
            fs::read(&file.path).context("Retained fixture text evidence is unavailable.")?;
        String::from_utf8(bytes)
            .map_err(|_| anyhow::anyhow!("Retained fixture text evidence is not UTF-8."))
    }

    fn exact_retained_fixture_bundle_object<'a>(
        value: &'a serde_json::Value,
        expected_fields: &[&str],
        label: &str,
    ) -> anyhow::Result<&'a serde_json::Map<String, serde_json::Value>> {
        let object = value
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("{label} must be an object."))?;
        if object.len() != expected_fields.len()
            || object
                .keys()
                .any(|field| !expected_fields.contains(&field.as_str()))
        {
            anyhow::bail!("{label} has an invalid schema.");
        }
        Ok(object)
    }

    fn retained_fixture_bundle_value<'a>(
        object: &'a serde_json::Map<String, serde_json::Value>,
        field: &str,
    ) -> anyhow::Result<&'a serde_json::Value> {
        object
            .get(field)
            .ok_or_else(|| anyhow::anyhow!("Retained fixture bundle is missing a required field."))
    }

    fn retained_fixture_bundle_string<'a>(
        object: &'a serde_json::Map<String, serde_json::Value>,
        field: &str,
        maximum_bytes: usize,
    ) -> anyhow::Result<&'a str> {
        let value = retained_fixture_bundle_value(object, field)?
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Retained fixture bundle string field is invalid."))?;
        if value.is_empty() || value.len() > maximum_bytes || value.contains('\0') {
            anyhow::bail!("Retained fixture bundle string field is invalid.");
        }
        Ok(value)
    }

    fn retained_fixture_bundle_bool(
        object: &serde_json::Map<String, serde_json::Value>,
        field: &str,
    ) -> anyhow::Result<bool> {
        retained_fixture_bundle_value(object, field)?
            .as_bool()
            .ok_or_else(|| anyhow::anyhow!("Retained fixture bundle boolean field is invalid."))
    }

    fn retained_fixture_bundle_u64(
        object: &serde_json::Map<String, serde_json::Value>,
        field: &str,
    ) -> anyhow::Result<u64> {
        retained_fixture_bundle_value(object, field)?
            .as_u64()
            .ok_or_else(|| anyhow::anyhow!("Retained fixture bundle numeric field is invalid."))
    }

    fn retained_fixture_bundle_string_array(
        object: &serde_json::Map<String, serde_json::Value>,
        field: &str,
    ) -> anyhow::Result<Vec<String>> {
        let values = retained_fixture_bundle_value(object, field)?
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("Retained fixture bundle list field is invalid."))?;
        let mut result = Vec::with_capacity(values.len());
        for value in values {
            let value = value
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Retained fixture bundle list field is invalid."))?;
            if value.is_empty() || value.len() > 256 || value.contains('\0') {
                anyhow::bail!("Retained fixture bundle list field is invalid.");
            }
            result.push(value.to_string());
        }
        if result.iter().collect::<BTreeSet<_>>().len() != result.len() {
            anyhow::bail!("Retained fixture bundle list field is invalid.");
        }
        Ok(result)
    }

    fn validate_retained_fixture_manifest(
        manifest: &serde_json::Map<String, serde_json::Value>,
        contract: &CatalogFixtureEvidenceContract,
    ) -> anyhow::Result<()> {
        if retained_fixture_bundle_u64(manifest, "schemaVersion")?
            != LEGACY_FIXTURE_BUNDLE_SCHEMA_VERSION
            || retained_fixture_bundle_string(manifest, "protocol", 128)?
                != LEGACY_FIXTURE_BUNDLE_PROTOCOL
            || retained_fixture_bundle_string(manifest, "runnerVersion", 64)?
                != contract.runner_version
            || retained_fixture_bundle_string(manifest, "scenarioID", 128)? != contract.scenario_id
            || retained_fixture_bundle_u64(manifest, "scenarioVersion")?
                != u64::from(contract.scenario_version)
            || retained_fixture_bundle_string(manifest, "registrySha256", 64)?
                != contract.registry_sha256
            || retained_fixture_bundle_string(manifest, "schemaSha256", 64)?
                != contract.schema_sha256
            || !valid_lowercase_sha256(retained_fixture_bundle_string(
                manifest,
                "registrySha256",
                64,
            )?)
            || !valid_lowercase_sha256(retained_fixture_bundle_string(
                manifest,
                "schemaSha256",
                64,
            )?)
            || !valid_retained_fixture_run_id(retained_fixture_bundle_string(
                manifest, "runID", 64,
            )?)
        {
            anyhow::bail!("Retained fixture run manifest is not catalog-attested.");
        }
        let expected_authority = serde_json::to_value(&contract.authority)
            .context("Unable to encode the immutable fixture authority contract.")?;
        let expected_fixture = serde_json::to_value(&contract.fixture)
            .context("Unable to encode the immutable fixture state contract.")?;
        if retained_fixture_bundle_value(manifest, "authority")? != &expected_authority
            || retained_fixture_bundle_value(manifest, "fixture")? != &expected_fixture
            || retained_fixture_bundle_string_array(manifest, "requiredEvidence")?
                != contract.required_evidence
            || retained_fixture_bundle_string_array(manifest, "assertions")? != contract.assertions
        {
            anyhow::bail!("Retained fixture run manifest is not catalog-attested.");
        }
        validate_retained_fixture_control(manifest, contract)?;
        validate_retained_fixture_options(retained_fixture_bundle_value(
            manifest,
            "resolvedOptions",
        )?)?;
        validate_retained_fixture_source(retained_fixture_bundle_value(manifest, "source")?)?;
        validate_retained_fixture_execution(retained_fixture_bundle_value(manifest, "execution")?)
    }

    fn validate_retained_fixture_control(
        manifest: &serde_json::Map<String, serde_json::Value>,
        contract: &CatalogFixtureEvidenceContract,
    ) -> anyhow::Result<()> {
        let control_mode = retained_fixture_bundle_string(manifest, "controlMode", 32)?;
        if !matches!(
            control_mode,
            "run" | "record" | "snapshot" | "pause" | "step" | "replay"
        ) {
            anyhow::bail!("Retained fixture control mode is invalid.");
        }
        let stop_after = retained_fixture_bundle_value(manifest, "stopAfterStep")?;
        let stop_after = if stop_after.is_null() {
            None
        } else {
            let value = stop_after
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Retained fixture stop boundary is invalid."))?;
            if !contract.steps.iter().any(|step| step == value) {
                anyhow::bail!("Retained fixture stop boundary is invalid.");
            }
            Some(value)
        };
        if matches!(control_mode, "pause" | "step") != stop_after.is_some()
            || matches!(control_mode, "run" | "record" | "snapshot") && stop_after.is_some()
        {
            anyhow::bail!("Retained fixture control mode is invalid.");
        }
        Ok(())
    }

    fn validate_retained_fixture_options(value: &serde_json::Value) -> anyhow::Result<()> {
        let options = exact_retained_fixture_bundle_object(
            value,
            &[
                "seed",
                "clock",
                "width",
                "height",
                "appearance",
                "reduceMotion",
                "increasedContrast",
                "locale",
            ],
            "Retained fixture options",
        )?;
        let seed = retained_fixture_bundle_u64(options, "seed")?;
        let width = retained_fixture_bundle_u64(options, "width")?;
        let height = retained_fixture_bundle_u64(options, "height")?;
        let appearance = retained_fixture_bundle_string(options, "appearance", 16)?;
        let clock = retained_fixture_bundle_string(options, "clock", 64)?;
        let locale = retained_fixture_bundle_string(options, "locale", 64)?;
        let _ = retained_fixture_bundle_bool(options, "reduceMotion")?;
        let _ = retained_fixture_bundle_bool(options, "increasedContrast")?;
        if seed > 2_147_483_647
            || !(320..=1_920).contains(&width)
            || !(180..=1_440).contains(&height)
            || !matches!(appearance, "light" | "dark")
            || !valid_retained_fixture_timestamp(clock)
            || !valid_fixture_locale(locale)
        {
            anyhow::bail!("Retained fixture options are invalid.");
        }
        Ok(())
    }

    fn validate_retained_fixture_source(value: &serde_json::Value) -> anyhow::Result<()> {
        let source = exact_retained_fixture_bundle_object(
            value,
            &[
                "repository",
                "commit",
                "branch",
                "clean",
                "dirtyPathCount",
                "diffSha256",
            ],
            "Retained fixture source identity",
        )?;
        let repository = retained_fixture_bundle_string(source, "repository", 4_096)?;
        let commit = retained_fixture_bundle_string(source, "commit", 64)?;
        let branch = retained_fixture_bundle_value(source, "branch")?
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Retained fixture source identity is invalid."))?;
        let _ = retained_fixture_bundle_bool(source, "clean")?;
        let dirty_paths = retained_fixture_bundle_u64(source, "dirtyPathCount")?;
        let diff_sha256 = retained_fixture_bundle_string(source, "diffSha256", 64)?;
        if !repository.starts_with('/')
            || !valid_retained_fixture_git_commit(commit)
            || branch.len() > 512
            || branch.contains('\0')
            || dirty_paths > 100_000
            || !valid_lowercase_sha256(diff_sha256)
        {
            anyhow::bail!("Retained fixture source identity is invalid.");
        }
        Ok(())
    }

    fn validate_retained_fixture_execution(value: &serde_json::Value) -> anyhow::Result<()> {
        let execution = exact_retained_fixture_bundle_object(
            value,
            &[
                "adapter",
                "networkSandbox",
                "shellUsed",
                "startedAt",
                "finishedAt",
                "exitCode",
                "timedOut",
                "runnerLogRedactions",
            ],
            "Retained fixture execution evidence",
        )?;
        if retained_fixture_bundle_string(execution, "adapter", 64)? != "swift-xctest"
            || retained_fixture_bundle_string(execution, "networkSandbox", 64)? != "deny-network"
            || retained_fixture_bundle_bool(execution, "shellUsed")?
            || !valid_retained_fixture_timestamp(retained_fixture_bundle_string(
                execution,
                "startedAt",
                64,
            )?)
            || !valid_retained_fixture_timestamp(retained_fixture_bundle_string(
                execution,
                "finishedAt",
                64,
            )?)
            || retained_fixture_bundle_value(execution, "exitCode")?.as_u64() != Some(0)
            || retained_fixture_bundle_bool(execution, "timedOut")?
            || retained_fixture_bundle_u64(execution, "runnerLogRedactions")? > 1_000_000
        {
            anyhow::bail!("Retained fixture execution evidence is invalid.");
        }
        Ok(())
    }

    fn validate_retained_fixture_request(
        value: &serde_json::Value,
        manifest: &serde_json::Map<String, serde_json::Value>,
        contract: &CatalogFixtureEvidenceContract,
    ) -> anyhow::Result<()> {
        let request = exact_retained_fixture_bundle_object(
            value,
            &[
                "protocol",
                "runnerVersion",
                "runID",
                "scenarioID",
                "scenarioVersion",
                "registrySha256",
                "schemaSha256",
                "controlMode",
                "stopAfterStep",
                "options",
                "authority",
                "fixture",
                "requiredEvidence",
                "assertions",
                "capabilitySha256",
            ],
            "Retained fixture request",
        )?;
        for field in [
            "protocol",
            "runnerVersion",
            "runID",
            "scenarioID",
            "scenarioVersion",
            "registrySha256",
            "schemaSha256",
            "controlMode",
            "stopAfterStep",
            "authority",
            "fixture",
            "requiredEvidence",
            "assertions",
        ] {
            if retained_fixture_bundle_value(request, field)?
                != retained_fixture_bundle_value(manifest, field)?
            {
                anyhow::bail!("Retained fixture request does not match its run manifest.");
            }
        }
        if retained_fixture_bundle_value(request, "options")?
            != retained_fixture_bundle_value(manifest, "resolvedOptions")?
            || !valid_lowercase_sha256(retained_fixture_bundle_string(
                request,
                "capabilitySha256",
                64,
            )?)
        {
            anyhow::bail!("Retained fixture request does not match its run manifest.");
        }
        // Reuse the catalog and bounded-option proof instead of trusting that
        // a matching pair of local JSON files is authoritative.
        validate_retained_fixture_manifest(manifest, contract)
    }

    fn validate_retained_fixture_result(
        value: &serde_json::Value,
        manifest: &serde_json::Map<String, serde_json::Value>,
        contract: &CatalogFixtureEvidenceContract,
    ) -> anyhow::Result<()> {
        let result = exact_retained_fixture_bundle_object(
            value,
            &[
                "schemaVersion",
                "status",
                "scenarioID",
                "scenarioVersion",
                "controlMode",
                "stopAfterStep",
                "assertions",
                "pendingAssertions",
                "allRequestedAssertionsEvaluated",
                "safeModelProgressOnly",
                "hiddenReasoningCaptured",
                "providerRoute",
                "modelRoute",
            ],
            "Retained fixture result",
        )?;
        for field in [
            "scenarioID",
            "scenarioVersion",
            "controlMode",
            "stopAfterStep",
        ] {
            if retained_fixture_bundle_value(result, field)?
                != retained_fixture_bundle_value(manifest, field)?
            {
                anyhow::bail!("Retained fixture result does not match its run manifest.");
            }
        }
        let status = retained_fixture_bundle_string(result, "status", 32)?;
        let control_mode = retained_fixture_bundle_string(manifest, "controlMode", 32)?;
        let expected_status = match control_mode {
            "pause" => "paused",
            "step" => "stepped",
            _ => "passed",
        };
        if retained_fixture_bundle_u64(result, "schemaVersion")?
            != LEGACY_FIXTURE_BUNDLE_SCHEMA_VERSION
            || status != expected_status
            || !retained_fixture_bundle_bool(result, "safeModelProgressOnly")?
            || retained_fixture_bundle_bool(result, "hiddenReasoningCaptured")?
            || retained_fixture_bundle_string(result, "providerRoute", 64)?
                != "none-isolated-fixture"
            || retained_fixture_bundle_string(result, "modelRoute", 64)? != "none-isolated-fixture"
        {
            anyhow::bail!("Retained fixture result is invalid.");
        }

        let assertions = retained_fixture_bundle_value(result, "assertions")?
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("Retained fixture assertion evidence is invalid."))?;
        let pending = retained_fixture_bundle_string_array(result, "pendingAssertions")?;
        let requested = contract.assertions.iter().collect::<BTreeSet<_>>();
        let evaluated = assertions.keys().collect::<BTreeSet<_>>();
        let pending_set = pending.iter().collect::<BTreeSet<_>>();
        if assertions.len() > contract.assertions.len()
            || evaluated.iter().any(|name| !requested.contains(name))
            || pending_set.iter().any(|name| !requested.contains(name))
            || !evaluated.is_disjoint(&pending_set)
            || evaluated.union(&pending_set).count() != requested.len()
            || assertions
                .values()
                .any(|value| value.as_bool() != Some(true))
            || retained_fixture_bundle_bool(result, "allRequestedAssertionsEvaluated")?
                != pending.is_empty()
            || status == "passed" && !pending.is_empty()
        {
            anyhow::bail!("Retained fixture assertion evidence is invalid.");
        }
        Ok(())
    }

    fn validate_retained_fixture_authority(
        value: &serde_json::Value,
        contract: &CatalogFixtureEvidenceContract,
    ) -> anyhow::Result<()> {
        let authority = exact_retained_fixture_bundle_object(
            value,
            &[
                "schemaVersion",
                "host",
                "network",
                "networkSandboxEnvironment",
                "providerRequests",
                "commercialAuthority",
                "productionMutation",
                "fixtureOnly",
                "supabaseSessionMinted",
                "subscriptionMinted",
                "entitlementMinted",
                "fundingAuthorityMinted",
                "usageReservationCreated",
                "providerRequestCreated",
            ],
            "Retained fixture authority evidence",
        )?;
        let expected = serde_json::to_value(&contract.authority)
            .context("Unable to encode the immutable fixture authority contract.")?;
        let expected = expected
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("Immutable fixture authority contract is invalid."))?;
        for field in [
            "host",
            "network",
            "providerRequests",
            "commercialAuthority",
            "productionMutation",
        ] {
            if retained_fixture_bundle_value(authority, field)?
                != expected.get(field).ok_or_else(|| {
                    anyhow::anyhow!("Immutable fixture authority contract is invalid.")
                })?
            {
                anyhow::bail!("Retained fixture authority evidence is invalid.");
            }
        }
        if retained_fixture_bundle_u64(authority, "schemaVersion")?
            != LEGACY_FIXTURE_BUNDLE_SCHEMA_VERSION
            || !retained_fixture_bundle_bool(authority, "networkSandboxEnvironment")?
            || !retained_fixture_bundle_bool(authority, "fixtureOnly")?
            || retained_fixture_bundle_bool(authority, "supabaseSessionMinted")?
            || retained_fixture_bundle_bool(authority, "subscriptionMinted")?
            || retained_fixture_bundle_bool(authority, "entitlementMinted")?
            || retained_fixture_bundle_bool(authority, "fundingAuthorityMinted")?
            || retained_fixture_bundle_bool(authority, "usageReservationCreated")?
            || retained_fixture_bundle_bool(authority, "providerRequestCreated")?
        {
            anyhow::bail!("Retained fixture authority evidence is invalid.");
        }
        Ok(())
    }

    fn validate_retained_fixture_animation(
        value: &serde_json::Value,
        options_value: &serde_json::Value,
    ) -> anyhow::Result<()> {
        let animation = exact_retained_fixture_bundle_object(
            value,
            &[
                "schemaVersion",
                "reduceMotion",
                "increasedContrast",
                "frames",
            ],
            "Retained fixture animation evidence",
        )?;
        let options = options_value
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("Retained fixture options are invalid."))?;
        let reduce_motion = retained_fixture_bundle_bool(options, "reduceMotion")?;
        let increased_contrast = retained_fixture_bundle_bool(options, "increasedContrast")?;
        if retained_fixture_bundle_u64(animation, "schemaVersion")?
            != LEGACY_FIXTURE_BUNDLE_SCHEMA_VERSION
            || retained_fixture_bundle_bool(animation, "reduceMotion")? != reduce_motion
            || retained_fixture_bundle_bool(animation, "increasedContrast")? != increased_contrast
        {
            anyhow::bail!("Retained fixture animation evidence is invalid.");
        }
        let frames = retained_fixture_bundle_value(animation, "frames")?
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("Retained fixture animation evidence is invalid."))?;
        if frames.is_empty() || frames.len() > 1_024 {
            anyhow::bail!("Retained fixture animation evidence is invalid.");
        }
        for (frame, state) in frames {
            if !valid_retained_fixture_bundle_identifier(frame) {
                anyhow::bail!("Retained fixture animation evidence is invalid.");
            }
            let state = exact_retained_fixture_bundle_object(
                state,
                &["animatedProgressActive", "decorativeMotionAllowed"],
                "Retained fixture animation frame",
            )?;
            let _ = retained_fixture_bundle_bool(state, "animatedProgressActive")?;
            if retained_fixture_bundle_bool(state, "decorativeMotionAllowed")? == reduce_motion {
                anyhow::bail!("Retained fixture animation evidence is invalid.");
            }
        }
        Ok(())
    }

    fn validate_retained_fixture_events(
        events_file: &FixtureBundleFile,
        structured_log_file: &FixtureBundleFile,
        run_id: &str,
        contract: &CatalogFixtureEvidenceContract,
    ) -> anyhow::Result<usize> {
        let events_text = read_retained_fixture_bundle_text(events_file)?;
        let mut events = Vec::new();
        for line in events_text.lines().filter(|line| !line.trim().is_empty()) {
            let event: serde_json::Value = serde_json::from_str(line)
                .map_err(|_| anyhow::anyhow!("Retained fixture event transcript is invalid."))?;
            let event = event
                .as_object()
                .ok_or_else(|| anyhow::anyhow!("Retained fixture event transcript is invalid."))?;
            let permitted_with_target = [
                "sequence",
                "runID",
                "scenarioID",
                "type",
                "step",
                "state",
                "title",
                "target",
                "occurredAt",
            ];
            let permitted_without_target = [
                "sequence",
                "runID",
                "scenarioID",
                "type",
                "step",
                "state",
                "title",
                "occurredAt",
            ];
            if (event.len() != permitted_with_target.len()
                || event
                    .keys()
                    .any(|field| !permitted_with_target.contains(&field.as_str())))
                && (event.len() != permitted_without_target.len()
                    || event
                        .keys()
                        .any(|field| !permitted_without_target.contains(&field.as_str())))
            {
                anyhow::bail!("Retained fixture event transcript is invalid.");
            }
            let sequence = retained_fixture_bundle_u64(event, "sequence")?;
            if sequence != u64::try_from(events.len() + 1).unwrap_or(u64::MAX)
                || retained_fixture_bundle_string(event, "runID", 64)? != run_id
                || retained_fixture_bundle_string(event, "scenarioID", 128)? != contract.scenario_id
                || !valid_retained_fixture_bundle_identifier(retained_fixture_bundle_string(
                    event, "type", 128,
                )?)
                || !valid_retained_fixture_bundle_identifier(retained_fixture_bundle_string(
                    event, "step", 128,
                )?)
                || !valid_retained_fixture_bundle_identifier(retained_fixture_bundle_string(
                    event, "state", 64,
                )?)
                || !retained_fixture_bundle_value(event, "title")?
                    .as_str()
                    .is_some_and(|title| valid_retained_fixture_event_copy(title, 240))
                || !valid_retained_fixture_timestamp(retained_fixture_bundle_string(
                    event,
                    "occurredAt",
                    64,
                )?)
            {
                anyhow::bail!("Retained fixture event transcript is invalid.");
            }
            if let Some(target) = event.get("target") {
                if !target.is_null() {
                    let target = target.as_str().ok_or_else(|| {
                        anyhow::anyhow!("Retained fixture event transcript is invalid.")
                    })?;
                    if !valid_retained_fixture_event_copy(target, 160) {
                        anyhow::bail!("Retained fixture event transcript is invalid.");
                    }
                }
            }
            events.push((
                retained_fixture_bundle_string(event, "type", 128)?.to_string(),
                retained_fixture_bundle_string(event, "state", 64)?.to_string(),
            ));
        }
        if events.is_empty() || events.len() > 4_096 {
            anyhow::bail!("Retained fixture event transcript is invalid.");
        }

        let structured_text = read_retained_fixture_bundle_text(structured_log_file)?;
        let mut structured = Vec::new();
        for line in structured_text
            .lines()
            .filter(|line| !line.trim().is_empty())
        {
            let entry: serde_json::Value = serde_json::from_str(line)
                .map_err(|_| anyhow::anyhow!("Retained fixture structured log is invalid."))?;
            let entry = exact_retained_fixture_bundle_object(
                &entry,
                &["event", "sequence", "state"],
                "Retained fixture structured log entry",
            )?;
            structured.push((
                retained_fixture_bundle_string(entry, "event", 128)?.to_string(),
                retained_fixture_bundle_u64(entry, "sequence")?,
                retained_fixture_bundle_string(entry, "state", 64)?.to_string(),
            ));
        }
        if structured.len() != events.len()
            || structured.iter().enumerate().any(|(index, entry)| {
                entry.0 != events[index].0
                    || entry.1 != u64::try_from(index + 1).unwrap_or(u64::MAX)
                    || entry.2 != events[index].1
            })
        {
            anyhow::bail!("Retained fixture structured log is invalid.");
        }
        Ok(events.len())
    }

    fn verify_retained_fixture_bundle_hashes(
        files: &[FixtureBundleFile],
        file_index: &BTreeMap<&str, &FixtureBundleFile>,
    ) -> anyhow::Result<()> {
        let hash_file = retained_fixture_bundle_file(file_index, EXPORTED_HASH_MANIFEST_FILE)?;
        let text = read_retained_fixture_bundle_text(hash_file)?;
        if !text.ends_with('\n') || text.ends_with("\n\n") {
            anyhow::bail!("Retained fixture hash manifest is invalid.");
        }
        let mut expected = BTreeMap::new();
        for line in text.lines() {
            let (digest, relative) = line
                .split_once("  ")
                .ok_or_else(|| anyhow::anyhow!("Retained fixture hash manifest is invalid."))?;
            if !valid_lowercase_sha256(digest)
                || relative.is_empty()
                || relative.starts_with('/')
                || relative
                    .split('/')
                    .any(|part| part.is_empty() || part == "." || part == "..")
                || expected
                    .insert(relative.to_string(), digest.to_string())
                    .is_some()
            {
                anyhow::bail!("Retained fixture hash manifest is invalid.");
            }
        }
        let actual = files
            .iter()
            .filter(|file| file.relative != EXPORTED_HASH_MANIFEST_FILE)
            .map(|file| file.relative.as_str())
            .collect::<BTreeSet<_>>();
        let expected_paths = expected.keys().map(String::as_str).collect::<BTreeSet<_>>();
        if expected_paths != actual {
            anyhow::bail!("Retained fixture hash manifest path set differs from evidence.");
        }
        for file in files
            .iter()
            .filter(|file| file.relative != EXPORTED_HASH_MANIFEST_FILE)
        {
            let actual_hash =
                sha256_private_export_file(&file.path, MAX_LEGACY_FIXTURE_BUNDLE_BYTES)?;
            if expected
                .get(&file.relative)
                .is_none_or(|hash| hash != &actual_hash)
            {
                anyhow::bail!("Retained fixture evidence integrity check failed.");
            }
        }
        Ok(())
    }

    /// Build the same logical comparison form as the legacy fixture tool,
    /// but retain only SHA-256 values. The legacy envelope intentionally
    /// ignores fresh run/source/execution/control correlation fields while
    /// retaining every scenario, option, authority, event, and evidence fact.
    fn retained_fixture_bundle_logical_snapshot(
        files: &[FixtureBundleFile],
        file_index: &BTreeMap<&str, &FixtureBundleFile>,
    ) -> anyhow::Result<FixtureBundleLogicalSnapshot> {
        let mut run = read_retained_fixture_bundle_json(file_index, "run.json")?;
        let run_object = run
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("Retained fixture run manifest must be an object."))?;
        for field in ["runID", "source", "execution", "controlMode"] {
            run_object.remove(field);
        }

        let mut result = read_retained_fixture_bundle_json(file_index, "result.json")?;
        result
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("Retained fixture result must be an object."))?
            .remove("controlMode");

        let mut events = Vec::new();
        let event_text = read_retained_fixture_bundle_text(retained_fixture_bundle_file(
            file_index,
            "events.jsonl",
        )?)?;
        for line in event_text.lines().filter(|line| !line.trim().is_empty()) {
            let mut event: serde_json::Value = serde_json::from_str(line)
                .map_err(|_| anyhow::anyhow!("Retained fixture event transcript is invalid."))?;
            event
                .as_object_mut()
                .ok_or_else(|| anyhow::anyhow!("Retained fixture event transcript is invalid."))?
                .remove("runID");
            events.push(event);
        }

        let structured_log = read_retained_fixture_bundle_text(retained_fixture_bundle_file(
            file_index,
            "structured-log.jsonl",
        )?)?;
        let mut evidence_directory_sha256 = BTreeMap::new();
        for directory in LEGACY_FIXTURE_EVIDENCE_DIRECTORIES {
            evidence_directory_sha256.insert(
                directory,
                retained_fixture_bundle_directory_sha256(files, directory)?,
            );
        }

        Ok(FixtureBundleLogicalSnapshot {
            run_configuration_sha256: retained_fixture_bundle_json_sha256(&run)?,
            result_sha256: retained_fixture_bundle_json_sha256(&result)?,
            authority_sha256: retained_fixture_bundle_json_sha256(
                &read_retained_fixture_bundle_json(file_index, "authority.json")?,
            )?,
            animation_sha256: retained_fixture_bundle_json_sha256(
                &read_retained_fixture_bundle_json(file_index, "animation.json")?,
            )?,
            event_transcript_sha256: retained_fixture_bundle_json_sha256(
                &serde_json::Value::Array(events),
            )?,
            structured_log_sha256: sha256_retained_fixture_bundle_bytes(structured_log.as_bytes()),
            evidence_directory_sha256,
        })
    }

    fn retained_fixture_bundle_directory_sha256(
        files: &[FixtureBundleFile],
        directory: &'static str,
    ) -> anyhow::Result<String> {
        let prefix = format!("{directory}/");
        let mut digest = Sha256::new();
        let mut count = 0_usize;
        for file in files {
            let Some(relative) = file.relative.strip_prefix(&prefix) else {
                continue;
            };
            if relative.is_empty() || relative.contains('/') {
                anyhow::bail!("Retained fixture evidence directory is invalid.");
            }
            let hash = sha256_private_export_file(&file.path, MAX_LEGACY_FIXTURE_BUNDLE_BYTES)?;
            digest.update(relative.as_bytes());
            digest.update([0_u8]);
            digest.update(hash.as_bytes());
            digest.update([b'\n']);
            count += 1;
        }
        if count == 0 {
            anyhow::bail!("Retained fixture bundle is missing required evidence.");
        }
        Ok(hex_encode(digest.finalize()))
    }

    fn retained_fixture_bundle_json_sha256(value: &serde_json::Value) -> anyhow::Result<String> {
        let mut bytes = Vec::new();
        canonical_retained_fixture_bundle_json(value, &mut bytes)?;
        Ok(sha256_retained_fixture_bundle_bytes(&bytes))
    }

    fn canonical_retained_fixture_bundle_json(
        value: &serde_json::Value,
        bytes: &mut Vec<u8>,
    ) -> anyhow::Result<()> {
        match value {
            serde_json::Value::Null => bytes.extend_from_slice(b"null"),
            serde_json::Value::Bool(value) => {
                bytes.extend_from_slice(if *value { b"true" } else { b"false" });
            }
            serde_json::Value::Number(value) => {
                bytes.extend_from_slice(value.to_string().as_bytes())
            }
            serde_json::Value::String(value) => {
                serde_json::to_writer(&mut *bytes, value)
                    .context("Unable to normalize retained fixture JSON.")?;
            }
            serde_json::Value::Array(values) => {
                bytes.push(b'[');
                for (index, value) in values.iter().enumerate() {
                    if index > 0 {
                        bytes.push(b',');
                    }
                    canonical_retained_fixture_bundle_json(value, bytes)?;
                }
                bytes.push(b']');
            }
            serde_json::Value::Object(values) => {
                bytes.push(b'{');
                let mut fields = values.keys().collect::<Vec<_>>();
                fields.sort_unstable();
                for (index, field) in fields.into_iter().enumerate() {
                    if index > 0 {
                        bytes.push(b',');
                    }
                    serde_json::to_writer(&mut *bytes, field.as_str())
                        .context("Unable to normalize retained fixture JSON.")?;
                    bytes.push(b':');
                    canonical_retained_fixture_bundle_json(
                        values.get(field).ok_or_else(|| {
                            anyhow::anyhow!("Retained fixture JSON changed while normalizing.")
                        })?,
                        bytes,
                    )?;
                }
                bytes.push(b'}');
            }
        }
        Ok(())
    }

    fn sha256_retained_fixture_bundle_bytes(bytes: &[u8]) -> String {
        hex_encode(Sha256::digest(bytes))
    }

    fn scan_retained_fixture_bundle_text(file: &FixtureBundleFile) -> anyhow::Result<()> {
        let extension = file.path.extension().and_then(|value| value.to_str());
        if !matches!(extension, Some("json" | "jsonl" | "log" | "txt" | "sha256")) {
            return Ok(());
        }
        let text = read_retained_fixture_bundle_text(file)?;
        let lower = text.to_ascii_lowercase();
        let sensitive_markers = [
            "sk_live_",
            "sk_test_",
            "sk-or-v1-",
            "bearer ",
            "authorization:",
            "-----begin rsa private key-----",
            "-----begin ec private key-----",
            "-----begin openssh private key-----",
            "-----begin private key-----",
            "<hidden_reasoning",
            "<analysis",
            "<system",
            "<developer",
            "<private_policy",
        ];
        if sensitive_markers
            .iter()
            .any(|marker| lower.contains(marker))
        {
            anyhow::bail!("Retained fixture evidence contains secret or private-reasoning text.");
        }
        Ok(())
    }

    fn valid_retained_fixture_bundle_identifier(value: &str) -> bool {
        !value.is_empty()
            && value.len() <= 128
            && value.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'.' | b'_' | b'-')
            })
    }

    fn valid_retained_fixture_event_copy(value: &str, maximum_characters: usize) -> bool {
        !value.is_empty()
            && value.len() <= maximum_characters.saturating_mul(4)
            && value.chars().count() <= maximum_characters
            && !value.contains('\0')
    }

    fn valid_retained_fixture_timestamp(value: &str) -> bool {
        (20..=64).contains(&value.len())
            && value.ends_with('Z')
            && value.contains('T')
            && value.bytes().all(|byte| {
                byte.is_ascii_digit() || matches!(byte, b'-' | b':' | b'.' | b'T' | b'Z')
            })
    }

    fn valid_retained_fixture_run_id(value: &str) -> bool {
        Uuid::parse_str(value)
            .ok()
            .is_some_and(|parsed| parsed.to_string() == value)
    }

    fn valid_retained_fixture_git_commit(value: &str) -> bool {
        matches!(value.len(), 40 | 64)
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }

    /// Verify a pre-existing fixture archive without creating a temporary
    /// root, launching the fixture host, or reaching the normal app. This is
    /// deliberately the same owner-only archive format emitted above, so the
    /// public CLI can attest retained evidence independently of the legacy
    /// Python controller.
    pub(super) fn verify_retained_fixture_archive(archive_value: &Path) -> anyhow::Result<()> {
        let verified = verify_retained_fixture_archive_contract(archive_value)?;
        let report = FixtureArchiveVerificationReport {
            schema_version: DIAGNOSTIC_EXPORT_SCHEMA_VERSION,
            lane: DiagnosticLane::PresentationFixture,
            status: DiagnosticTerminalStatus::Succeeded,
            message: format!(
                "Verified retained zero-authority fixture archive at {}.",
                verified.archive_path
            ),
            archive_path: verified.archive_path,
            fixture_id: verified.fixture_id,
            screenshot_path: verified.screenshot_path,
            screenshot_sha256: verified.screenshot_sha256,
            presentation_fixture: true,
            authority: "none",
            network: "denied",
            capture_method: SELF_CAPTURE_METHOD,
            computer_use: SELF_CAPTURE_NOT_USED,
            screen_recording: SELF_CAPTURE_NOT_USED,
        };
        print_report(&report)
    }

    fn verify_retained_fixture_archive_contract(
        archive_value: &Path,
    ) -> anyhow::Result<VerifiedFixtureArchive> {
        let archive = resolve_owner_selected_export_directory(archive_value)?;
        let screenshot = archive.join(EXPORTED_CAPTURE_FILE);
        let artifact_manifest = archive.join(EXPORTED_ARTIFACT_MANIFEST_FILE);
        let hash_manifest = archive.join(EXPORTED_HASH_MANIFEST_FILE);
        verify_exact_fixture_archive_members(
            &archive,
            &screenshot,
            &artifact_manifest,
            &hash_manifest,
        )?;

        let manifest_bytes = fs::read(&artifact_manifest)
            .context("Retained fixture artifact manifest is unavailable.")?;
        let manifest: FixtureArtifactManifest = serde_json::from_slice(&manifest_bytes)
            .context("Retained fixture artifact manifest is invalid.")?;
        let screenshot_path = absolute_direct_child_path_string(&archive, &screenshot)?;
        let artifact_manifest_path =
            absolute_direct_child_path_string(&archive, &artifact_manifest)?;
        let hash_manifest_path = absolute_direct_child_path_string(&archive, &hash_manifest)?;
        let expected_paths = vec![
            screenshot_path.clone(),
            artifact_manifest_path,
            hash_manifest_path,
        ];
        if manifest.schema_version != DIAGNOSTIC_EXPORT_SCHEMA_VERSION
            || manifest.screenshot_path != screenshot_path
            || manifest.artifact_paths != expected_paths
            || manifest.capture_method != SELF_CAPTURE_METHOD
            || manifest.computer_use != SELF_CAPTURE_NOT_USED
            || manifest.screen_recording != SELF_CAPTURE_NOT_USED
            || !manifest.presentation_fixture
            || manifest.authority != "none"
            || manifest.network != "denied"
            || !fixture_archive_metadata_is_consistent(&manifest)
        {
            anyhow::bail!("Retained fixture artifact manifest is invalid.");
        }

        let screenshot_sha256 = sha256_private_export_file(&screenshot, MAX_CAPTURE_BYTES)?;
        let artifact_manifest_sha256 =
            sha256_private_export_file(&artifact_manifest, MAX_FIXTURE_RESPONSE_BYTES as u64)?;
        let (expected_screenshot_sha256, expected_manifest_sha256) =
            parse_retained_fixture_hash_manifest(&hash_manifest)?;
        if manifest.screenshot_sha256 != screenshot_sha256
            || expected_screenshot_sha256 != screenshot_sha256
            || expected_manifest_sha256 != artifact_manifest_sha256
        {
            anyhow::bail!("Retained fixture archive integrity check failed.");
        }

        Ok(VerifiedFixtureArchive {
            archive_path: absolute_path_string(&archive)?,
            fixture_id: manifest.fixture_id,
            screenshot_path,
            screenshot_sha256,
        })
    }

    fn verify_exact_fixture_archive_members(
        archive: &Path,
        screenshot: &Path,
        artifact_manifest: &Path,
        hash_manifest: &Path,
    ) -> anyhow::Result<()> {
        let mut names = BTreeSet::new();
        for entry in fs::read_dir(archive).context("Retained fixture archive is unavailable.")? {
            let entry = entry.context("Retained fixture archive is unavailable.")?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!("Retained fixture archive has an invalid name."))?;
            if !names.insert(name) {
                anyhow::bail!("Retained fixture archive has duplicate artifact names.");
            }
        }
        let expected_names = BTreeSet::from([
            EXPORTED_CAPTURE_FILE.to_string(),
            EXPORTED_ARTIFACT_MANIFEST_FILE.to_string(),
            EXPORTED_HASH_MANIFEST_FILE.to_string(),
        ]);
        if names != expected_names {
            anyhow::bail!("Retained fixture archive contains unexpected artifacts.");
        }
        validate_owner_only_export_file(screenshot, MAX_CAPTURE_BYTES)?;
        validate_owner_only_export_file(artifact_manifest, MAX_FIXTURE_RESPONSE_BYTES as u64)?;
        validate_owner_only_export_file(hash_manifest, MAX_FIXTURE_RESPONSE_BYTES as u64)
    }

    fn parse_retained_fixture_hash_manifest(path: &Path) -> anyhow::Result<(String, String)> {
        validate_owner_only_export_file(path, MAX_FIXTURE_RESPONSE_BYTES as u64)?;
        let bytes = fs::read(path).context("Retained fixture hash manifest is unavailable.")?;
        if !bytes.ends_with(b"\n") {
            anyhow::bail!("Retained fixture hash manifest is invalid.");
        }
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| anyhow::anyhow!("Retained fixture hash manifest is invalid."))?;
        let mut lines = text.lines();
        let screenshot = parse_retained_fixture_hash_line(
            lines
                .next()
                .ok_or_else(|| anyhow::anyhow!("Retained fixture hash manifest is invalid."))?,
            EXPORTED_CAPTURE_FILE,
        )?;
        let artifact_manifest = parse_retained_fixture_hash_line(
            lines
                .next()
                .ok_or_else(|| anyhow::anyhow!("Retained fixture hash manifest is invalid."))?,
            EXPORTED_ARTIFACT_MANIFEST_FILE,
        )?;
        if lines.next().is_some() {
            anyhow::bail!("Retained fixture hash manifest is invalid.");
        }
        Ok((screenshot, artifact_manifest))
    }

    fn parse_retained_fixture_hash_line(line: &str, expected_name: &str) -> anyhow::Result<String> {
        let (digest, name) = line
            .split_once("  ")
            .ok_or_else(|| anyhow::anyhow!("Retained fixture hash manifest is invalid."))?;
        if !valid_lowercase_sha256(digest) || name != expected_name {
            anyhow::bail!("Retained fixture hash manifest is invalid.");
        }
        Ok(digest.to_string())
    }

    fn fixture_archive_metadata_is_consistent(manifest: &FixtureArtifactManifest) -> bool {
        let requested = &manifest.ui_metadata.requested;
        let effective = &manifest.ui_metadata.effective;
        let requested_appearance = requested.appearance.as_str();
        let requested_font_scale = requested.font_scale.as_str();
        let expected_dynamic_type_size = match requested_font_scale {
            "small" => "small",
            "standard" => "large",
            "large" => "xxx_large",
            _ => return false,
        };
        let capture_matches = match requested.capture_kind.as_str() {
            "initial" | "terminal" => requested.capture_event_id.is_none(),
            "named_event" => requested
                .capture_event_id
                .as_deref()
                .is_some_and(valid_fixture_archive_identifier),
            _ => false,
        };
        let requested_dimensions_valid = (1..=8_192).contains(&requested.content_width_points)
            && (1..=8_192).contains(&requested.content_height_points);
        let expected_width = f64::from(requested.content_width_points);
        let expected_height = f64::from(requested.content_height_points);
        let display_matches = if effective.display_available {
            effective
                .display_identifier
                .is_some_and(|identifier| identifier != 0)
        } else {
            effective.display_identifier.is_none()
        };
        let requested_display_matches = requested.display_identifier.is_none_or(|identifier| {
            effective.display_available && effective.display_identifier == Some(identifier)
        });
        let requested_backing_scale_matches =
            requested.backing_scale_milli.is_none_or(|expected| {
                requested_display_matches
                    && backing_scale_milli(effective.backing_scale_factor) == Some(expected)
            });
        let maximum_pixel_width = u32::from(requested.content_width_points).saturating_mul(8);
        let maximum_pixel_height = u32::from(requested.content_height_points).saturating_mul(8);

        manifest.ui_metadata.schema_version == FIXTURE_UI_METADATA_SCHEMA_VERSION
            && valid_fixture_archive_identifier(&manifest.fixture_id)
            && manifest.fixture_id == requested.fixture_id
            && requested.fixture_id == effective.fixture_id
            && valid_fixture_archive_identifier(&requested.surface_id)
            && requested.surface_id == effective.surface_id
            && valid_fixture_archive_identifier(&requested.state_id)
            && requested.state_id == effective.state_id
            && requested_dimensions_valid
            && matches!(requested_appearance, "light" | "dark" | "auto")
            && matches!(
                requested.theme.as_str(),
                "system"
                    | "light"
                    | "dark"
                    | "midnight"
                    | "graphite"
                    | "meadow"
                    | "sunset"
                    | "dawn"
                    | "linen"
                    | "rainbow"
                    | "galaxy"
                    | "aurora"
                    | "ocean"
                    | "ember"
                    | "sol"
            )
            && valid_fixture_locale(&requested.locale)
            && matches!(
                requested.layout_direction.as_str(),
                "left_to_right" | "right_to_left"
            )
            && capture_matches
            && effective.content_frame_points.x.is_finite()
            && effective.content_frame_points.y.is_finite()
            && effective.content_frame_points.width.is_finite()
            && effective.content_frame_points.height.is_finite()
            && effective.content_frame_points.x == 0.0
            && effective.content_frame_points.y == 0.0
            && effective.content_frame_points.width == expected_width
            && effective.content_frame_points.height == expected_height
            && effective.capture_pixel_width >= u32::from(requested.content_width_points)
            && effective.capture_pixel_width <= maximum_pixel_width
            && effective.capture_pixel_height >= u32::from(requested.content_height_points)
            && effective.capture_pixel_height <= maximum_pixel_height
            && matches!(
                effective.appearance.as_str(),
                "light" | "dark" | "unresolved"
            )
            && fixture_appearance_matches(requested_appearance, effective.appearance.as_str())
            && effective.theme == requested.theme
            && effective.locale == requested.locale
            && effective.layout_direction == requested.layout_direction
            && effective.font_scale == requested.font_scale
            && effective.applied_presentation.layout_direction == requested.layout_direction
            && effective.applied_presentation.font_scale == requested.font_scale
            && effective.applied_presentation.dynamic_type_size == expected_dynamic_type_size
            && effective.applied_presentation.reduce_motion == requested.reduce_motion
            && effective.applied_presentation.increased_contrast == requested.increased_contrast
            && effective.applied_presentation.contrast_gain
                == fixture_contrast_gain(requested.increased_contrast)
            && display_matches
            && requested_display_matches
            && effective.backing_scale_factor.is_finite()
            && (0.5..=8.0).contains(&effective.backing_scale_factor)
            && requested_backing_scale_matches
            && effective.accessibility_identifier == FIXTURE_HOST_ACCESSIBILITY_IDENTIFIER
            && effective.accessibility_label == PRESENTATION_FIXTURE_MARKER
            && fixture_window_metadata_matches(
                &effective.window_metadata,
                effective,
                expected_width,
                expected_height,
            )
            && fixture_rendered_icon_registry_is_well_formed(&effective.rendered_icon_registry)
    }

    fn valid_fixture_archive_identifier(value: &str) -> bool {
        !value.is_empty()
            && value.len() <= 128
            && value.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'.' | b'_' | b'-')
            })
    }

    fn valid_fixture_locale(value: &str) -> bool {
        !value.is_empty()
            && value.len() <= 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    }

    fn fixture_rendered_icon_registry_is_well_formed(
        registry: &FixtureRenderedIconRegistry,
    ) -> bool {
        if registry.registry.len() > 128 || registry.rendered_icon_ids.len() > 1_024 {
            return false;
        }
        let ids = registry
            .registry
            .iter()
            .map(|entry| entry.registry_id.as_str())
            .collect::<BTreeSet<_>>();
        ids.len() == registry.registry.len()
            && registry.registry.iter().all(|entry| {
                valid_fixture_archive_identifier(&entry.registry_id)
                    && !entry.asset.is_empty()
                    && entry.asset.len() <= 256
                    && valid_lowercase_sha256(&entry.asset_sha256)
                    && !entry.tint.is_empty()
                    && entry.tint.len() <= 64
                    && entry.size_points.width.is_finite()
                    && entry.size_points.height.is_finite()
                    && (1.0..=1_024.0).contains(&entry.size_points.width)
                    && (1.0..=1_024.0).contains(&entry.size_points.height)
                    && !entry.fallback_state.is_empty()
                    && entry.fallback_state.len() <= 64
            })
            && registry
                .rendered_icon_ids
                .iter()
                .all(|identifier| ids.contains(identifier.as_str()))
    }

    fn copy_verified_capture(
        capture: &ValidatedFixtureCapture,
        destination: &Path,
    ) -> anyhow::Result<()> {
        let mut input = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&capture.source_path)
            .context("Presentation fixture capture is unavailable.")?;
        let mut output = create_new_private_export_file(destination)?;
        let mut digest = Sha256::new();
        let mut total = 0_u64;
        let mut buffer = [0_u8; 64 * 1_024];
        loop {
            let read = input.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            total = total.saturating_add(u64::try_from(read)?);
            if total > MAX_CAPTURE_BYTES {
                anyhow::bail!("Presentation fixture capture is invalid.");
            }
            digest.update(&buffer[..read]);
            output.write_all(&buffer[..read])?;
        }
        output.sync_all()?;
        if total == 0 || hex_encode(&digest.finalize()) != capture.sha256 {
            anyhow::bail!("Presentation fixture capture integrity check failed.");
        }
        validate_owner_only_export_file(destination, MAX_CAPTURE_BYTES)
    }

    fn create_new_private_export_file(path: &Path) -> anyhow::Result<File> {
        let parent = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Retained fixture artifact path is invalid."))?;
        validate_owner_only_export_directory(parent)?;
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .context("Unable to create a retained fixture artifact.")
    }

    fn write_new_private_export(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
        let mut file = create_new_private_export_file(path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        validate_owner_only_export_file(path, MAX_FIXTURE_RESPONSE_BYTES as u64)
    }

    fn sha256_private_export_file(path: &Path, maximum_bytes: u64) -> anyhow::Result<String> {
        validate_owner_only_export_file(path, maximum_bytes)?;
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .context("Fixture artifact is unavailable.")?;
        let mut digest = Sha256::new();
        let mut total = 0_u64;
        let mut buffer = [0_u8; 64 * 1_024];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            total = total.saturating_add(u64::try_from(read)?);
            if total > maximum_bytes {
                anyhow::bail!("Fixture artifact is invalid.");
            }
            digest.update(&buffer[..read]);
        }
        if total == 0 {
            anyhow::bail!("Fixture artifact is invalid.");
        }
        Ok(hex_encode(&digest.finalize()))
    }

    fn absolute_path_string(path: &Path) -> anyhow::Result<String> {
        let canonical = path
            .canonicalize()
            .context("Fixture artifact is unavailable.")?;
        if !canonical.is_absolute() {
            anyhow::bail!("Fixture artifact path is invalid.");
        }
        Ok(canonical.to_string_lossy().into_owned())
    }

    fn absolute_direct_child_path_string(root: &Path, path: &Path) -> anyhow::Result<String> {
        if !root.is_absolute() || path.parent() != Some(root) || !path.is_absolute() {
            anyhow::bail!("Fixture artifact path is invalid.");
        }
        Ok(path.to_string_lossy().into_owned())
    }

    fn wait_for_child(child: &mut Child) -> anyhow::Result<()> {
        let deadline = Instant::now() + HOST_EXIT_TIMEOUT;
        while Instant::now() < deadline {
            if let Some(status) = child.try_wait()? {
                if status.success() {
                    return Ok(());
                }
                anyhow::bail!("Presentation fixture host exited without completing the fixture.");
            }
            thread::sleep(Duration::from_millis(25));
        }
        anyhow::bail!("Presentation fixture host did not terminate in time.");
    }

    fn stop_child(child: &mut Child) {
        let _ = child.kill();
        let _ = child.wait();
    }

    fn random_bytes() -> [u8; 32] {
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        let mut bytes = [0_u8; 32];
        bytes[..16].copy_from_slice(first.as_bytes());
        bytes[16..].copy_from_slice(second.as_bytes());
        bytes
    }

    fn valid_lowercase_sha256(value: &str) -> bool {
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (byte.is_ascii_lowercase() && byte <= b'f'))
    }

    fn hex_encode(bytes: impl AsRef<[u8]>) -> String {
        bytes
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    #[cfg(test)]
    mod tests {
        use std::io::Read;

        use codex_whisply::DiagnosticFixtureReplayControl;
        use zip::ZipArchive;

        use super::super::FixtureAppearanceArg;
        use super::super::FixtureBackingScaleArg;
        use super::super::FixtureCaptureArg;
        use super::super::FixtureContrastArg;
        use super::super::FixtureFontScaleArg;
        use super::super::FixtureLayoutDirectionArg;
        use super::super::FixtureMotionArg;
        use super::super::FixtureThemeArg;
        use super::*;

        fn request(root: &OwnerOnlyTemporaryRoot) -> ForceUiRequest {
            ForceUiRequest {
                scenario: FixtureScenario {
                    fixture_id: "fixture-0".to_string(),
                    surface_id: "chat.overlay".to_string(),
                    state_id: "empty".to_string(),
                    seed: 0,
                    lane: DiagnosticLane::PresentationFixture,
                    fixture_root: root.path().to_path_buf(),
                    simulated: true,
                    billable: false,
                    authority: "none".to_string(),
                },
                width: 430,
                height: 760,
                appearance: super::super::FixtureAppearance::Auto,
                theme: FixtureTheme::System,
                presentation: FixturePresentation {
                    locale: "en_US".to_string(),
                    layout_direction: FixtureLayoutDirection::LeftToRight,
                    font_scale: FixtureFontScale::Standard,
                    reduce_motion: true,
                    increased_contrast: false,
                },
                display: FixtureDisplay::default(),
                capture: FixtureCapturePoint::Initial,
                watermark: PRESENTATION_FIXTURE_MARKER.to_string(),
                fixture_thread: None,
                fixture_replay: None,
            }
        }

        fn ui_metadata(request: &ForceUiRequest) -> FixtureUiMetadata {
            FixtureUiMetadata {
                schema_version: FIXTURE_UI_METADATA_SCHEMA_VERSION,
                requested: FixtureRequestedUiMetadata {
                    fixture_id: request.scenario.fixture_id.clone(),
                    surface_id: request.scenario.surface_id.clone(),
                    state_id: request.scenario.state_id.clone(),
                    seed: request.scenario.seed,
                    content_width_points: request.width,
                    content_height_points: request.height,
                    appearance: fixture_appearance_name(request.appearance).to_string(),
                    theme: fixture_theme_name(request.theme).to_string(),
                    locale: request.presentation.locale.clone(),
                    layout_direction: fixture_layout_direction_name(
                        request.presentation.layout_direction,
                    )
                    .to_string(),
                    font_scale: fixture_font_scale_name(request.presentation.font_scale)
                        .to_string(),
                    reduce_motion: request.presentation.reduce_motion,
                    increased_contrast: request.presentation.increased_contrast,
                    display_identifier: request.display.display_identifier,
                    backing_scale_milli: request.display.backing_scale_milli,
                    capture_kind: "initial".to_string(),
                    capture_event_id: None,
                },
                effective: FixtureEffectiveUiMetadata {
                    fixture_id: request.scenario.fixture_id.clone(),
                    surface_id: request.scenario.surface_id.clone(),
                    state_id: request.scenario.state_id.clone(),
                    content_frame_points: FixtureContentFrame {
                        x: 0.0,
                        y: 0.0,
                        width: f64::from(request.width),
                        height: f64::from(request.height),
                    },
                    capture_pixel_width: u32::from(request.width),
                    capture_pixel_height: u32::from(request.height),
                    appearance: "light".to_string(),
                    theme: fixture_theme_name(request.theme).to_string(),
                    locale: request.presentation.locale.clone(),
                    layout_direction: fixture_layout_direction_name(
                        request.presentation.layout_direction,
                    )
                    .to_string(),
                    font_scale: fixture_font_scale_name(request.presentation.font_scale)
                        .to_string(),
                    reduce_motion: request.presentation.reduce_motion,
                    increased_contrast: request.presentation.increased_contrast,
                    applied_presentation: FixtureAppliedPresentationMetadata {
                        layout_direction: fixture_layout_direction_name(
                            request.presentation.layout_direction,
                        )
                        .to_string(),
                        font_scale: fixture_font_scale_name(request.presentation.font_scale)
                            .to_string(),
                        dynamic_type_size: fixture_dynamic_type_size_name(
                            request.presentation.font_scale,
                        )
                        .to_string(),
                        reduce_motion: request.presentation.reduce_motion,
                        increased_contrast: request.presentation.increased_contrast,
                        contrast_gain: fixture_contrast_gain(
                            request.presentation.increased_contrast,
                        ),
                    },
                    display_available: false,
                    display_identifier: None,
                    backing_scale_factor: 1.0,
                    window_is_key: false,
                    window_is_main: false,
                    application_is_active: false,
                    accessibility_identifier: FIXTURE_HOST_ACCESSIBILITY_IDENTIFIER.to_string(),
                    accessibility_label: PRESENTATION_FIXTURE_MARKER.to_string(),
                    window_metadata: FixtureWindowMetadata {
                        level: FixtureWindowLevel {
                            semantic: "normal".to_string(),
                            raw_value: 0,
                        },
                        frame_points: FixtureWindowFrame {
                            x: 0.0,
                            y: 0.0,
                            width: f64::from(request.width),
                            height: f64::from(request.height) + 28.0,
                        },
                        display_available: false,
                        display_identifier: None,
                        display_frame_points: None,
                        display_visible_frame_points: None,
                        collection_behavior: FixtureWindowCollectionBehavior {
                            raw_value: 0,
                            can_join_all_spaces: false,
                            move_to_active_space: false,
                            stationary: false,
                            full_screen_auxiliary: false,
                            ignores_cycle: false,
                        },
                        is_on_active_space: false,
                        is_visible: false,
                    },
                    rendered_icon_registry: expected_fixture_rendered_icon_registry(request)
                        .expect("test fixture icon registry"),
                },
            }
        }

        fn response(request: &ForceUiRequest, capture_sha256: String) -> FixtureHostResponse {
            FixtureHostResponse {
                schema_version: DIAGNOSTIC_PROTOCOL_SCHEMA_VERSION,
                fixture_id: request.scenario.fixture_id.clone(),
                terminal_status: DiagnosticTerminalStatus::Succeeded,
                presentation_fixture: true,
                authority: "none".to_string(),
                network: "denied".to_string(),
                capture_method: SELF_CAPTURE_METHOD.to_string(),
                computer_use: SELF_CAPTURE_NOT_USED.to_string(),
                screen_recording: SELF_CAPTURE_NOT_USED.to_string(),
                capture_path: "fixture-capture.png".to_string(),
                capture_sha256,
                ui_metadata: ui_metadata(request),
            }
        }

        #[test]
        fn host_response_cannot_claim_fixture_success_with_authority() {
            let root = OwnerOnlyTemporaryRoot::new("whisply-fixture").expect("secure root");
            let request = request(&root);
            let response = FixtureHostResponse {
                authority: "account".to_string(),
                ..response(&request, "0".repeat(64))
            };
            assert!(validate_response(&response, &request, &root).is_err());
        }

        #[test]
        fn host_response_requires_the_fixed_capture_name() {
            let root = OwnerOnlyTemporaryRoot::new("whisply-fixture").expect("secure root");
            let request = request(&root);
            let response = FixtureHostResponse {
                capture_path: "another.png".to_string(),
                ..response(&request, "0".repeat(64))
            };
            assert!(validate_response(&response, &request, &root).is_err());
        }

        #[test]
        fn host_response_rejects_computer_use_or_screen_recording_capture_claims() {
            let root = OwnerOnlyTemporaryRoot::new("whisply-fixture").expect("secure root");
            let request = request(&root);

            for (capture_method, computer_use, screen_recording) in [
                (
                    "screen-recording",
                    SELF_CAPTURE_NOT_USED,
                    SELF_CAPTURE_NOT_USED,
                ),
                (SELF_CAPTURE_METHOD, "used", SELF_CAPTURE_NOT_USED),
                (SELF_CAPTURE_METHOD, SELF_CAPTURE_NOT_USED, "used"),
            ] {
                let response = FixtureHostResponse {
                    capture_method: capture_method.to_string(),
                    computer_use: computer_use.to_string(),
                    screen_recording: screen_recording.to_string(),
                    ..response(&request, "0".repeat(64))
                };
                assert!(validate_response(&response, &request, &root).is_err());
            }
        }

        #[test]
        fn host_response_rejects_ui_metadata_that_does_not_match_the_request() {
            let root = OwnerOnlyTemporaryRoot::new("whisply-fixture").expect("secure root");
            let base_request = request(&root);
            let mut host_response = response(&base_request, "0".repeat(64));
            host_response.ui_metadata.requested.content_width_points = base_request.width + 1;
            assert!(validate_response(&host_response, &base_request, &root).is_err());

            let mut host_response = response(&base_request, "0".repeat(64));
            host_response.ui_metadata.effective.accessibility_label = "ordinary app".to_string();
            assert!(validate_response(&host_response, &base_request, &root).is_err());

            let mut host_response = response(&base_request, "0".repeat(64));
            host_response.ui_metadata.effective.display_available = true;
            host_response.ui_metadata.effective.display_identifier = None;
            assert!(validate_response(&host_response, &base_request, &root).is_err());

            let mut light_request = request(&root);
            light_request.appearance = FixtureAppearance::Light;
            let mut host_response = response(&light_request, "0".repeat(64));
            host_response.ui_metadata.effective.appearance = "dark".to_string();
            assert!(validate_response(&host_response, &light_request, &root).is_err());

            let mut theme_request = request(&root);
            theme_request.theme = FixtureTheme::Midnight;
            let mut host_response = response(&theme_request, "0".repeat(64));
            host_response.ui_metadata.effective.theme = "system".to_string();
            assert!(validate_response(&host_response, &theme_request, &root).is_err());
        }

        #[test]
        fn ui_metadata_keeps_host_accessibility_distinct_and_requires_fixture_render_policy() {
            let root = OwnerOnlyTemporaryRoot::new("whisply-fixture").expect("secure root");
            let request = request(&root);
            let mut metadata = ui_metadata(&request);
            metadata.effective.reduce_motion = !request.presentation.reduce_motion;
            metadata.effective.increased_contrast = !request.presentation.increased_contrast;

            assert!(ui_metadata_matches_request(&metadata, &request));

            metadata.effective.applied_presentation.reduce_motion =
                !request.presentation.reduce_motion;
            assert!(!ui_metadata_matches_request(&metadata, &request));

            let mut metadata = ui_metadata(&request);
            metadata.effective.applied_presentation.increased_contrast =
                !request.presentation.increased_contrast;
            assert!(!ui_metadata_matches_request(&metadata, &request));

            let mut metadata = ui_metadata(&request);
            metadata.effective.applied_presentation.contrast_gain = 1.1;
            assert!(!ui_metadata_matches_request(&metadata, &request));

            let mut metadata = ui_metadata(&request);
            metadata.requested.layout_direction = "right_to_left".to_string();
            assert!(!ui_metadata_matches_request(&metadata, &request));

            let mut metadata = ui_metadata(&request);
            metadata.effective.font_scale = "large".to_string();
            assert!(!ui_metadata_matches_request(&metadata, &request));

            let mut metadata = ui_metadata(&request);
            metadata.effective.applied_presentation.layout_direction = "right_to_left".to_string();
            assert!(!ui_metadata_matches_request(&metadata, &request));

            let mut metadata = ui_metadata(&request);
            metadata.effective.applied_presentation.dynamic_type_size = "xxx_large".to_string();
            assert!(!ui_metadata_matches_request(&metadata, &request));
        }

        #[test]
        fn ui_metadata_rejects_window_level_frame_and_collection_behavior_drift() {
            let root = OwnerOnlyTemporaryRoot::new("whisply-fixture").expect("secure root");
            let request = request(&root);

            let mut metadata = ui_metadata(&request);
            metadata.effective.window_metadata.level.semantic = "floating".to_string();
            assert!(!ui_metadata_matches_request(&metadata, &request));

            let mut metadata = ui_metadata(&request);
            metadata.effective.window_metadata.frame_points.width = 0.0;
            assert!(!ui_metadata_matches_request(&metadata, &request));

            let mut metadata = ui_metadata(&request);
            metadata
                .effective
                .window_metadata
                .collection_behavior
                .can_join_all_spaces = true;
            assert!(!ui_metadata_matches_request(&metadata, &request));
        }

        #[test]
        fn ui_metadata_rejects_rendered_icon_registry_drift() {
            let root = OwnerOnlyTemporaryRoot::new("whisply-fixture").expect("secure root");
            let mut preferences = request(&root);
            preferences.scenario.surface_id = "preferences".to_string();
            preferences.scenario.state_id = "general".to_string();
            preferences.width = 880;
            preferences.height = 600;

            let metadata = ui_metadata(&preferences);
            assert!(ui_metadata_matches_request(&metadata, &preferences));
            assert_eq!(metadata.effective.rendered_icon_registry.registry.len(), 13);
            assert_eq!(
                metadata
                    .effective
                    .rendered_icon_registry
                    .rendered_icon_ids
                    .len(),
                13
            );

            let mut metadata = ui_metadata(&preferences);
            metadata.effective.rendered_icon_registry.registry[0].asset =
                "sf-symbol:arrow.left".to_string();
            assert!(!ui_metadata_matches_request(&metadata, &preferences));

            let mut metadata = ui_metadata(&preferences);
            metadata.effective.rendered_icon_registry.registry[0].asset_sha256 = "0".repeat(64);
            assert!(!ui_metadata_matches_request(&metadata, &preferences));

            let mut metadata = ui_metadata(&preferences);
            metadata.effective.rendered_icon_registry.registry[0].tint = "warning".to_string();
            assert!(!ui_metadata_matches_request(&metadata, &preferences));

            let mut metadata = ui_metadata(&preferences);
            metadata.effective.rendered_icon_registry.registry[0]
                .size_points
                .width = 17.0;
            assert!(!ui_metadata_matches_request(&metadata, &preferences));

            let mut metadata = ui_metadata(&preferences);
            metadata
                .effective
                .rendered_icon_registry
                .rendered_icon_ids
                .pop();
            assert!(!ui_metadata_matches_request(&metadata, &preferences));

            let mut history = request(&root);
            history.scenario.surface_id = "history".to_string();
            history.scenario.state_id = "empty".to_string();
            history.width = 480;
            history.height = 600;
            let metadata = ui_metadata(&history);
            assert!(ui_metadata_matches_request(&metadata, &history));
            assert_eq!(metadata.effective.rendered_icon_registry.registry.len(), 2);
            assert_eq!(
                metadata
                    .effective
                    .rendered_icon_registry
                    .rendered_icon_ids
                    .len(),
                2
            );

            let mut metadata = ui_metadata(&history);
            metadata.effective.rendered_icon_registry.registry[1].tint = "secondary".to_string();
            assert!(!ui_metadata_matches_request(&metadata, &history));

            let mut needs_input = request(&root);
            needs_input.scenario.state_id = "needs-input".to_string();
            needs_input.width = 430;
            needs_input.height = 100;
            let metadata = ui_metadata(&needs_input);
            assert!(ui_metadata_matches_request(&metadata, &needs_input));
            assert_eq!(metadata.effective.rendered_icon_registry.registry.len(), 1);
            assert_eq!(
                metadata
                    .effective
                    .rendered_icon_registry
                    .rendered_icon_ids
                    .len(),
                1
            );

            let mut metadata = ui_metadata(&needs_input);
            metadata.effective.rendered_icon_registry.registry[0]
                .size_points
                .width = 11.0;
            assert!(!ui_metadata_matches_request(&metadata, &needs_input));

            let mut subscription = request(&root);
            subscription.scenario.surface_id = "subscription.gate".to_string();
            subscription.scenario.state_id = "starter".to_string();
            subscription.width = 880;
            subscription.height = 760;
            let metadata = ui_metadata(&subscription);
            assert!(ui_metadata_matches_request(&metadata, &subscription));
            assert_eq!(metadata.effective.rendered_icon_registry.registry.len(), 5);
            assert_eq!(
                metadata
                    .effective
                    .rendered_icon_registry
                    .rendered_icon_ids
                    .len(),
                38
            );

            let mut metadata = ui_metadata(&subscription);
            metadata.effective.rendered_icon_registry.registry[0].fallback_state =
                "none".to_string();
            assert!(!ui_metadata_matches_request(&metadata, &subscription));
        }

        #[test]
        fn retained_fixture_export_returns_absolute_private_non_overwriting_paths()
        -> anyhow::Result<()> {
            let root = OwnerOnlyTemporaryRoot::new("whisply-fixture")?;
            let request = request(&root);
            let source = root.child("fixture-capture.png")?;
            let capture_bytes = b"verified self-captured fixture bytes";
            write_new_private(&source, capture_bytes)?;
            let capture_sha256 = hex_encode(Sha256::digest(capture_bytes));
            let response = response(&request, capture_sha256);
            let validated = validate_response(&response, &request, &root)?;
            let output = tempfile::tempdir()?;
            fs::set_permissions(output.path(), fs::Permissions::from_mode(0o700))?;
            let output_root = resolve_owner_selected_export_directory(output.path())?;

            let retained = retain_verified_fixture_artifacts(&validated, &response, &output_root)?;
            assert_eq!(retained.artifact_paths.len(), 3);
            assert!(Path::new(&retained.screenshot_path).is_absolute());
            assert_eq!(fs::read(&retained.screenshot_path)?, capture_bytes);
            for path in &retained.artifact_paths {
                validate_owner_only_export_file(Path::new(path), MAX_CAPTURE_BYTES)?;
            }

            let artifact_manifest = Path::new(&retained.artifact_paths[1]);
            let manifest: serde_json::Value =
                serde_json::from_slice(&fs::read(artifact_manifest)?)?;
            assert_eq!(
                manifest["screenshotPath"].as_str(),
                Some(retained.screenshot_path.as_str())
            );
            assert_eq!(
                manifest["artifactPaths"],
                serde_json::json!(&retained.artifact_paths)
            );
            assert_eq!(
                manifest["screenshotSha256"].as_str(),
                Some(response.capture_sha256.as_str())
            );
            assert_eq!(
                manifest["computerUse"].as_str(),
                Some(SELF_CAPTURE_NOT_USED)
            );
            assert_eq!(
                manifest["screenRecording"].as_str(),
                Some(SELF_CAPTURE_NOT_USED)
            );
            assert_eq!(
                manifest["uiMetadata"]["requested"]["fixtureId"].as_str(),
                Some(request.scenario.fixture_id.as_str())
            );
            assert_eq!(
                manifest["uiMetadata"]["requested"]["contentWidthPoints"].as_u64(),
                Some(u64::from(request.width))
            );
            assert_eq!(
                manifest["uiMetadata"]["effective"]["accessibilityIdentifier"].as_str(),
                Some(FIXTURE_HOST_ACCESSIBILITY_IDENTIFIER)
            );
            assert_eq!(
                manifest["uiMetadata"]["effective"]["stateId"].as_str(),
                Some(request.scenario.state_id.as_str())
            );
            assert_eq!(
                manifest["uiMetadata"]["effective"]["accessibilityLabel"].as_str(),
                Some(PRESENTATION_FIXTURE_MARKER)
            );
            assert_eq!(
                manifest["uiMetadata"]["effective"]["windowMetadata"]["level"]["semantic"].as_str(),
                Some("normal")
            );
            assert_eq!(
                manifest["uiMetadata"]["effective"]["windowMetadata"]["level"]["rawValue"].as_i64(),
                Some(0)
            );
            assert_eq!(
                manifest["uiMetadata"]["effective"]["windowMetadata"]["collectionBehavior"]
                    ["rawValue"]
                    .as_u64(),
                Some(0)
            );

            let hashes = fs::read_to_string(&retained.artifact_paths[2])?;
            assert!(hashes.contains(EXPORTED_CAPTURE_FILE));
            assert!(hashes.contains(EXPORTED_ARTIFACT_MANIFEST_FILE));
            let archive = artifact_manifest
                .parent()
                .expect("retained artifact archive");
            let verified = verify_retained_fixture_archive_contract(archive)?;
            assert_eq!(verified.fixture_id, request.scenario.fixture_id);
            assert_eq!(verified.screenshot_path, retained.screenshot_path);
            assert_eq!(verified.screenshot_sha256, response.capture_sha256);
            let unexpected = archive.join("unexpected.json");
            write_new_private_export(&unexpected, b"{}")?;
            assert!(verify_retained_fixture_archive_contract(archive).is_err());
            assert!(write_new_private_export(artifact_manifest, b"overwrite").is_err());
            Ok(())
        }

        fn write_retained_fixture_bundle_json(
            bundle: &Path,
            relative: &str,
            value: &serde_json::Value,
        ) -> anyhow::Result<()> {
            let mut bytes = serde_json::to_vec(value)?;
            bytes.push(b'\n');
            write_new_private_export(&bundle.join(relative), &bytes)
        }

        fn write_retained_fixture_bundle_hashes(
            bundle: &Path,
            paths: &[String],
        ) -> anyhow::Result<()> {
            let mut paths = paths.to_vec();
            paths.sort();
            let mut contents = String::new();
            for relative in paths {
                let digest = sha256_private_export_file(
                    &bundle.join(&relative),
                    MAX_LEGACY_FIXTURE_BUNDLE_BYTES,
                )?;
                contents.push_str(&format!("{digest}  {relative}\n"));
            }
            write_new_private_export(
                &bundle.join(EXPORTED_HASH_MANIFEST_FILE),
                contents.as_bytes(),
            )
        }

        fn copy_retained_fixture_bundle(source: &Path, destination: &Path) -> anyhow::Result<()> {
            fs::create_dir(destination)?;
            fs::set_permissions(destination, fs::Permissions::from_mode(0o700))?;
            for entry in fs::read_dir(source)? {
                let entry = entry?;
                let source_path = entry.path();
                let destination_path = destination.join(entry.file_name());
                if fs::symlink_metadata(&source_path)?.is_dir() {
                    fs::create_dir(&destination_path)?;
                    fs::set_permissions(&destination_path, fs::Permissions::from_mode(0o700))?;
                    for child in fs::read_dir(&source_path)? {
                        let child = child?;
                        let child_destination = destination_path.join(child.file_name());
                        fs::copy(child.path(), &child_destination)?;
                        fs::set_permissions(&child_destination, fs::Permissions::from_mode(0o600))?;
                    }
                } else {
                    fs::copy(&source_path, &destination_path)?;
                    fs::set_permissions(&destination_path, fs::Permissions::from_mode(0o600))?;
                }
            }
            Ok(())
        }

        #[test]
        fn retained_fixture_bundle_is_catalog_attested_private_and_non_executable()
        -> anyhow::Result<()> {
            let output = tempfile::tempdir()?;
            fs::set_permissions(output.path(), fs::Permissions::from_mode(0o700))?;
            let bundle = output.path().join("fixture-evidence");
            fs::create_dir(&bundle)?;
            fs::set_permissions(&bundle, fs::Permissions::from_mode(0o700))?;
            for directory in [
                "screenshots",
                "accessibility",
                "layout",
                "visible-copy",
                "icons",
            ] {
                let path = bundle.join(directory);
                fs::create_dir(&path)?;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
            }

            let contract = super::super::scenario_registry::catalog_fixture_evidence_contract(
                "browser-work-success",
            )?;
            let run_id = "00000000-0000-4000-8000-000000000001";
            let options = serde_json::json!({
                "seed": 7,
                "clock": "2026-08-15T00:00:00Z",
                "width": 430,
                "height": 760,
                "appearance": "light",
                "reduceMotion": true,
                "increasedContrast": false,
                "locale": "en_US",
            });
            let authority_contract = serde_json::to_value(&contract.authority)?;
            let fixture_contract = serde_json::to_value(&contract.fixture)?;
            let run = serde_json::json!({
                "schemaVersion": LEGACY_FIXTURE_BUNDLE_SCHEMA_VERSION,
                "protocol": LEGACY_FIXTURE_BUNDLE_PROTOCOL,
                "runnerVersion": contract.runner_version.clone(),
                "runID": run_id,
                "scenarioID": contract.scenario_id.clone(),
                "scenarioVersion": contract.scenario_version,
                "controlMode": "run",
                "stopAfterStep": serde_json::Value::Null,
                "source": {
                    "repository": "/private/fixture",
                    "commit": "0000000000000000000000000000000000000000",
                    "branch": "fixture",
                    "clean": true,
                    "dirtyPathCount": 0,
                    "diffSha256": "0000000000000000000000000000000000000000000000000000000000000000",
                },
                "registrySha256": contract.registry_sha256.clone(),
                "schemaSha256": contract.schema_sha256.clone(),
                "resolvedOptions": options,
                "authority": authority_contract,
                "fixture": fixture_contract,
                "requiredEvidence": contract.required_evidence.clone(),
                "assertions": contract.assertions.clone(),
                "execution": {
                    "adapter": "swift-xctest",
                    "networkSandbox": "deny-network",
                    "shellUsed": false,
                    "startedAt": "2026-08-15T00:00:00Z",
                    "finishedAt": "2026-08-15T00:00:01Z",
                    "exitCode": 0,
                    "timedOut": false,
                    "runnerLogRedactions": 0,
                },
            });
            let assertions = contract
                .assertions
                .iter()
                .map(|assertion| (assertion.clone(), serde_json::Value::Bool(true)))
                .collect::<serde_json::Map<_, _>>();
            let request = serde_json::json!({
                "protocol": LEGACY_FIXTURE_BUNDLE_PROTOCOL,
                "runnerVersion": contract.runner_version.clone(),
                "runID": run_id,
                "scenarioID": contract.scenario_id,
                "scenarioVersion": contract.scenario_version,
                "registrySha256": contract.registry_sha256.clone(),
                "schemaSha256": contract.schema_sha256.clone(),
                "controlMode": "run",
                "stopAfterStep": serde_json::Value::Null,
                "options": run["resolvedOptions"].clone(),
                "authority": run["authority"].clone(),
                "fixture": run["fixture"].clone(),
                "requiredEvidence": run["requiredEvidence"].clone(),
                "assertions": run["assertions"].clone(),
                "capabilitySha256": "0000000000000000000000000000000000000000000000000000000000000000",
            });
            let result = serde_json::json!({
                "schemaVersion": LEGACY_FIXTURE_BUNDLE_SCHEMA_VERSION,
                "status": "passed",
                "scenarioID": run["scenarioID"].clone(),
                "scenarioVersion": run["scenarioVersion"].clone(),
                "controlMode": "run",
                "stopAfterStep": serde_json::Value::Null,
                "assertions": assertions,
                "pendingAssertions": [],
                "allRequestedAssertionsEvaluated": true,
                "safeModelProgressOnly": true,
                "hiddenReasoningCaptured": false,
                "providerRoute": "none-isolated-fixture",
                "modelRoute": "none-isolated-fixture",
            });
            let authority = serde_json::json!({
                "schemaVersion": LEGACY_FIXTURE_BUNDLE_SCHEMA_VERSION,
                "host": run["authority"]["host"].clone(),
                "network": run["authority"]["network"].clone(),
                "networkSandboxEnvironment": true,
                "providerRequests": run["authority"]["providerRequests"].clone(),
                "commercialAuthority": run["authority"]["commercialAuthority"].clone(),
                "productionMutation": run["authority"]["productionMutation"].clone(),
                "fixtureOnly": true,
                "supabaseSessionMinted": false,
                "subscriptionMinted": false,
                "entitlementMinted": false,
                "fundingAuthorityMinted": false,
                "usageReservationCreated": false,
                "providerRequestCreated": false,
            });
            let animation = serde_json::json!({
                "schemaVersion": LEGACY_FIXTURE_BUNDLE_SCHEMA_VERSION,
                "reduceMotion": true,
                "increasedContrast": false,
                "frames": {
                    "fixture-frame": {
                        "animatedProgressActive": false,
                        "decorativeMotionAllowed": false,
                    },
                },
            });
            let event = serde_json::json!({
                "sequence": 1,
                "runID": run_id,
                "scenarioID": run["scenarioID"].clone(),
                "type": "fixture.started",
                "step": "fixture-start",
                "state": "succeeded",
                "title": "Captured isolated fixture evidence",
                "occurredAt": "2026-08-15T00:00:00Z",
            });
            let mut paths = Vec::new();
            for (relative, value) in [
                ("run-request.json", request),
                ("run.json", run),
                ("result.json", result),
                ("authority.json", authority),
                ("animation.json", animation),
                (
                    "accessibility/frame.json",
                    serde_json::json!({"schemaVersion": 1}),
                ),
                ("layout/frame.json", serde_json::json!({"schemaVersion": 1})),
                (
                    "visible-copy/frame.json",
                    serde_json::json!({"schemaVersion": 1}),
                ),
                ("icons/frame.json", serde_json::json!({"schemaVersion": 1})),
            ] {
                write_retained_fixture_bundle_json(&bundle, relative, &value)?;
                paths.push(relative.to_string());
            }
            write_new_private_export(&bundle.join("runner.log"), b"fixture runner complete\n")?;
            paths.push("runner.log".to_string());
            write_new_private_export(&bundle.join("screenshots/frame.png"), b"fixture png")?;
            paths.push("screenshots/frame.png".to_string());
            let event_line = format!("{}\n", serde_json::to_string(&event)?);
            write_new_private_export(&bundle.join("events.jsonl"), event_line.as_bytes())?;
            paths.push("events.jsonl".to_string());
            write_new_private_export(
                &bundle.join("structured-log.jsonl"),
                b"{\"event\":\"fixture.started\",\"sequence\":1,\"state\":\"succeeded\"}\n",
            )?;
            paths.push("structured-log.jsonl".to_string());
            write_retained_fixture_bundle_hashes(&bundle, &paths)?;

            let verified = verify_retained_fixture_bundle_contract(&bundle)?;
            assert_eq!(verified.scenario_id, "browser-work-success");
            assert_eq!(verified.scenario_version, contract.scenario_version);
            assert_eq!(verified.event_count, 1);
            assert_eq!(verified.file_count, paths.len() + 1);
            assert_eq!(verified.total_bytes > 0, true);
            assert!(valid_lowercase_sha256(&verified.hash_manifest_sha256));

            let export_directory = output.path().join("fixture-exports");
            fs::create_dir(&export_directory)?;
            fs::set_permissions(&export_directory, fs::Permissions::from_mode(0o700))?;
            let export_path = export_directory.join("fixture-evidence.zip");
            let exported = export_retained_fixture_bundle_contract(&bundle, &export_path)?;
            assert_eq!(exported.verified.scenario_id, "browser-work-success");
            assert_eq!(exported.verified.event_count, 1);
            assert!(valid_lowercase_sha256(&exported.sha256));
            assert_eq!(
                exported.output_path,
                export_path.canonicalize()?.to_string_lossy().into_owned()
            );
            validate_owner_only_export_file(
                &export_path,
                MAX_REDACTED_FIXTURE_BUNDLE_EXPORT_BYTES,
            )?;
            let export_entries = redacted_fixture_bundle_export_entries(&exported.verified)?;
            assert_redacted_fixture_bundle_export(&export_path, &export_entries)?;
            let mut archive = ZipArchive::new(File::open(&export_path)?)?;
            let mut archive_members = BTreeSet::new();
            for index in 0..archive.len() {
                archive_members.insert(archive.by_index(index)?.name().to_string());
            }
            let expected_members = [
                EXPORTED_HASH_MANIFEST_FILE,
                REDACTED_FIXTURE_BUNDLE_EXPORT_MANIFEST_FILE,
                REDACTED_FIXTURE_BUNDLE_EXPORT_SUMMARY_FILE,
            ]
            .into_iter()
            .map(str::to_string)
            .collect::<BTreeSet<_>>();
            assert_eq!(archive_members, expected_members);
            let mut summary = String::new();
            archive
                .by_name(REDACTED_FIXTURE_BUNDLE_EXPORT_SUMMARY_FILE)?
                .read_to_string(&mut summary)?;
            let summary: serde_json::Value = serde_json::from_str(&summary)?;
            assert_eq!(summary["redacted"], serde_json::Value::Bool(true));
            assert_eq!(
                summary["rawEvidenceExported"],
                serde_json::Value::Bool(false)
            );
            for forbidden in [
                "events",
                "structuredLog",
                "screenshots",
                "accessibility",
                "layout",
                "icons",
                "visibleCopy",
            ] {
                assert!(summary.get(forbidden).is_none());
            }
            assert!(export_retained_fixture_bundle_contract(&bundle, &export_path).is_err());

            let same = compare_retained_fixture_bundles_contract(&bundle, &bundle)?;
            assert!(same.equivalent);
            assert!(same.changed.is_empty());

            let candidate = output.path().join("fixture-evidence-candidate");
            copy_retained_fixture_bundle(&bundle, &candidate)?;
            let replay_run_id = "00000000-0000-4000-8000-000000000002";
            let mut replay_run: serde_json::Value =
                serde_json::from_slice(&fs::read(candidate.join("run.json"))?)?;
            replay_run["runID"] = serde_json::json!(replay_run_id);
            replay_run["controlMode"] = serde_json::json!("replay");
            replay_run["source"]["repository"] = serde_json::json!("/private/replay-fixture");
            replay_run["source"]["branch"] = serde_json::json!("replay");
            replay_run["source"]["clean"] = serde_json::json!(false);
            replay_run["source"]["dirtyPathCount"] = serde_json::json!(1);
            replay_run["source"]["diffSha256"] = serde_json::json!(
                "1111111111111111111111111111111111111111111111111111111111111111"
            );
            replay_run["execution"]["startedAt"] = serde_json::json!("2026-08-15T00:02:00Z");
            replay_run["execution"]["finishedAt"] = serde_json::json!("2026-08-15T00:02:01Z");
            fs::remove_file(candidate.join("run.json"))?;
            write_retained_fixture_bundle_json(&candidate, "run.json", &replay_run)?;

            let mut replay_request: serde_json::Value =
                serde_json::from_slice(&fs::read(candidate.join("run-request.json"))?)?;
            replay_request["runID"] = serde_json::json!(replay_run_id);
            replay_request["controlMode"] = serde_json::json!("replay");
            fs::remove_file(candidate.join("run-request.json"))?;
            write_retained_fixture_bundle_json(&candidate, "run-request.json", &replay_request)?;

            let mut replay_result: serde_json::Value =
                serde_json::from_slice(&fs::read(candidate.join("result.json"))?)?;
            replay_result["controlMode"] = serde_json::json!("replay");
            fs::remove_file(candidate.join("result.json"))?;
            write_retained_fixture_bundle_json(&candidate, "result.json", &replay_result)?;

            let mut replay_event: serde_json::Value =
                serde_json::from_slice(&fs::read(candidate.join("events.jsonl"))?)?;
            replay_event["runID"] = serde_json::json!(replay_run_id);
            fs::remove_file(candidate.join("events.jsonl"))?;
            write_new_private_export(
                &candidate.join("events.jsonl"),
                format!("{}\n", serde_json::to_string(&replay_event)?).as_bytes(),
            )?;
            fs::remove_file(candidate.join(EXPORTED_HASH_MANIFEST_FILE))?;
            write_retained_fixture_bundle_hashes(&candidate, &paths)?;
            let replay_equivalent = compare_retained_fixture_bundles_contract(&bundle, &candidate)?;
            assert!(replay_equivalent.equivalent);
            assert!(replay_equivalent.changed.is_empty());

            let changed_animation = serde_json::json!({
                "schemaVersion": LEGACY_FIXTURE_BUNDLE_SCHEMA_VERSION,
                "reduceMotion": true,
                "increasedContrast": false,
                "frames": {
                    "fixture-frame": {
                        "animatedProgressActive": true,
                        "decorativeMotionAllowed": false,
                    },
                },
            });
            fs::remove_file(candidate.join("animation.json"))?;
            write_retained_fixture_bundle_json(&candidate, "animation.json", &changed_animation)?;
            fs::remove_file(candidate.join(EXPORTED_HASH_MANIFEST_FILE))?;
            write_retained_fixture_bundle_hashes(&candidate, &paths)?;
            let changed = compare_retained_fixture_bundles_contract(&bundle, &candidate)?;
            assert!(!changed.equivalent);
            assert_eq!(
                changed.changed,
                vec![FixtureBundleComparisonChange::Animation]
            );

            write_new_private_export(&bundle.join("unexpected.json"), b"{}")?;
            assert!(verify_retained_fixture_bundle_contract(&bundle).is_err());
            Ok(())
        }

        #[test]
        fn retained_fixture_export_requires_a_private_owner_selected_directory()
        -> anyhow::Result<()> {
            let output = tempfile::tempdir()?;
            fs::set_permissions(output.path(), fs::Permissions::from_mode(0o755))?;
            assert!(resolve_owner_selected_export_directory(output.path()).is_err());
            assert!(resolve_owner_selected_export_directory(Path::new("/")).is_err());
            Ok(())
        }

        #[test]
        fn capture_event_requires_the_named_event_variant() {
            let root = OwnerOnlyTemporaryRoot::new("whisply-fixture").expect("secure root");
            let output = tempfile::tempdir().expect("private output directory");
            let args = ForceUiArgs {
                surface: "chat.overlay".to_string(),
                state: "empty".to_string(),
                seed: 0,
                width: 430,
                height: 760,
                appearance: FixtureAppearanceArg::Auto,
                theme: FixtureThemeArg::System,
                locale: "en_US".to_string(),
                layout_direction: FixtureLayoutDirectionArg::LeftToRight,
                font_scale: FixtureFontScaleArg::Standard,
                motion: FixtureMotionArg::Reduced,
                contrast: FixtureContrastArg::Standard,
                display: None,
                backing_scale: None,
                capture: FixtureCaptureArg::Initial,
                capture_event: Some("rendered".to_string()),
                output_dir: output.path().to_path_buf(),
            };

            assert!(build_request(args, &root).is_err());
        }

        #[test]
        fn direct_named_event_capture_is_closed_over_the_real_chat_overlay_sequence() {
            let root = OwnerOnlyTemporaryRoot::new("whisply-fixture").expect("secure root");
            let output = tempfile::tempdir().expect("private output directory");
            let args = ForceUiArgs {
                surface: "chat.overlay".to_string(),
                state: "streaming".to_string(),
                seed: 0,
                width: 430,
                height: 760,
                appearance: FixtureAppearanceArg::Auto,
                theme: FixtureThemeArg::System,
                locale: "en_US".to_string(),
                layout_direction: FixtureLayoutDirectionArg::LeftToRight,
                font_scale: FixtureFontScaleArg::Standard,
                motion: FixtureMotionArg::Reduced,
                contrast: FixtureContrastArg::Standard,
                display: None,
                backing_scale: None,
                capture: FixtureCaptureArg::NamedEvent,
                capture_event: Some("chat-overlay-streaming".to_string()),
                output_dir: output.path().to_path_buf(),
            };

            let request = build_request(args, &root).expect("reviewed named capture");
            assert_eq!(request.scenario.surface_id, "chat.overlay");
            assert_eq!(request.scenario.state_id, "streaming");
            assert_eq!(
                request.capture,
                FixtureCapturePoint::NamedEvent {
                    event_id: "chat-overlay-streaming".to_string(),
                }
            );

            let invalid_args = ForceUiArgs {
                surface: "chat.overlay".to_string(),
                state: "streaming".to_string(),
                seed: 0,
                width: 430,
                height: 760,
                appearance: FixtureAppearanceArg::Auto,
                theme: FixtureThemeArg::System,
                locale: "en_US".to_string(),
                layout_direction: FixtureLayoutDirectionArg::LeftToRight,
                font_scale: FixtureFontScaleArg::Standard,
                motion: FixtureMotionArg::Reduced,
                contrast: FixtureContrastArg::Standard,
                display: None,
                backing_scale: None,
                capture: FixtureCaptureArg::NamedEvent,
                capture_event: Some("caller-supplied-event".to_string()),
                output_dir: output.path().to_path_buf(),
            };
            assert!(build_request(invalid_args, &root).is_err());
        }

        #[test]
        fn force_ui_keeps_fixture_local_presentation_controls_in_the_typed_request() {
            let root = OwnerOnlyTemporaryRoot::new("whisply-fixture").expect("secure root");
            let output = tempfile::tempdir().expect("private output directory");
            let args = ForceUiArgs {
                surface: "chat.overlay".to_string(),
                state: "empty".to_string(),
                seed: 19,
                width: 430,
                height: 760,
                appearance: FixtureAppearanceArg::Dark,
                theme: FixtureThemeArg::Midnight,
                locale: "fr_CA".to_string(),
                layout_direction: FixtureLayoutDirectionArg::RightToLeft,
                font_scale: FixtureFontScaleArg::Large,
                motion: FixtureMotionArg::Full,
                contrast: FixtureContrastArg::Increased,
                display: Some(42),
                backing_scale: Some(FixtureBackingScaleArg(2_000)),
                capture: FixtureCaptureArg::Initial,
                capture_event: None,
                output_dir: output.path().to_path_buf(),
            };

            let request = build_request(args, &root).expect("valid fixture request");

            assert_eq!(request.presentation.locale, "fr_CA");
            assert_eq!(
                request.presentation.layout_direction,
                FixtureLayoutDirection::RightToLeft
            );
            assert_eq!(request.presentation.font_scale, FixtureFontScale::Large);
            assert!(!request.presentation.reduce_motion);
            assert!(request.presentation.increased_contrast);
            assert_eq!(request.appearance, FixtureAppearance::Dark);
            assert_eq!(request.theme, FixtureTheme::Midnight);
            assert_eq!(request.display.display_identifier, Some(42));
            assert_eq!(request.display.backing_scale_milli, Some(2_000));
        }

        #[test]
        fn backing_scale_parser_is_bounded_and_preserves_milli_precision() {
            assert_eq!(
                "2.0"
                    .parse::<FixtureBackingScaleArg>()
                    .expect("valid scale")
                    .0,
                2_000
            );
            assert_eq!(
                "1.125"
                    .parse::<FixtureBackingScaleArg>()
                    .expect("valid scale")
                    .0,
                1_125
            );
            assert_eq!(
                "1.234"
                    .parse::<FixtureBackingScaleArg>()
                    .expect("valid scale")
                    .0,
                1_234
            );
            assert!("0.49".parse::<FixtureBackingScaleArg>().is_err());
            assert!("8.1".parse::<FixtureBackingScaleArg>().is_err());
            assert!("1.1234".parse::<FixtureBackingScaleArg>().is_err());
        }

        #[test]
        fn ui_metadata_requires_the_selected_display_and_actual_backing_scale() {
            let root = OwnerOnlyTemporaryRoot::new("whisply-fixture").expect("secure root");
            let mut request = request(&root);
            request.display = FixtureDisplay {
                display_identifier: Some(42),
                backing_scale_milli: Some(2_000),
            };
            let mut metadata = ui_metadata(&request);

            // An explicit display cannot silently fall back to an arbitrary
            // current screen or backing scale.
            assert!(!ui_metadata_matches_request(&metadata, &request));

            let display_frame = FixtureWindowFrame {
                x: 0.0,
                y: 0.0,
                width: 1_920.0,
                height: 1_080.0,
            };
            metadata.effective.display_available = true;
            metadata.effective.display_identifier = Some(42);
            metadata.effective.backing_scale_factor = 2.0;
            metadata.effective.window_metadata.display_available = true;
            metadata.effective.window_metadata.display_identifier = Some(42);
            metadata.effective.window_metadata.display_frame_points = Some(display_frame.clone());
            metadata
                .effective
                .window_metadata
                .display_visible_frame_points = Some(display_frame);
            assert!(ui_metadata_matches_request(&metadata, &request));

            metadata.effective.backing_scale_factor = 1.0;
            assert!(!ui_metadata_matches_request(&metadata, &request));

            metadata.effective.backing_scale_factor = 2.0;
            metadata.effective.display_identifier = Some(43);
            metadata.effective.window_metadata.display_identifier = Some(43);
            assert!(!ui_metadata_matches_request(&metadata, &request));
        }

        #[test]
        fn catalog_request_carries_the_exact_non_authoritative_fixture_thread() {
            let root = OwnerOnlyTemporaryRoot::new("whisply-fixture").expect("secure root");
            let catalog =
                super::super::scenario_registry::catalog_fixture_execution("browser-work-success")
                    .expect("catalog fixture execution");
            let request =
                build_catalog_request(catalog, &root, Some(FixtureCapturePoint::Terminal))
                    .expect("catalog fixture request");
            let execution = request.fixture_thread.expect("fixture thread");

            assert_eq!(request.scenario.surface_id, "chat.activity");
            assert_eq!(request.scenario.state_id, "terminal");
            assert_eq!(request.capture, FixtureCapturePoint::Terminal);
            assert_eq!(request.theme, FixtureTheme::System);
            assert_eq!(request.scenario.fixture_id, execution.plan.fixture_id);
            assert!(execution.events.events.iter().all(|event| {
                event.simulated && !event.billable && event.authority.commercial_authority == "none"
            }));

            let initial_catalog =
                super::super::scenario_registry::catalog_fixture_execution("browser-work-success")
                    .expect("catalog fixture execution");
            let initial =
                build_catalog_request(initial_catalog, &root, Some(FixtureCapturePoint::Initial))
                    .expect("initial catalog fixture request");
            assert_eq!(initial.scenario.state_id, "initial");
            assert_eq!(initial.capture, FixtureCapturePoint::Initial);

            let named_catalog =
                super::super::scenario_registry::catalog_fixture_execution("browser-work-success")
                    .expect("named catalog fixture execution");
            let event_id = named_catalog.execution.events.events[1].event_id.clone();
            let named = build_catalog_request(
                named_catalog,
                &root,
                Some(FixtureCapturePoint::NamedEvent {
                    event_id: event_id.clone(),
                }),
            )
            .expect("catalog named-event fixture request");
            assert_eq!(named.scenario.state_id, "event");
            assert_eq!(named.capture, FixtureCapturePoint::NamedEvent { event_id });

            let unknown_catalog =
                super::super::scenario_registry::catalog_fixture_execution("browser-work-success")
                    .expect("unknown-event catalog fixture execution");
            assert!(
                build_catalog_request(
                    unknown_catalog,
                    &root,
                    Some(FixtureCapturePoint::NamedEvent {
                        event_id: "unregistered-fixture-event".to_string(),
                    }),
                )
                .is_err()
            );
        }

        #[test]
        fn catalog_replay_request_derives_the_exact_fixture_capture_from_the_reducer() {
            let root = OwnerOnlyTemporaryRoot::new("whisply-fixture").expect("secure root");
            let controls = [
                DiagnosticFixtureReplayControl::Pause,
                DiagnosticFixtureReplayControl::Step,
                DiagnosticFixtureReplayControl::Speed {
                    speed: codex_whisply::DiagnosticFixtureReplaySpeed::Double,
                },
            ];
            let catalog = super::super::scenario_registry::catalog_fixture_execution_with_replay(
                "browser-work-success",
                &controls,
            )
            .expect("catalog replay fixture execution");
            let expected_event_id = catalog.execution.events.events[1].event_id.clone();
            let request = build_catalog_request(catalog, &root, None)
                .expect("reducer-derived catalog fixture request");

            assert_eq!(request.scenario.state_id, "event");
            assert_eq!(
                request.capture,
                FixtureCapturePoint::NamedEvent {
                    event_id: expected_event_id,
                }
            );
            let replay = request
                .fixture_replay
                .as_ref()
                .expect("reducer-derived replay state");
            assert!(replay.paused);
            assert_eq!(
                replay.speed,
                codex_whisply::DiagnosticFixtureReplaySpeed::Double
            );
            assert_eq!(replay.checkpoint_index, 1);
            assert!(request.validate().is_ok());

            let catalog = super::super::scenario_registry::catalog_fixture_execution_with_replay(
                "browser-work-success",
                &controls,
            )
            .expect("catalog replay fixture execution");
            assert!(
                build_catalog_request(catalog, &root, Some(FixtureCapturePoint::Terminal),)
                    .is_err()
            );
        }

        #[test]
        fn every_catalog_fixture_request_fits_the_bounded_owner_only_frame() {
            let root = OwnerOnlyTemporaryRoot::new("whisply-fixture").expect("secure root");
            let scenarios = super::super::scenario_registry::run(
                super::super::scenario_registry::DiagnosticScenarioCommand {
                    action: super::super::scenario_registry::DiagnosticScenarioAction::List,
                },
            )
            .expect("embedded scenario catalog");
            let scenario_ids = scenarios["scenarios"]
                .as_array()
                .expect("catalog scenarios")
                .iter()
                .map(|scenario| {
                    scenario["id"]
                        .as_str()
                        .expect("catalog scenario id")
                        .to_string()
                })
                .collect::<Vec<_>>();

            for scenario_id in scenario_ids {
                let catalog =
                    super::super::scenario_registry::catalog_fixture_execution(&scenario_id)
                        .expect("catalog fixture execution");
                let request =
                    build_catalog_request(catalog, &root, Some(FixtureCapturePoint::Terminal))
                        .expect("bounded catalog fixture request");
                let bytes = serde_json::to_vec(&request).expect("fixture request encoding");
                assert!(!bytes.is_empty());
                assert!(bytes.len() <= MAX_FIXTURE_REQUEST_BYTES);
            }
        }
    }
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
        fixture_host: "requires a release-matched native app",
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

#[cfg(unix)]
fn write_new_owner_only_catalog_replay_bundle(
    requested: &Path,
    bytes: &[u8],
) -> anyhow::Result<PathBuf> {
    use std::os::unix::fs::MetadataExt;

    if bytes.is_empty() || bytes.len() > MAX_REPLAY_BUNDLE_BYTES {
        anyhow::bail!("Catalog replay bundle exceeds its bounded export size.");
    }
    let parent = requested
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Catalog replay export path has no parent directory."))?;
    let file_name = requested
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty() && *name != "." && *name != "..")
        .ok_or_else(|| anyhow::anyhow!("Catalog replay export path needs a normal file name."))?;
    if !file_name.ends_with(".json") {
        anyhow::bail!("Catalog replay export must use a .json file name.");
    }
    let parent_metadata =
        fs::symlink_metadata(parent).context("Catalog replay export directory is unavailable.")?;
    if parent_metadata.file_type().is_symlink() || !parent_metadata.is_dir() {
        anyhow::bail!("Catalog replay export directory must be a real directory.");
    }
    let canonical_parent = parent
        .canonicalize()
        .context("Catalog replay export directory is unavailable.")?;
    if !canonical_parent.is_absolute() || canonical_parent == Path::new("/") {
        anyhow::bail!("Catalog replay export directory is invalid.");
    }
    let metadata = fs::symlink_metadata(&canonical_parent)
        .context("Catalog replay export directory is unavailable.")?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        anyhow::bail!("Catalog replay export directory must be owner-only.");
    }
    let output = canonical_parent.join(file_name);
    match fs::symlink_metadata(&output) {
        Ok(_) => anyhow::bail!("Catalog replay export destination already exists."),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).context("Catalog replay export destination is unavailable.");
        }
    }
    write_new_private(&output, bytes)?;
    let metadata =
        fs::symlink_metadata(&output).context("Catalog replay export could not be validated.")?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
        || metadata.len() != bytes.len() as u64
    {
        anyhow::bail!("Catalog replay export file is invalid.");
    }
    Ok(output)
}

#[cfg(not(unix))]
fn write_new_owner_only_catalog_replay_bundle(
    requested: &Path,
    bytes: &[u8],
) -> anyhow::Result<PathBuf> {
    let _ = requested;
    let _ = bytes;
    anyhow::bail!("Catalog replay export requires an owner-only Unix directory.");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog_replay_bundle_value() -> Value {
        let inspection = scenario_registry::run(scenario_registry::DiagnosticScenarioCommand {
            action: scenario_registry::DiagnosticScenarioAction::Inspect(
                scenario_registry::DiagnosticScenarioLookupArgs {
                    scenario: "browser-work-success".to_string(),
                },
            ),
        })
        .expect("embedded scenario inspection");
        serde_json::json!({
            "schemaVersion": scenario_registry::CATALOG_REPLAY_BUNDLE_SCHEMA_VERSION,
            "protocol": scenario_registry::CATALOG_REPLAY_BUNDLE_PROTOCOL,
            "registrySchemaVersion": 1,
            "registryProtocol": "whisply.diagnostics.v1",
            "registryRunnerVersion": "1.0.0",
            "registrySha256": inspection["registrySha256"].clone(),
            "schemaSha256": inspection["schemaSha256"].clone(),
            "scenarioId": "browser-work-success",
            "scenarioVersion": inspection["scenario"]["version"].clone(),
            "controls": [
                {"kind": "pause"},
                {"kind": "step"},
                {"kind": "speed", "speed": "double"}
            ]
        })
    }

    #[test]
    fn standalone_catalog_replay_rebuilds_the_fixture_from_a_redacted_bundle() -> anyhow::Result<()>
    {
        let root = tempfile::tempdir()?;
        let bundle = root.path().join("catalog-replay.json");
        write_new_private(
            &bundle,
            &serde_json::to_vec(&catalog_replay_bundle_value())?,
        )?;

        let catalog = validate_catalog_replay_bundle(&bundle)?;
        assert_eq!(catalog.execution.plan.scenario_id, "browser-work-success");
        let replay = catalog.replay.expect("reducer-derived replay");
        assert!(replay.paused);
        assert_eq!(replay.checkpoint_index, 1);
        assert!(
            catalog
                .execution
                .selectors
                .tools
                .iter()
                .all(|tool| !tool.can_execute && !tool.can_resolve_real_authority)
        );

        let invalid_bundle = root.path().join("catalog-replay-invalid.json");
        let mut invalid = catalog_replay_bundle_value();
        invalid["visibleCopy"] = Value::String("caller-authored state".to_string());
        write_new_private(&invalid_bundle, &serde_json::to_vec(&invalid)?)?;
        assert!(validate_catalog_replay_bundle(&invalid_bundle).is_err());
        Ok(())
    }

    #[test]
    fn catalog_replay_bundle_export_is_private_consumer_valid_and_non_overwriting()
    -> anyhow::Result<()> {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        #[cfg(unix)]
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir()?;
        #[cfg(unix)]
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700))?;
        let output = root.path().join("catalog-replay.json");
        let export_args = |output: PathBuf| DiagnosticCatalogReplayExportArgs {
            scenario: "browser-work-success".to_string(),
            controls: vec![
                scenario_registry::DiagnosticScenarioReplayControlArg(
                    DiagnosticFixtureReplayControl::Pause,
                ),
                scenario_registry::DiagnosticScenarioReplayControlArg(
                    DiagnosticFixtureReplayControl::Step,
                ),
            ],
            output,
        };

        export_catalog_replay_bundle(export_args(output.clone()))?;
        let bytes = fs::read(&output)?;
        let value: Value = serde_json::from_slice(&bytes)?;
        assert_eq!(
            value["protocol"],
            scenario_registry::CATALOG_REPLAY_BUNDLE_PROTOCOL
        );
        assert_eq!(value["scenarioId"], "browser-work-success");
        assert!(value.get("visibleCopy").is_none());
        assert!(value.get("authority").is_none());
        let catalog =
            scenario_registry::catalog_fixture_execution_from_redacted_replay_bundle(&bytes)?;
        let replay = catalog.replay.expect("reducer-derived replay");
        assert!(replay.paused);
        assert_eq!(replay.checkpoint_index, 1);
        #[cfg(unix)]
        {
            let metadata = fs::symlink_metadata(&output)?;
            assert_eq!(metadata.mode() & 0o077, 0);
            assert_eq!(metadata.len(), bytes.len() as u64);
        }
        assert!(export_catalog_replay_bundle(export_args(output)).is_err());

        #[cfg(unix)]
        {
            fs::set_permissions(root.path(), fs::Permissions::from_mode(0o755))?;
            assert!(
                export_catalog_replay_bundle(export_args(
                    root.path().join("public-catalog-replay.json"),
                ))
                .is_err()
            );
        }
        Ok(())
    }

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
        assert!(encoded.contains("requires a release-matched native app"));
        Ok(())
    }
}

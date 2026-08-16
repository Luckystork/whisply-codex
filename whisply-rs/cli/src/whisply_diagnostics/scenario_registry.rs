//! Immutable, read-only projection of the deterministic scenario catalog.
//!
//! The catalog is compiled from the same canonical source consumed by the
//! compatibility controller. It accepts no caller-supplied path and cannot
//! launch a fixture, attach to the app, or acquire product authority.

use anyhow::Context;
use clap::Args;
use codex_whisply::DIAGNOSTIC_SCENARIO_REGISTRY_PROTOCOL;
use codex_whisply::DIAGNOSTIC_SCENARIO_REGISTRY_SCHEMA_VERSION;
use codex_whisply::DIAGNOSTIC_SCENARIO_RUNNER_VERSION;
use codex_whisply::DiagnosticCommercialBoundary;
use codex_whisply::DiagnosticFixtureEventEnvelope;
use codex_whisply::DiagnosticFixtureEventKind;
use codex_whisply::DiagnosticFixtureEventStream;
use codex_whisply::DiagnosticFixtureExecution;
use codex_whisply::DiagnosticFixtureReplayControl;
use codex_whisply::DiagnosticFixtureReplaySpeed;
use codex_whisply::DiagnosticFixtureReplayState;
use codex_whisply::DiagnosticFixtureSelectors;
use codex_whisply::DiagnosticFixtureThreadPlan;
use codex_whisply::DiagnosticFixtureThreadTimeline;
use codex_whisply::DiagnosticScenario;
use codex_whisply::DiagnosticScenarioAuthority;
use codex_whisply::DiagnosticScenarioFixture;
use codex_whisply::DiagnosticScenarioRegistry;
use codex_whisply::FixtureAppearance;
use codex_whisply::FixtureCapturePoint;
use codex_whisply::FixtureFontScale;
use codex_whisply::FixtureLayoutDirection;
use codex_whisply::FixturePresentation;
use codex_whisply::FixtureTheme;
use codex_whisply::MAX_DIAGNOSTIC_FIXTURE_EVENTS;
use codex_whisply::MAX_DIAGNOSTIC_FIXTURE_REPLAY_CONTROLS;
use codex_whisply::build_diagnostic_fixture_event_stream;
use codex_whisply::build_diagnostic_fixture_execution;
use codex_whisply::build_diagnostic_fixture_replay;
use codex_whisply::build_diagnostic_fixture_selectors;
use codex_whisply::build_diagnostic_fixture_thread_plan;
use codex_whisply::build_diagnostic_fixture_thread_timeline;
use codex_whisply::parse_diagnostic_scenario_registry;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use sha2::Digest;
use sha2::Sha256;
use std::path::PathBuf;
use std::str::FromStr;

const EMBEDDED_SCENARIO_REGISTRY: &str =
    include_str!("../../../../../../docs/contextual-action-layer/diagnostic-scenarios.json");
const EMBEDDED_SCENARIO_SCHEMA: &str =
    include_str!("../../../../../../docs/contextual-action-layer/diagnostic-scenarios.schema.json");

/// Schema revision for the only replay bundle accepted by the installed
/// fixture lane. The bundle carries controls, never caller-authored view or
/// authority state.
pub(crate) const CATALOG_REPLAY_BUNDLE_SCHEMA_VERSION: u16 = 1;
/// Stable wire identity for a redacted, catalog-bound fixture replay bundle.
pub(crate) const CATALOG_REPLAY_BUNDLE_PROTOCOL: &str = "whisply.diagnostics.catalog-replay.v1";

#[derive(Debug, Args)]
pub(crate) struct DiagnosticScenarioCommand {
    #[command(subcommand)]
    pub(crate) action: DiagnosticScenarioAction,
}

#[derive(Debug, clap::Subcommand)]
pub(crate) enum DiagnosticScenarioAction {
    /// List immutable scenario summaries; this never launches a fixture.
    List,
    /// Print one immutable scenario definition; this never launches a fixture.
    Describe(DiagnosticScenarioLookupArgs),
    /// Print one definition plus the fixed catalog/schema hashes.
    Inspect(DiagnosticScenarioLookupArgs),
    /// Derive a deterministic, non-executable fixture-thread plan.
    Plan(DiagnosticScenarioLookupArgs),
    /// Print every virtual checkpoint of a catalog-derived fixture-thread plan.
    Timeline(DiagnosticScenarioLookupArgs),
    /// Print redacted, fixture-only metadata events without executing a fixture.
    Events(DiagnosticScenarioEventsArgs),
    /// Print catalog-derived synthetic account/model/profile/no-op tool selectors.
    Selectors(DiagnosticScenarioLookupArgs),
    /// Reduce bounded replay controls through one immutable fixture timeline.
    Replay(DiagnosticScenarioReplayArgs),
    /// Execute one immutable scenario in the isolated native fixture host.
    Execute(DiagnosticScenarioExecuteArgs),
    /// Print the embedded registry JSON Schema and its hash.
    Schema,
    /// Revalidate the embedded catalog's fixed zero-authority contract.
    Validate,
}

#[derive(Debug, Args)]
pub(crate) struct DiagnosticScenarioLookupArgs {
    /// Registered deterministic scenario id.
    #[arg(value_name = "SCENARIO")]
    pub(crate) scenario: String,
}

/// Read-only selection over one immutable catalog event stream. Every option
/// derives its view from the registered scenario; it cannot inject, edit, or
/// execute an event.
#[derive(Debug, Args)]
pub(crate) struct DiagnosticScenarioEventsArgs {
    /// Registered deterministic scenario id.
    #[arg(value_name = "SCENARIO")]
    pub(crate) scenario: String,

    /// Print the most recent COUNT redacted catalog events in chronological order.
    #[arg(
        long,
        value_name = "COUNT",
        conflicts_with_all = ["show", "kind"]
    )]
    pub(crate) tail: Option<usize>,

    /// Print one exact redacted catalog event id.
    #[arg(
        long,
        value_name = "EVENT",
        conflicts_with_all = ["tail", "kind"]
    )]
    pub(crate) show: Option<String>,

    /// Print redacted catalog events of one fixed lifecycle kind.
    #[arg(long, value_enum, conflicts_with_all = ["tail", "show"])]
    pub(crate) kind: Option<DiagnosticScenarioEventKindArg>,
}

/// A read-only replay reduction over one registered deterministic scenario.
/// Repeated controls are applied in order and can only select projections that
/// already exist in the catalog-derived virtual timeline.
#[derive(Debug, Args)]
pub(crate) struct DiagnosticScenarioReplayArgs {
    /// Registered deterministic scenario id.
    #[arg(value_name = "SCENARIO")]
    pub(crate) scenario: String,

    /// Ordered fixture replay control: pause, resume, step, seek:N, or
    /// speed:half|normal|double|quadruple. This does not launch a fixture.
    #[arg(long = "control", value_name = "CONTROL")]
    pub(crate) controls: Vec<DiagnosticScenarioReplayControlArg>,
}

impl DiagnosticScenarioReplayArgs {
    pub(crate) fn parsed_controls(&self) -> anyhow::Result<Vec<DiagnosticFixtureReplayControl>> {
        parse_replay_controls(&self.controls)
    }
}

/// A closed CLI spelling for the reducer's closed control set. It deliberately
/// accepts no JSON, event ids, timestamps, paths, accounts, targets, or other
/// normal-application input.
#[derive(Clone, Debug)]
pub(crate) struct DiagnosticScenarioReplayControlArg(pub(crate) DiagnosticFixtureReplayControl);

impl FromStr for DiagnosticScenarioReplayControlArg {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.trim() != value {
            return Err("Replay control may not include surrounding whitespace.".to_string());
        }
        let control = match value {
            "pause" => DiagnosticFixtureReplayControl::Pause,
            "resume" => DiagnosticFixtureReplayControl::Resume,
            "step" => DiagnosticFixtureReplayControl::Step,
            "speed:half" => DiagnosticFixtureReplayControl::Speed {
                speed: DiagnosticFixtureReplaySpeed::Half,
            },
            "speed:normal" => DiagnosticFixtureReplayControl::Speed {
                speed: DiagnosticFixtureReplaySpeed::Normal,
            },
            "speed:double" => DiagnosticFixtureReplayControl::Speed {
                speed: DiagnosticFixtureReplaySpeed::Double,
            },
            "speed:quadruple" => DiagnosticFixtureReplayControl::Speed {
                speed: DiagnosticFixtureReplaySpeed::Quadruple,
            },
            _ => {
                if let Some(index) = value.strip_prefix("seek:") {
                    let checkpoint_index = index.parse::<u16>().map_err(|_| {
                        "Seek must use a bounded non-negative checkpoint index, for example seek:2."
                            .to_string()
                    })?;
                    DiagnosticFixtureReplayControl::Seek { checkpoint_index }
                } else {
                    return Err(
                        "Replay control must be pause, resume, step, seek:N, or speed:half|normal|double|quadruple."
                            .to_string(),
                    );
                }
            }
        };
        Ok(Self(control))
    }
}

fn parse_replay_controls(
    controls: &[DiagnosticScenarioReplayControlArg],
) -> anyhow::Result<Vec<DiagnosticFixtureReplayControl>> {
    let controls = controls
        .iter()
        .map(|control| control.0.clone())
        .collect::<Vec<_>>();
    validate_replay_controls(&controls)?;
    Ok(controls)
}

fn validate_replay_controls(controls: &[DiagnosticFixtureReplayControl]) -> anyhow::Result<()> {
    if controls.len() > MAX_DIAGNOSTIC_FIXTURE_REPLAY_CONTROLS {
        anyhow::bail!(
            "Replay accepts at most {MAX_DIAGNOSTIC_FIXTURE_REPLAY_CONTROLS} ordered controls."
        );
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub(crate) enum DiagnosticScenarioEventKindArg {
    Pending,
    Checkpoint,
    Completed,
}

impl DiagnosticScenarioEventKindArg {
    fn fixture_kind(self) -> DiagnosticFixtureEventKind {
        match self {
            Self::Pending => DiagnosticFixtureEventKind::Pending,
            Self::Checkpoint => DiagnosticFixtureEventKind::Checkpoint,
            Self::Completed => DiagnosticFixtureEventKind::Completed,
        }
    }
}

#[derive(Debug, Args)]
pub(crate) struct DiagnosticScenarioExecuteArgs {
    /// Registered deterministic scenario id.
    #[arg(value_name = "SCENARIO")]
    pub(crate) scenario: String,

    /// Catalog-owned virtual state to capture. A named event is accepted only
    /// when `--capture-event` names an event already present in the immutable
    /// catalog stream; callers cannot provide a timeline or authority state.
    #[arg(long, value_enum)]
    pub(crate) capture: Option<DiagnosticScenarioCaptureArg>,

    /// One redacted catalog event id, valid only with `--capture named-event`.
    #[arg(long, value_name = "EVENT")]
    pub(crate) capture_event: Option<String>,

    /// Ordered fixture replay control: pause, resume, step, seek:N, or
    /// speed:half|normal|double|quadruple. When present, the capture point is
    /// derived by the reducer; `--capture` and `--capture-event` are rejected.
    #[arg(long = "replay-control", value_name = "CONTROL")]
    pub(crate) replay_controls: Vec<DiagnosticScenarioReplayControlArg>,

    /// Existing owner-only directory that receives a new retained fixture archive.
    #[arg(long, value_name = "DIRECTORY", value_hint = clap::ValueHint::DirPath)]
    pub(crate) output_dir: PathBuf,
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub(crate) enum DiagnosticScenarioCaptureArg {
    Initial,
    NamedEvent,
    Terminal,
}

impl DiagnosticScenarioExecuteArgs {
    pub(crate) fn capture_point(&self) -> anyhow::Result<FixtureCapturePoint> {
        match (
            self.capture
                .unwrap_or(DiagnosticScenarioCaptureArg::Terminal),
            self.capture_event.as_deref(),
        ) {
            (DiagnosticScenarioCaptureArg::Initial, None) => Ok(FixtureCapturePoint::Initial),
            (DiagnosticScenarioCaptureArg::Terminal, None) => Ok(FixtureCapturePoint::Terminal),
            (DiagnosticScenarioCaptureArg::NamedEvent, Some(event_id)) => {
                Ok(FixtureCapturePoint::NamedEvent {
                    event_id: event_id.to_string(),
                })
            }
            (DiagnosticScenarioCaptureArg::NamedEvent, None) => {
                anyhow::bail!("Named-event capture requires --capture-event <EVENT>.")
            }
            (_, Some(_)) => {
                anyhow::bail!("--capture-event is valid only with --capture named-event.")
            }
        }
    }

    pub(crate) fn parsed_replay_controls(
        &self,
    ) -> anyhow::Result<Vec<DiagnosticFixtureReplayControl>> {
        if !self.replay_controls.is_empty()
            && (self.capture.is_some() || self.capture_event.is_some())
        {
            anyhow::bail!(
                "Replay controls derive the fixture capture; do not combine them with --capture or --capture-event."
            );
        }
        parse_replay_controls(&self.replay_controls)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ScenarioListResponse {
    ok: bool,
    protocol: String,
    scenarios: Vec<ScenarioListEntry>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ScenarioListEntry {
    id: String,
    version: u32,
    title: String,
    area: String,
    steps: Vec<String>,
    authority: DiagnosticScenarioAuthority,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ScenarioResponse {
    ok: bool,
    scenario: DiagnosticScenario,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ScenarioInspectResponse {
    ok: bool,
    scenario: DiagnosticScenario,
    registry_sha256: String,
    schema_sha256: String,
    commands: [&'static str; 11],
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ScenarioPlanResponse {
    ok: bool,
    plan: DiagnosticFixtureThreadPlan,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ScenarioTimelineResponse {
    ok: bool,
    timeline: DiagnosticFixtureThreadTimeline,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ScenarioEventsResponse {
    ok: bool,
    events: DiagnosticFixtureEventStream,
}

/// A bounded, read-only view over the complete immutable stream. The stream
/// summary tells callers what was selected without handing a partial stream to
/// any execution path.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ScenarioEventSelectionResponse {
    ok: bool,
    selection: &'static str,
    requested_tail_count: Option<usize>,
    requested_event_id: Option<String>,
    requested_kind: Option<DiagnosticFixtureEventKind>,
    stream: ScenarioEventStreamSummary,
    events: Vec<DiagnosticFixtureEventEnvelope>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ScenarioEventStreamSummary {
    schema_version: u16,
    protocol: String,
    fixture_id: String,
    scenario_id: String,
    total_event_count: usize,
}

impl From<&DiagnosticFixtureEventStream> for ScenarioEventStreamSummary {
    fn from(stream: &DiagnosticFixtureEventStream) -> Self {
        Self {
            schema_version: stream.schema_version,
            protocol: stream.protocol.clone(),
            fixture_id: stream.fixture_id.clone(),
            scenario_id: stream.scenario_id.clone(),
            total_event_count: stream.events.len(),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ScenarioSelectorsResponse {
    ok: bool,
    selectors: DiagnosticFixtureSelectors,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ScenarioReplayResponse {
    ok: bool,
    replay: DiagnosticFixtureReplayState,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ScenarioSchemaResponse {
    ok: bool,
    schema: Value,
    sha256: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ScenarioValidationResponse {
    ok: bool,
    protocol: String,
    runner_version: String,
    scenario_count: usize,
    area_count: usize,
    sha256: String,
    commercial_boundary: DiagnosticCommercialBoundary,
}

/// Exact presentation controls coupled to a catalog-attested fixture handoff.
/// The public scenario command cannot override any of these fields.
pub(crate) struct CatalogFixtureExecution {
    pub(crate) execution: DiagnosticFixtureExecution,
    pub(crate) replay: Option<DiagnosticFixtureReplayState>,
    pub(crate) width: u16,
    pub(crate) height: u16,
    pub(crate) appearance: FixtureAppearance,
    pub(crate) theme: FixtureTheme,
    pub(crate) presentation: FixturePresentation,
}

/// Immutable catalog facts needed to attest a completed legacy XCTest
/// evidence directory. This is deliberately data only: it cannot name a
/// bundle path, launch a fixture, replay a fixture, or attach to the normal
/// app. The verifier uses it to reject a locally authored registry or a
/// cross-scenario evidence splice before it reads any rendered evidence.
pub(crate) struct CatalogFixtureEvidenceContract {
    pub(crate) runner_version: String,
    pub(crate) registry_sha256: String,
    pub(crate) schema_sha256: String,
    pub(crate) scenario_id: String,
    pub(crate) scenario_version: u32,
    pub(crate) authority: DiagnosticScenarioAuthority,
    pub(crate) fixture: DiagnosticScenarioFixture,
    pub(crate) required_evidence: Vec<String>,
    pub(crate) assertions: Vec<String>,
    pub(crate) steps: Vec<String>,
}

/// The complete accepted shape of an on-disk production replay bundle. It is
/// intentionally just a hash/version-pinned catalog selector plus the closed
/// reducer controls; all renderable state is rebuilt from the installed
/// catalog after validation.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RedactedCatalogReplayBundle {
    schema_version: u16,
    protocol: String,
    registry_schema_version: u16,
    registry_protocol: String,
    registry_runner_version: String,
    registry_sha256: String,
    schema_sha256: String,
    scenario_id: String,
    scenario_version: u32,
    controls: Vec<Value>,
}

/// Parses the sole accepted mutable part of a production replay bundle. Serde
/// intentionally ignores unknown fields on internally tagged enum variants,
/// so validate each object shape before asking it to decode the closed control
/// enum. This prevents an otherwise inert extra field from silently becoming
/// accepted production bundle surface area.
fn strict_redacted_replay_controls(
    controls: &[Value],
) -> anyhow::Result<Vec<DiagnosticFixtureReplayControl>> {
    let mut parsed = Vec::with_capacity(controls.len());
    for control in controls {
        let object = control
            .as_object()
            .context("Replay control must be a JSON object.")?;
        let kind = object
            .get("kind")
            .and_then(Value::as_str)
            .context("Replay control must name a string kind.")?;
        let allowed_fields = match kind {
            "pause" | "resume" | "step" => &["kind"][..],
            "seek" => &["kind", "checkpoint_index"][..],
            "speed" => &["kind", "speed"][..],
            _ => anyhow::bail!("Replay control kind is not part of the closed fixture set."),
        };
        if object.len() != allowed_fields.len()
            || object
                .keys()
                .any(|field| !allowed_fields.contains(&field.as_str()))
        {
            anyhow::bail!("Replay control contains fields outside its closed fixture shape.");
        }
        parsed.push(
            serde_json::from_value(control.clone())
                .context("Replay control does not match the fixed fixture schema.")?,
        );
    }
    validate_replay_controls(&parsed)?;
    Ok(parsed)
}

pub(crate) fn run(command: DiagnosticScenarioCommand) -> anyhow::Result<Value> {
    let registry = embedded_registry()?;
    match command.action {
        DiagnosticScenarioAction::List => {
            let scenarios = registry
                .scenarios
                .iter()
                .map(|scenario| ScenarioListEntry {
                    id: scenario.id.clone(),
                    version: scenario.version,
                    title: scenario.title.clone(),
                    area: scenario.area.clone(),
                    steps: scenario.steps.clone(),
                    authority: scenario.authority.clone(),
                })
                .collect();
            serde_json::to_value(ScenarioListResponse {
                ok: true,
                protocol: registry.protocol,
                scenarios,
            })
            .context("Unable to encode the scenario catalog.")
        }
        DiagnosticScenarioAction::Describe(args) => serde_json::to_value(ScenarioResponse {
            ok: true,
            scenario: find_scenario(&registry, &args.scenario)?.clone(),
        })
        .context("Unable to encode the scenario definition."),
        DiagnosticScenarioAction::Inspect(args) => serde_json::to_value(ScenarioInspectResponse {
            ok: true,
            scenario: find_scenario(&registry, &args.scenario)?.clone(),
            registry_sha256: sha256_hex(EMBEDDED_SCENARIO_REGISTRY),
            schema_sha256: sha256_hex(EMBEDDED_SCENARIO_SCHEMA),
            commands: [
                "list",
                "describe",
                "inspect",
                "plan",
                "timeline",
                "events",
                "selectors",
                "replay",
                "execute",
                "schema",
                "validate",
            ],
        })
        .context("Unable to encode the scenario inspection."),
        DiagnosticScenarioAction::Plan(args) => serde_json::to_value(ScenarioPlanResponse {
            ok: true,
            plan: build_diagnostic_fixture_thread_plan(&registry, &args.scenario)
                .map_err(|error| anyhow::anyhow!(error.to_string()))?,
        })
        .context("Unable to encode the deterministic fixture-thread plan."),
        DiagnosticScenarioAction::Timeline(args) => {
            serde_json::to_value(ScenarioTimelineResponse {
                ok: true,
                timeline: build_diagnostic_fixture_thread_timeline(&registry, &args.scenario)
                    .map_err(|error| anyhow::anyhow!(error.to_string()))?,
            })
            .context("Unable to encode the deterministic fixture-thread timeline.")
        }
        DiagnosticScenarioAction::Events(args) => {
            let stream = build_diagnostic_fixture_event_stream(&registry, &args.scenario)
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            if let Some(selection) = select_catalog_events(&stream, args)? {
                serde_json::to_value(selection)
                    .context("Unable to encode the selected fixture events.")
            } else {
                serde_json::to_value(ScenarioEventsResponse {
                    ok: true,
                    events: stream,
                })
                .context("Unable to encode the redacted fixture event stream.")
            }
        }
        DiagnosticScenarioAction::Selectors(args) => {
            serde_json::to_value(ScenarioSelectorsResponse {
                ok: true,
                selectors: build_diagnostic_fixture_selectors(&registry, &args.scenario)
                    .map_err(|error| anyhow::anyhow!(error.to_string()))?,
            })
            .context("Unable to encode the catalog-derived fixture selectors.")
        }
        DiagnosticScenarioAction::Replay(args) => serde_json::to_value(ScenarioReplayResponse {
            ok: true,
            replay: catalog_fixture_replay(&args.scenario, &args.parsed_controls()?)?,
        })
        .context("Unable to encode the deterministic fixture replay."),
        DiagnosticScenarioAction::Execute(_) => {
            anyhow::bail!("Scenario execution must use the isolated native fixture controller.")
        }
        DiagnosticScenarioAction::Schema => {
            let schema = serde_json::from_str(EMBEDDED_SCENARIO_SCHEMA)
                .map_err(|_| anyhow::anyhow!("Embedded scenario schema is invalid."))?;
            serde_json::to_value(ScenarioSchemaResponse {
                ok: true,
                schema,
                sha256: sha256_hex(EMBEDDED_SCENARIO_SCHEMA),
            })
            .context("Unable to encode the scenario schema.")
        }
        DiagnosticScenarioAction::Validate => serde_json::to_value(ScenarioValidationResponse {
            ok: true,
            protocol: registry.protocol,
            runner_version: registry.runner_version,
            scenario_count: registry.scenarios.len(),
            area_count: registry.areas.len(),
            sha256: sha256_hex(EMBEDDED_SCENARIO_REGISTRY),
            commercial_boundary: registry.commercial_boundary,
        })
        .context("Unable to encode the scenario validation."),
    }
}

/// Rebuilds the one exact deterministic fixture handoff plus its catalogued
/// presentation defaults. It accepts only a registered id and never names a
/// host path, account, endpoint, executable, credential, or real adapter.
pub(crate) fn catalog_fixture_execution(
    scenario_id: &str,
) -> anyhow::Result<CatalogFixtureExecution> {
    let registry = embedded_registry()?;
    let scenario = find_scenario(&registry, scenario_id)?;
    let appearance = match scenario.defaults.appearance.as_str() {
        "light" => FixtureAppearance::Light,
        "dark" => FixtureAppearance::Dark,
        _ => anyhow::bail!("Embedded scenario catalog has an invalid fixture appearance."),
    };
    Ok(CatalogFixtureExecution {
        execution: build_diagnostic_fixture_execution(&registry, scenario_id)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?,
        replay: None,
        width: scenario.defaults.width,
        height: scenario.defaults.height,
        appearance,
        // Catalog scenarios remain immutable. Direct fixture theme selection
        // never widens their catalog-owned presentation contract.
        theme: FixtureTheme::System,
        presentation: FixturePresentation {
            locale: scenario.defaults.locale.clone(),
            layout_direction: FixtureLayoutDirection::LeftToRight,
            font_scale: FixtureFontScale::Standard,
            reduce_motion: scenario.defaults.reduce_motion,
            increased_contrast: scenario.defaults.increased_contrast,
        },
    })
}

/// Returns the fixed catalog attestation for a retained fixture evidence
/// directory. The only input is a registered scenario id; source paths,
/// accounts, endpoints, credentials, and executable state remain unavailable.
pub(crate) fn catalog_fixture_evidence_contract(
    scenario_id: &str,
) -> anyhow::Result<CatalogFixtureEvidenceContract> {
    let registry = embedded_registry()?;
    let scenario = find_scenario(&registry, scenario_id)?;
    Ok(CatalogFixtureEvidenceContract {
        runner_version: registry.runner_version.clone(),
        registry_sha256: sha256_hex(EMBEDDED_SCENARIO_REGISTRY),
        schema_sha256: sha256_hex(EMBEDDED_SCENARIO_SCHEMA),
        scenario_id: scenario.id.clone(),
        scenario_version: scenario.version,
        authority: scenario.authority.clone(),
        fixture: scenario.fixture.clone(),
        required_evidence: scenario.required_evidence.clone(),
        assertions: scenario.assertions.clone(),
        steps: scenario.steps.clone(),
    })
}

/// Rebuilds the exact catalog fixture handoff plus one reducer-derived replay
/// snapshot. The replay accepts only the closed control sequence and remains
/// tied to the same immutable execution that will cross the fixture boundary.
pub(crate) fn catalog_fixture_execution_with_replay(
    scenario_id: &str,
    controls: &[DiagnosticFixtureReplayControl],
) -> anyhow::Result<CatalogFixtureExecution> {
    let mut catalog = catalog_fixture_execution(scenario_id)?;
    catalog.replay = Some(catalog_fixture_replay(scenario_id, controls)?);
    Ok(catalog)
}

/// Validates one bounded redacted replay bundle against the exact catalog,
/// schema, registry version, scenario version, and reducer accepted by this
/// installed CLI. No bundle field is forwarded as UI state: after validation
/// the fixture request is regenerated from immutable catalog data.
pub(crate) fn catalog_fixture_execution_from_redacted_replay_bundle(
    bytes: &[u8],
) -> anyhow::Result<CatalogFixtureExecution> {
    let bundle: RedactedCatalogReplayBundle = serde_json::from_slice(bytes)
        .context("Replay bundle must use the fixed catalog-replay schema.")?;
    if bundle.schema_version != CATALOG_REPLAY_BUNDLE_SCHEMA_VERSION
        || bundle.protocol != CATALOG_REPLAY_BUNDLE_PROTOCOL
        || bundle.registry_schema_version != DIAGNOSTIC_SCENARIO_REGISTRY_SCHEMA_VERSION
        || bundle.registry_protocol != DIAGNOSTIC_SCENARIO_REGISTRY_PROTOCOL
        || bundle.registry_runner_version != DIAGNOSTIC_SCENARIO_RUNNER_VERSION
        || bundle.registry_sha256 != sha256_hex(EMBEDDED_SCENARIO_REGISTRY)
        || bundle.schema_sha256 != sha256_hex(EMBEDDED_SCENARIO_SCHEMA)
    {
        anyhow::bail!("Replay bundle does not match this installed scenario catalog.");
    }
    let controls = strict_redacted_replay_controls(&bundle.controls)?;
    let registry = embedded_registry()?;
    let scenario = find_scenario(&registry, &bundle.scenario_id)?;
    if scenario.version != bundle.scenario_version {
        anyhow::bail!("Replay bundle does not match the registered scenario version.");
    }
    catalog_fixture_execution_with_replay(&bundle.scenario_id, &controls)
}

/// Constructs the sole bounded replay bundle accepted by the installed
/// production fixture lane. The caller can select only a registered scenario
/// and the closed reducer controls; every hash, version, and renderable field
/// comes from the compiled catalog.
pub(crate) fn catalog_redacted_replay_bundle(
    scenario_id: &str,
    control_args: &[DiagnosticScenarioReplayControlArg],
) -> anyhow::Result<Vec<u8>> {
    let controls = parse_replay_controls(control_args)?;
    let registry = embedded_registry()?;
    let scenario = find_scenario(&registry, scenario_id)?;
    let controls = controls
        .iter()
        .map(serde_json::to_value)
        .collect::<Result<Vec<_>, _>>()
        .context("Unable to encode the fixed replay controls.")?;
    let bundle = RedactedCatalogReplayBundle {
        schema_version: CATALOG_REPLAY_BUNDLE_SCHEMA_VERSION,
        protocol: CATALOG_REPLAY_BUNDLE_PROTOCOL.to_string(),
        registry_schema_version: DIAGNOSTIC_SCENARIO_REGISTRY_SCHEMA_VERSION,
        registry_protocol: DIAGNOSTIC_SCENARIO_REGISTRY_PROTOCOL.to_string(),
        registry_runner_version: DIAGNOSTIC_SCENARIO_RUNNER_VERSION.to_string(),
        registry_sha256: sha256_hex(EMBEDDED_SCENARIO_REGISTRY),
        schema_sha256: sha256_hex(EMBEDDED_SCENARIO_SCHEMA),
        scenario_id: scenario.id.clone(),
        scenario_version: scenario.version,
        controls,
    };
    let bytes = serde_json::to_vec_pretty(&bundle)
        .context("Unable to encode the catalog replay bundle.")?;
    // The producer and the installed consumer must agree exactly. This also
    // executes the reducer over the closed controls before any file is written.
    let _ = catalog_fixture_execution_from_redacted_replay_bundle(&bytes)?;
    Ok(bytes)
}

/// Reduces the sole current immutable catalog execution for a scenario. This
/// is read-only and cannot launch a fixture or attach to the normal app.
pub(crate) fn catalog_fixture_replay(
    scenario_id: &str,
    controls: &[DiagnosticFixtureReplayControl],
) -> anyhow::Result<DiagnosticFixtureReplayState> {
    let registry = embedded_registry()?;
    build_diagnostic_fixture_replay(&registry, scenario_id, controls)
        .map_err(|error| anyhow::anyhow!(error.to_string()))
}

fn embedded_registry() -> anyhow::Result<DiagnosticScenarioRegistry> {
    parse_diagnostic_scenario_registry(EMBEDDED_SCENARIO_REGISTRY)
        .map_err(|_| anyhow::anyhow!("Embedded scenario catalog is invalid."))
}

fn find_scenario<'a>(
    registry: &'a DiagnosticScenarioRegistry,
    scenario_id: &str,
) -> anyhow::Result<&'a DiagnosticScenario> {
    registry
        .scenarios
        .iter()
        .find(|scenario| scenario.id == scenario_id)
        .ok_or_else(|| anyhow::anyhow!("Unknown registered diagnostic scenario."))
}

/// Selects an inspectable subset while leaving the complete, immutable stream
/// untouched. A caller cannot combine selectors, fabricate an event, or turn
/// the result into a mutable replay input.
fn select_catalog_events(
    stream: &DiagnosticFixtureEventStream,
    args: DiagnosticScenarioEventsArgs,
) -> anyhow::Result<Option<ScenarioEventSelectionResponse>> {
    let selection_count = usize::from(args.tail.is_some())
        + usize::from(args.show.is_some())
        + usize::from(args.kind.is_some());
    if selection_count > 1 {
        anyhow::bail!("Choose only one event selector: --tail, --show, or --kind.");
    }

    let summary = ScenarioEventStreamSummary::from(stream);
    let response = match (args.tail, args.show, args.kind) {
        (None, None, None) => return Ok(None),
        (Some(count), None, None) => {
            if !(1..=MAX_DIAGNOSTIC_FIXTURE_EVENTS).contains(&count) {
                anyhow::bail!(
                    "Event tail count must be between 1 and {MAX_DIAGNOSTIC_FIXTURE_EVENTS}."
                );
            }
            let start = stream.events.len().saturating_sub(count);
            ScenarioEventSelectionResponse {
                ok: true,
                selection: "tail",
                requested_tail_count: Some(count),
                requested_event_id: None,
                requested_kind: None,
                stream: summary,
                events: stream.events[start..].to_vec(),
            }
        }
        (None, Some(event_id), None) => {
            if !valid_catalog_event_id(&event_id) {
                anyhow::bail!("Event id must be a bounded lowercase fixture identifier.");
            }
            let event = stream
                .events
                .iter()
                .find(|event| event.event_id == event_id)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("Unknown catalog event for this scenario."))?;
            ScenarioEventSelectionResponse {
                ok: true,
                selection: "show",
                requested_tail_count: None,
                requested_event_id: Some(event_id),
                requested_kind: None,
                stream: summary,
                events: vec![event],
            }
        }
        (None, None, Some(kind)) => {
            let kind = kind.fixture_kind();
            ScenarioEventSelectionResponse {
                ok: true,
                selection: "filter",
                requested_tail_count: None,
                requested_event_id: None,
                requested_kind: Some(kind),
                stream: summary,
                events: stream
                    .events
                    .iter()
                    .filter(|event| event.kind == kind)
                    .cloned()
                    .collect(),
            }
        }
        _ => unreachable!("multiple selectors are rejected above"),
    };
    Ok(Some(response))
}

fn valid_catalog_event_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
}

fn sha256_hex(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

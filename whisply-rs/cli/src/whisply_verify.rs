//! Fixed verification boundaries for a checked-out Whisply source tree.
//!
//! `whisply debug verify` is intentionally developer-only: it may run a
//! reviewed local developer manifest, but it never accepts a command from the
//! caller, never invokes release shipping, and never passes provider or
//! production credentials to a child process.
//!
//! `whisply test release` is a separate public, zero-authority boundary. Its
//! versioned group plan is fixed in this module and, until an installed fixture
//! host exists, it fails closed rather than spawning a build, shipping, or live
//! integration process.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;
use std::time::Instant;

use anyhow::Context;
use clap::Args;
use clap::ValueEnum;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;
use tempfile::TempDir;

pub(crate) const VERIFY_MANIFEST_VERSION: u16 = 27;
pub(crate) const RELEASE_TEST_MANIFEST_VERSION: u16 = 2;

const ROOT_SENTINELS: &[&str] = &[
    "AGENTS.md",
    "WhisplyCore/Package.swift",
    "WhisplyCore/Package.resolved",
    "Runtime/whisply-codex/whisply-rs/Cargo.toml",
    "scripts/verify_divergence_registry.py",
    "scripts/verify_protected_product_surfaces.py",
    "workers/proxy/package.json",
];

// The managed Skills/Plugins service lives in the reviewed Website sibling
// checkout. It is a fixed workspace—not a user-provided path—and must be the
// immediate sibling of the Whisply source root so debug verify cannot be
// repurposed as an arbitrary local command runner.
const WEBSITE_SIBLING_DIRECTORY: &str = "Whisply Website";
const WEBSITE_SIBLING_ROOT_SENTINELS: &[&str] = &[
    "package.json",
    "package-lock.json",
    "src/routes/v1/skills-plugins.ts",
    "tests/unit/skills-plugins-runtime.test.ts",
];
const WEBSITE_SIBLING_CHECK_IDS: &[&str] = &["website-managed-skills-plugins-contracts"];

// Verification children must never reach an external network destination. The
// loopback and local-Unix-socket exceptions are deliberately narrow: Rust and
// Swift tests use local mock servers and the managed native-broker fixture to
// prove no-I/O contracts, and `sandbox-exec` classifies their bind/connect
// operations as network access too. External TCP/UDP remains denied.
const NETWORK_DENY_SANDBOX_PROFILE: &str = concat!(
    "(version 1) ",
    "(allow default) ",
    "(deny network*) ",
    "(allow network-inbound (local ip \"localhost:*\")) ",
    "(allow network-outbound (remote ip \"localhost:*\")) ",
    "(allow network-inbound (local unix-socket)) ",
    "(allow network-outbound (remote unix-socket))",
);
const PINNED_RUST_TOOLCHAIN: &str = "1.95.0";
const PINNED_NODE_VERSION: &str = "26.3.0";
const RUST_VERIFY_STACK_BYTES: &str = "67108864";
const TRUSTED_CHILD_SYSTEM_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";
const RUSTY_V8_ARCHIVE_ENV: &str = "RUSTY_V8_ARCHIVE";
const RUSTY_V8_BINDING_ENV: &str = "RUSTY_V8_SRC_BINDING_PATH";
const RUSTY_V8_ARTIFACT_PROFILE: &str = "ptrcomp_sandbox_release";

/// A locally verified pair of Codex-built V8 artifacts. The debug verifier
/// deliberately does not fetch this pair: fetching would require external
/// network authority. A reviewed bootstrap provides the pair and its sibling
/// checksum manifest; this runner verifies it again before allowing Cargo to
/// see either path.
#[derive(Debug)]
struct VerifiedRustyV8Artifacts {
    archive: PathBuf,
    binding: PathBuf,
}

/// Per-child empty user and product state. Developer tool caches are supplied
/// explicitly for Cargo, so a fixed verification child never falls back to a
/// user's real `HOME`, Whisply configuration, auth files, or credential-store
/// selection. Each test-owned `WHISPLY_HOME` chooses its own SQLite state,
/// rather than reaching another test's files or the person's account.
struct IsolatedProductHome {
    _root: TempDir,
    home: PathBuf,
    whisply_home: PathBuf,
}

impl IsolatedProductHome {
    fn create() -> anyhow::Result<Self> {
        let root = tempfile::Builder::new()
            .prefix("whisply-debug-verify-")
            .tempdir()
            .context("Could not create an isolated Whisply product home for debug verify")?;
        let home = root.path().join("home");
        let whisply_home = root.path().join("whisply-home");
        for path in [&home, &whisply_home] {
            std::fs::create_dir(path).with_context(|| {
                format!(
                    "Could not create isolated debug verify product state at {}",
                    path.display()
                )
            })?;
        }
        Ok(Self {
            _root: root,
            home,
            whisply_home,
        })
    }
}

#[derive(Debug, Args)]
pub(crate) struct VerifyCommand {
    /// List the versioned fixed verification manifest without executing checks.
    #[arg(long)]
    list: bool,

    /// Verification suite to run. `all` intentionally excludes live integration checks.
    #[arg(long, value_enum, default_value_t = VerifySuite::All)]
    suite: VerifySuite,

    /// Restrict execution to the fixed checks that cover one named user-configured feature.
    /// Repeat this flag to select multiple feature lanes; arbitrary paths and commands stay unavailable.
    #[arg(long = "feature", value_enum)]
    features: Vec<VerifyFeature>,

    /// Report the checks that would run without spawning child processes.
    #[arg(long)]
    dry_run: bool,

    /// Emit a machine-readable report; child output is intentionally suppressed.
    #[arg(long)]
    json: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub(crate) enum VerifySuite {
    Static,
    Rust,
    Mac,
    Website,
    Integration,
    All,
}

/// A fixed, user-facing selector for the existing automated feature-coverage
/// matrix. This is not a capability allowlist: it only makes the reviewed
/// local/custom coverage lanes directly runnable without accepting a caller
/// supplied test command, source path, or configuration file.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
enum VerifyFeature {
    #[value(name = "goals-and-plan-updates")]
    GoalsAndPlanUpdates,
    #[value(name = "hooks")]
    Hooks,
    #[value(name = "custom-mcp-stdio-http-oauth")]
    CustomMcpStdioHttpOauth,
    #[value(name = "plugins")]
    Plugins,
    #[value(name = "skills")]
    Skills,
    #[value(name = "agents")]
    Agents,
    #[value(name = "local-execution-and-file-editing")]
    LocalExecutionAndFileEditing,
    #[value(name = "memory-audio-and-transcripts")]
    MemoryAudioAndTranscripts,
    #[value(name = "browser-and-computer-use-interfaces")]
    BrowserAndComputerUseInterfaces,
    #[value(name = "connectors")]
    Connectors,
    #[value(name = "cli-and-tui")]
    CliAndTui,
}

impl VerifyFeature {
    const fn coverage_id(self) -> &'static str {
        match self {
            Self::GoalsAndPlanUpdates => "goals_and_plan_updates",
            Self::Hooks => "hooks",
            Self::CustomMcpStdioHttpOauth => "custom_mcp_stdio_http_oauth",
            Self::Plugins => "plugins",
            Self::Skills => "skills",
            Self::Agents => "agents",
            Self::LocalExecutionAndFileEditing => "local_execution_and_file_editing",
            Self::MemoryAudioAndTranscripts => "memory_audio_and_transcripts",
            Self::BrowserAndComputerUseInterfaces => "browser_and_computer_use_interfaces",
            Self::Connectors => "connectors",
            Self::CliAndTui => "cli_and_tui",
        }
    }
}

/// The only public release-test entry point. It deliberately has no command,
/// executable, config-path, authority, or confirmation arguments.
#[derive(Debug, Args)]
pub(crate) struct TestCommand {
    #[command(subcommand)]
    subcommand: TestSubcommand,
}

#[derive(Debug, clap::Subcommand)]
enum TestSubcommand {
    /// Run the fixed, zero-authority release-test plan.
    Release(ReleaseTestCommand),
}

#[derive(Debug, Args)]
struct ReleaseTestCommand {
    /// Named fixed release-test group. `full` selects every defined group.
    #[arg(long, value_enum, default_value_t = ReleaseTestGroup::Full)]
    group: ReleaseTestGroup,

    /// List the versioned fixed release-test plan without executing it.
    #[arg(long)]
    list: bool,

    /// Report the fixed groups that would be attempted without running them.
    #[arg(long)]
    dry_run: bool,

    /// Emit a machine-readable fixed-plan report.
    #[arg(long)]
    json: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
enum ReleaseTestGroup {
    Schema,
    Runtime,
    Ui,
    Policy,
    Provider,
    Tool,
    Native,
    Connector,
    Installed,
    Affected,
    Full,
}

#[derive(Clone, Copy, Debug, Serialize)]
struct ReleaseTestGroupDefinition {
    group: ReleaseTestGroup,
    description: &'static str,
}

const RELEASE_TEST_GROUPS: &[ReleaseTestGroupDefinition] = &[
    ReleaseTestGroupDefinition {
        group: ReleaseTestGroup::Schema,
        description: "Schema, registry, and release-manifest contracts.",
    },
    ReleaseTestGroupDefinition {
        group: ReleaseTestGroup::Runtime,
        description: "Deterministic runtime replay and launch contracts.",
    },
    ReleaseTestGroupDefinition {
        group: ReleaseTestGroup::Ui,
        description: "Zero-authority UI fixture and presentation contracts.",
    },
    ReleaseTestGroupDefinition {
        group: ReleaseTestGroup::Policy,
        description: "Subscription, entitlement, scope, and confirmation denial contracts.",
    },
    ReleaseTestGroupDefinition {
        group: ReleaseTestGroup::Provider,
        description: "Provider-family integration evidence under normal Usage.",
    },
    ReleaseTestGroupDefinition {
        group: ReleaseTestGroup::Tool,
        description: "CLI and Mac tool-policy parity contracts.",
    },
    ReleaseTestGroupDefinition {
        group: ReleaseTestGroup::Native,
        description: "Native setup, permission, cancellation, and cleanup contracts.",
    },
    ReleaseTestGroupDefinition {
        group: ReleaseTestGroup::Connector,
        description: "Connector read, revoke, and reconnect contracts.",
    },
    ReleaseTestGroupDefinition {
        group: ReleaseTestGroup::Installed,
        description: "Exact installed-artifact fixture and regression evidence.",
    },
    ReleaseTestGroupDefinition {
        group: ReleaseTestGroup::Affected,
        description: "Affected-surface regression selection from the fixed release plan.",
    },
    ReleaseTestGroupDefinition {
        group: ReleaseTestGroup::Full,
        description: "Every fixed zero-authority release-test group.",
    },
];

const RELEASE_TEST_PROHIBITED_ACTIONS: &[&str] = &[
    "build",
    "deploy",
    "publish",
    "appcast mutation",
    "authority grant",
    "final confirmation",
    "scripts/ship.sh",
];

/// Fixed, source-owned evidence for ordinary user-configured Codex features.
///
/// This is a coverage inventory, not a product capability allowlist: the
/// checks exercise the existing local/custom fixtures and must never be used
/// to remove a feature merely because it is not a proprietary hosted rail.
/// `later_integration_evidence` names the deliberately non-automated macOS or
/// consent evidence that cannot be safely simulated by the offline runner.
#[derive(Clone, Copy, Debug, Serialize)]
struct UserConfiguredFeatureCoverage {
    feature: &'static str,
    release_group: ReleaseTestGroup,
    automated_check_ids: &'static [&'static str],
    fixture_paths: &'static [&'static str],
    later_integration_evidence: Option<&'static str>,
}

const USER_CONFIGURED_FEATURE_COVERAGE: &[UserConfiguredFeatureCoverage] = &[
    UserConfiguredFeatureCoverage {
        feature: "goals_and_plan_updates",
        release_group: ReleaseTestGroup::Tool,
        automated_check_ids: &[
            "rust-core-full",
            "rust-goal-extension-full",
            "rust-app-server-v2-full",
        ],
        fixture_paths: &[
            "Runtime/whisply-codex/whisply-rs/core/src/tools/spec_plan_tests.rs",
            "Runtime/whisply-codex/whisply-rs/core/tests/suite/tool_harness.rs",
            "Runtime/whisply-codex/whisply-rs/ext/goal/tests/goal_extension_backend.rs",
            "Runtime/whisply-codex/whisply-rs/app-server/tests/suite/v2/thread_resume.rs",
        ],
        later_integration_evidence: None,
    },
    UserConfiguredFeatureCoverage {
        feature: "hooks",
        release_group: ReleaseTestGroup::Tool,
        automated_check_ids: &["rust-core-full"],
        fixture_paths: &[
            "Runtime/whisply-codex/whisply-rs/core/tests/suite/hooks.rs",
            "Runtime/whisply-codex/whisply-rs/core/tests/suite/hooks_mcp.rs",
        ],
        later_integration_evidence: None,
    },
    UserConfiguredFeatureCoverage {
        feature: "custom_mcp_stdio_http_oauth",
        release_group: ReleaseTestGroup::Tool,
        automated_check_ids: &[
            "rust-core-full",
            "rust-rmcp-client-full",
            "rust-codex-mcp-full",
            "rust-mcp-extension-full",
            "rust-mcp-server-full",
            "rust-app-server-v2-full",
        ],
        fixture_paths: &[
            "Runtime/whisply-codex/whisply-rs/core/tests/suite/rmcp_client.rs",
            "Runtime/whisply-codex/whisply-rs/rmcp-client/tests/streamable_http_oauth_startup.rs",
            "Runtime/whisply-codex/whisply-rs/rmcp-client/tests/resources.rs",
        ],
        later_integration_evidence: Some(
            "User-initiated OAuth consent against a real third-party server remains an owner integration scenario.",
        ),
    },
    UserConfiguredFeatureCoverage {
        feature: "plugins",
        release_group: ReleaseTestGroup::Tool,
        automated_check_ids: &[
            "rust-core-full",
            "rust-plugin-core-full",
            "rust-plugin-manifest-full",
            "rust-external-agent-migration-full",
            "rust-app-server-v2-full",
            "rust-tui-full",
            "mac-managed-skills-plugins-contracts",
            "website-managed-skills-plugins-contracts",
        ],
        fixture_paths: &[
            "Runtime/whisply-codex/whisply-rs/core/tests/suite/plugins.rs",
            "Runtime/whisply-codex/whisply-rs/core-plugins/src/store_tests.rs",
            "Runtime/whisply-codex/whisply-rs/core-plugins/src/loader_tests.rs",
            "../Whisply Website/tests/unit/skills-plugins-runtime.test.ts",
            "../Whisply Website/tests/unit/skills-plugins-route.test.ts",
        ],
        later_integration_evidence: Some(
            "Installed local-package selection remains a macOS/UI integration scenario.",
        ),
    },
    UserConfiguredFeatureCoverage {
        feature: "skills",
        release_group: ReleaseTestGroup::Tool,
        automated_check_ids: &[
            "rust-core-full",
            "rust-skills-full",
            "rust-core-skills-full",
            "rust-skills-extension-full",
            "rust-external-agent-migration-full",
            "rust-app-server-v2-full",
            "rust-tui-full",
            "mac-managed-skills-plugins-contracts",
            "website-managed-skills-plugins-contracts",
        ],
        fixture_paths: &[
            "Runtime/whisply-codex/whisply-rs/core/tests/suite/skills.rs",
            "Runtime/whisply-codex/whisply-rs/core/tests/suite/skills_extension.rs",
            "Runtime/whisply-codex/whisply-rs/cli/src/whisply_skills.rs",
            "Runtime/whisply-codex/whisply-rs/skills/src/selection_tests.rs",
            "Runtime/whisply-codex/whisply-rs/ext/skills/tests/skills_extension.rs",
            "../Whisply Website/tests/unit/skill-claim-replay-contract.test.ts",
            "../Whisply Website/tests/unit/skill-transform-funding.test.ts",
        ],
        later_integration_evidence: Some(
            "Interactive skill setup and invocation presentation remain macOS/UI integration scenarios.",
        ),
    },
    UserConfiguredFeatureCoverage {
        feature: "agents",
        release_group: ReleaseTestGroup::Runtime,
        automated_check_ids: &["rust-core-full", "rust-app-server-v2-full"],
        fixture_paths: &[
            "Runtime/whisply-codex/whisply-rs/core/tests/suite/multi_agent_resume.rs",
            "Runtime/whisply-codex/whisply-rs/core/tests/suite/agent_execution.rs",
        ],
        later_integration_evidence: None,
    },
    UserConfiguredFeatureCoverage {
        feature: "local_execution_and_file_editing",
        release_group: ReleaseTestGroup::Tool,
        automated_check_ids: &[
            "rust-core-full",
            "rust-exec-server-full",
            "rust-exec-full",
            "mac-swift-package-full",
        ],
        fixture_paths: &[
            "Runtime/whisply-codex/whisply-rs/core/tests/suite/apply_patch_cli.rs",
            "Runtime/whisply-codex/whisply-rs/core/tests/suite/unified_exec.rs",
            "WhisplyCore/Tests/WhisplyCoreTests/LocalFileAccessServiceTests.swift",
        ],
        later_integration_evidence: Some(
            "Real macOS file-picker and TCC permission presentation remain device/UI evidence.",
        ),
    },
    UserConfiguredFeatureCoverage {
        feature: "memory_audio_and_transcripts",
        release_group: ReleaseTestGroup::Native,
        automated_check_ids: &["mac-memory-audio-transcript-contracts"],
        fixture_paths: &[
            "WhisplyCore/Tests/WhisplyCoreTests/ContextualMemoryCommandSafetyTests.swift",
            "WhisplyCore/Tests/WhisplyCoreTests/ContextualAccountServiceTests.swift",
            "WhisplyCore/Tests/WhisplyCoreTests/ConversationContextCompactionBoundaryTests.swift",
            "WhisplyCore/Tests/WhisplyCoreTests/ConversationContextCompactionTests.swift",
            "WhisplyCore/Tests/WhisplyCoreTests/ConversationResponseContextTests.swift",
            "WhisplyCore/Tests/WhisplyCoreTests/RealtimeTranscriptionContractTests.swift",
            "WhisplyCore/Tests/WhisplyCoreTests/TranscriptPrivacyContractTests.swift",
            "WhisplyCore/Tests/WhisplyCoreTests/TranscriptPrivacyOverrideStoreTests.swift",
            "WhisplyCore/Tests/WhisplyCoreTests/TranscriptStopLifecycleTests.swift",
            "WhisplyCore/Tests/WhisplyCoreTests/TranscriptTaskOpportunityTests.swift",
            "WhisplyCore/Tests/WhisplyCoreTests/TranscriptionArtifactFilterTests.swift",
            "WhisplyCore/Tests/WhisplyCoreTests/PreferencesProductContractTests.swift",
            "WhisplyCore/Tests/WhisplyCoreTests/SessionWorkspaceTests.swift",
            "WhisplyCore/Tests/WhisplyCoreTests/ChatHistoryMutationBarrierTests.swift",
        ],
        later_integration_evidence: Some(
            "The fixed offline suite covers memory controls, attribution, compaction separation, transcription routing, transcript privacy, and explicit capture intent. Fresh-candidate microphone or system-audio TCC, live capture, overlay presentation, and owner acceptance remain device/UI evidence.",
        ),
    },
    UserConfiguredFeatureCoverage {
        feature: "browser_and_computer_use_interfaces",
        release_group: ReleaseTestGroup::Native,
        automated_check_ids: &[
            "static-diagnostic-fixture-contract",
            "mac-swift-package-full",
            "mac-diagnostic-fixture-scenarios-full",
        ],
        fixture_paths: &[
            "WhisplyCore/Tests/WhisplyCoreTests/BrowserComputerUseTests.swift",
            "WhisplyCore/Tests/WhisplyCoreTests/BrowserLogicalConnectionTests.swift",
            "WhisplyCore/Tests/WhisplyCoreTests/BrowserConnectionPresentationTests.swift",
            "WhisplyCore/Tests/WhisplyCoreTests/ChromePluginIntegrationStatusTests.swift",
            "WhisplyCore/Tests/WhisplyCoreTests/DirectBrowserTaskCoordinatorTests.swift",
            "WhisplyCore/Tests/WhisplyCoreTests/ComputerUsePolicyTests.swift",
            "WhisplyCore/Tests/WhisplyCoreTests/ComputerUseSelectedModelRoutingTests.swift",
            "WhisplyCore/Tests/WhisplyCoreTests/ComputerUseHelperActionRoutingTests.swift",
            "docs/contextual-action-layer/diagnostic-scenarios.json",
            "scripts/test_whisply_diagnostics.py",
        ],
        later_integration_evidence: Some(
            "Automated Swift coverage verifies the existing Browser/Chrome/Computer Use policy and adapter contracts plus 25 deterministic zero-authority presentation scenarios. The owner-only installed-app diagnostics bridge exercises the established standalone route; it does not claim or enable the separate first-party model-to-native app-server route. Fresh-candidate Browser/Chrome setup, extension connection, TCC prompts, overlay, and Stop behavior require macOS/UI evidence.",
        ),
    },
    UserConfiguredFeatureCoverage {
        feature: "connectors",
        release_group: ReleaseTestGroup::Connector,
        automated_check_ids: &[
            "static-diagnostic-fixture-contract",
            "mac-swift-package-full",
            "mac-diagnostic-fixture-scenarios-full",
        ],
        fixture_paths: &[
            "WhisplyCore/Tests/WhisplyCoreTests/ConnectorRuntimeContractTests.swift",
            "WhisplyCore/Tests/WhisplyCoreTests/DirectConnectorChatToolLoopTests.swift",
            "docs/contextual-action-layer/diagnostic-scenarios.json",
            "scripts/test_whisply_diagnostics.py",
        ],
        later_integration_evidence: Some(
            "Automated Swift coverage verifies typed connector policy and adapter contracts plus deterministic zero-authority connector presentation scenarios. The owner-only installed-app diagnostics bridge exercises the established connected-source route after normal consent; it does not claim or enable a separate first-party model-to-connector app-server route. Provider consent, reconnect, and revoke require fresh-candidate integration evidence.",
        ),
    },
    UserConfiguredFeatureCoverage {
        feature: "cli_and_tui",
        release_group: ReleaseTestGroup::Ui,
        automated_check_ids: &[
            "rust-cli-full",
            "rust-tui-full",
            "rust-exec-full",
            "rust-exec-server-full",
        ],
        fixture_paths: &[
            "Runtime/whisply-codex/whisply-rs/cli/tests/provider_authority.rs",
            "Runtime/whisply-codex/whisply-rs/cli/tests/login.rs",
            "Runtime/whisply-codex/whisply-rs/cli/tests/plugin_cli.rs",
            "Runtime/whisply-codex/whisply-rs/cli/tests/mcp_add_remove.rs",
            "Runtime/whisply-codex/whisply-rs/exec/tests/suite/approval_policy.rs",
            "Runtime/whisply-codex/whisply-rs/exec/tests/suite/output_schema.rs",
            "Runtime/whisply-codex/whisply-rs/exec/tests/suite/server_error_exit.rs",
            "Runtime/whisply-codex/whisply-rs/exec/tests/event_processor_with_json_output.rs",
            "Runtime/whisply-codex/whisply-rs/tui/tests/suite/vt100_live_commit.rs",
            "Runtime/whisply-codex/whisply-rs/tui/tests/suite/status_indicator.rs",
        ],
        later_integration_evidence: Some(
            "The fixed offline lane covers the Whisply command surface, terminal rendering, approval-policy selection, structured output, versioned JSONL events, and machine-readable exit codes. It keeps the preserved upstream MCP, plugin, and skill command families runnable rather than treating them as removable. Terminal-only reporting of macOS-native setup routes and interactive confirmation presentation remain device evidence.",
        ),
    },
];

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum CheckAvailability {
    Runnable,
    ExplicitSelectionOnly,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum CheckStatus {
    Passed,
    Failed,
    Blocked,
    DryRun,
}

#[derive(Clone, Copy, Debug)]
enum CheckAction {
    Run {
        program: &'static str,
        args: &'static [&'static str],
        working_directory: &'static str,
        required_paths: &'static [&'static str],
    },
    Blocked,
}

/// Determines whether a fixed verification child is wrapped by the verifier's
/// network-deny Seatbelt profile or must exercise Seatbelt itself.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VerificationSandbox {
    OuterNetworkDeny,
    TestSuiteOwnsSeatbelt,
}

#[derive(Clone, Copy, Debug)]
struct VerificationCheck {
    id: &'static str,
    suite: VerifySuite,
    availability: CheckAvailability,
    action: CheckAction,
    remediation: &'static str,
    failure_summary: &'static str,
}

// These are fixed local build prerequisites, not additional product-feature
// coverage claims. They must be selected and run before the dependent check
// even when a caller narrows verification to a named feature lane.
const VERIFY_CHECK_PREREQUISITES: &[(&str, &[&str])] = &[
    (
        "rust-core-full",
        &[
            "rust-app-server-helper-cli",
            "rust-app-server-helper-code-mode",
            "rust-app-server-helper-stdio",
        ],
    ),
    (
        "rust-app-server-v2-full",
        &[
            "rust-app-server-helper-cli",
            "rust-app-server-helper-stdio",
            "rust-app-server-helper-code-mode",
        ],
    ),
];

fn verification_sandbox(check: &VerificationCheck) -> VerificationSandbox {
    match check.id {
        // The full v2 app-server and exec suites exercise the product's real
        // sandbox-exec path. macOS rejects that intentionally nested Seatbelt
        // invocation when the test binary is already wrapped by sandbox-exec,
        // so these fixed suites must own the inner sandbox. The diagnostic
        // fixture wave starts its own per-scenario deny-network host; the four
        // Rust suites exercise the product's own sandbox-exec path. Their verifier
        // children remain credential-free, Cargo-offline, and use isolated
        // product homes; all other fixed checks keep the outer
        // external-network-deny boundary.
        "rust-app-server-v2-full"
        | "rust-core-full"
        | "rust-exec-server-full"
        | "rust-exec-full"
        | "mac-diagnostic-fixture-scenarios-full" => VerificationSandbox::TestSuiteOwnsSeatbelt,
        _ => VerificationSandbox::OuterNetworkDeny,
    }
}

const VERIFY_MANIFEST: &[VerificationCheck] = &[
    VerificationCheck {
        id: "static-protected-product-surfaces",
        suite: VerifySuite::Static,
        availability: CheckAvailability::Runnable,
        action: CheckAction::Run {
            program: "python3",
            args: &["scripts/verify_protected_product_surfaces.py"],
            working_directory: ".",
            required_paths: &["scripts/verify_protected_product_surfaces.py"],
        },
        remediation: "Repair the protected source contract or update its reviewed manifest deliberately.",
        failure_summary: "Protected product surface verification did not complete.",
    },
    VerificationCheck {
        id: "static-contextual-action-artifacts",
        suite: VerifySuite::Static,
        availability: CheckAvailability::Runnable,
        action: CheckAction::Run {
            program: "python3",
            args: &["scripts/verify_contextual_action_artifacts.py"],
            working_directory: ".",
            required_paths: &["scripts/verify_contextual_action_artifacts.py"],
        },
        remediation: "Repair the immutable requirement catalog, supplemental decisions, or release-evidence metadata before continuing.",
        failure_summary: "Contextual-action artifact verification did not complete.",
    },
    VerificationCheck {
        id: "static-wcd-evidence-ledger",
        suite: VerifySuite::Static,
        availability: CheckAvailability::Runnable,
        action: CheckAction::Run {
            program: "python3",
            args: &["scripts/verify_wcd_evidence_ledger.py", "--json"],
            working_directory: ".",
            required_paths: &[
                "scripts/verify_wcd_evidence_ledger.py",
                "docs/whisply-codex-distribution-evidence.json",
            ],
        },
        remediation: "Reconcile every WCD requirement with current implementation, verification, release, and owner evidence; do not treat planning checkboxes as release proof.",
        failure_summary: "WCD evidence-ledger verification did not complete.",
    },
    VerificationCheck {
        id: "static-divergence-registry-audit",
        suite: VerifySuite::Static,
        availability: CheckAvailability::Runnable,
        action: CheckAction::Run {
            program: "python3",
            args: &["scripts/verify_divergence_registry.py", "--json"],
            working_directory: ".",
            required_paths: &[
                "scripts/verify_divergence_registry.py",
                "Runtime/divergence-registry.md",
                "Runtime/divergence-registry.audit.json",
                "Runtime/upstream-base.json",
            ],
        },
        remediation: "Map every committed fork change to a reviewed TDR source scope; do not add a silent runtime or adapter escape hatch.",
        failure_summary: "Divergence-registry release audit did not complete.",
    },
    VerificationCheck {
        id: "static-runtime-cloud-config-authority",
        suite: VerifySuite::Static,
        availability: CheckAvailability::Runnable,
        action: CheckAction::Run {
            program: "python3",
            args: &[
                "-m",
                "unittest",
                "discover",
                "-s",
                "tests",
                "-p",
                "test_whisply_runtime_cloud_config_authority.py",
            ],
            working_directory: ".",
            required_paths: &["tests/test_whisply_runtime_cloud_config_authority.py"],
        },
        remediation: "Restore the managed no-cloud runtime boundary before continuing.",
        failure_summary: "Runtime cloud-config authority verification did not complete.",
    },
    VerificationCheck {
        id: "static-gateway-installation-binding-contract",
        suite: VerifySuite::Static,
        availability: CheckAvailability::Runnable,
        action: CheckAction::Run {
            program: "python3",
            args: &[
                "-m",
                "unittest",
                "discover",
                "-s",
                "tests",
                "-p",
                "test_gateway_installation_binding_contract.py",
            ],
            working_directory: ".",
            required_paths: &[
                "tests/test_gateway_installation_binding_contract.py",
                "supabase/functions/mint-proxy-token/index.ts",
                "WhisplyCore/Sources/WhisplyNativeBrokerService/main.swift",
                "WhisplyCore/Sources/WhisplyApp/Services/WhisplyProductServices.swift",
                "workers/proxy/src/auth.ts",
                "workers/proxy/src/runtime-gateway.ts",
            ],
        },
        remediation: "Keep gateway capabilities bound to the verified account, registered device, session, and installation tuple before they are minted.",
        failure_summary: "Gateway installation-binding verification did not complete.",
    },
    VerificationCheck {
        id: "static-debug-verify-contract",
        suite: VerifySuite::Static,
        availability: CheckAvailability::Runnable,
        action: CheckAction::Run {
            program: "python3",
            args: &[
                "-m",
                "unittest",
                "discover",
                "-s",
                "tests",
                "-p",
                "test_whisply_debug_verify_contract.py",
            ],
            working_directory: ".",
            required_paths: &["tests/test_whisply_debug_verify_contract.py"],
        },
        remediation: "Keep debug verify fixed-manifest, source-root-bound, and credential-free.",
        failure_summary: "Debug verifier contract verification did not complete.",
    },
    VerificationCheck {
        id: "static-installed-live-diagnostics-contract",
        suite: VerifySuite::Static,
        availability: CheckAvailability::Runnable,
        action: CheckAction::Run {
            program: "python3",
            args: &[
                "-m",
                "unittest",
                "discover",
                "-s",
                "tests",
                "-p",
                "test_installed_live_diagnostics_contract.py",
            ],
            working_directory: ".",
            required_paths: &[
                "tests/test_installed_live_diagnostics_contract.py",
                "Runtime/whisply-codex/whisply-rs/cli/src/whisply_diagnostics/installed_live.rs",
                "WhisplyCore/Sources/WhisplyApp/Services/LiveDiagnosticController.swift",
            ],
        },
        remediation: "Keep installed-app diagnostics explicit, release-bound, and aligned with the owner-only Swift live protocol without making it a release or fixture runner.",
        failure_summary: "Installed live-diagnostics contract verification did not complete.",
    },
    VerificationCheck {
        id: "static-live-diagnostics-security",
        suite: VerifySuite::Static,
        availability: CheckAvailability::Runnable,
        action: CheckAction::Run {
            program: "python3",
            args: &[
                "-m",
                "unittest",
                "discover",
                "-s",
                "scripts",
                "-p",
                "test_whisply_live_diagnostics.py",
            ],
            working_directory: ".",
            required_paths: &[
                "scripts/test_whisply_live_diagnostics.py",
                "scripts/whisply_diagnostics.py",
            ],
        },
        remediation: "Keep default live-diagnostic exports owner-only, non-exact, release-bound, and redacted under the supported macOS Python runtime.",
        failure_summary: "Live-diagnostic export and release-binding verification did not complete.",
    },
    VerificationCheck {
        id: "static-release-feature-coverage",
        suite: VerifySuite::Static,
        availability: CheckAvailability::Runnable,
        action: CheckAction::Run {
            program: "python3",
            args: &[
                "-m",
                "unittest",
                "discover",
                "-s",
                "tests",
                "-p",
                "test_release_feature_coverage.py",
            ],
            working_directory: ".",
            required_paths: &[
                "tests/test_release_feature_coverage.py",
                "docs/release-validation-runbook.md",
                "Runtime/whisply-codex/whisply-rs/app-server/tests/suite/v2/mod.rs",
                "Runtime/whisply-codex/whisply-rs/whisply-runtime/tests/fixtures/whisply-model-selector-test-v1.json",
                "Runtime/contracts/fixtures/whisply-tool-lifecycle-v1.json",
                "Runtime/contracts/fixtures/whisply-session-account-behavior-v1.json",
                "Runtime/contracts/fixtures/whisply-session-account-behavior-v1.sha256",
                "workers/proxy/src/model-catalog.ts",
                "WhisplyCore/Sources/WhisplyApp/Services/ChatModelCatalog.swift",
            ],
        },
        remediation: "Keep every named release-facing feature in the fixed automated coverage manifest or document a narrowly scoped host-only exception explicitly.",
        failure_summary: "Release feature-coverage contract verification did not complete.",
    },
    VerificationCheck {
        id: "static-protected-window-invariant",
        suite: VerifySuite::Static,
        availability: CheckAvailability::Runnable,
        action: CheckAction::Run {
            program: "python3",
            args: &[
                "-m",
                "unittest",
                "discover",
                "-s",
                "tests",
                "-p",
                "test_verify_panels.py",
            ],
            working_directory: ".",
            required_paths: &[
                "tests/test_verify_panels.py",
                "scripts/verify_panels.sh",
                "WhisplyCore/Sources/WhisplyUI/Panels/WhisplyProtectedWindow.swift",
            ],
        },
        remediation: "Route every stealth-eligible surface through WhisplyProtectedPanel/WhisplyProtectedWindow. Justify any escape inline with `// verify_panels:allow reason: <why>`, and never bypass the protected base from a broker-presented tree.",
        failure_summary: "Protected window/panel construction invariant verification did not complete.",
    },
    VerificationCheck {
        id: "static-product-contract-verifiers",
        suite: VerifySuite::Static,
        availability: CheckAvailability::Runnable,
        action: CheckAction::Run {
            program: "python3",
            args: &[
                "-m",
                "unittest",
                "discover",
                "-s",
                "tests",
                "-p",
                "test_product_contract_verifiers.py",
            ],
            working_directory: ".",
            required_paths: &[
                "tests/test_product_contract_verifiers.py",
                "scripts/verify_agent_activity_safe_narration.py",
                "scripts/verify_computer_use_master_policy.py",
                "scripts/verify_local_file_privacy_boundary.py",
            ],
        },
        remediation: "Keep every scripts/verify_*.py product-contract verifier in exactly one reviewed bucket. Repair an offline regression rather than moving it to the failing list, and promote a repaired verifier into the offline set in the same change so the failing list only shrinks.",
        failure_summary: "Product contract verifier inventory verification did not complete.",
    },
    VerificationCheck {
        id: "static-diagnostic-fixture-contract",
        suite: VerifySuite::Static,
        availability: CheckAvailability::Runnable,
        action: CheckAction::Run {
            program: "python3",
            args: &[
                "-m",
                "unittest",
                "discover",
                "-s",
                "scripts",
                "-p",
                "verify_whisply_diagnostics.py",
            ],
            working_directory: ".",
            required_paths: &[
                "scripts/verify_whisply_diagnostics.py",
                "scripts/whisply_diagnostics.py",
                "scripts/test_whisply_diagnostics.py",
                "docs/contextual-action-layer/diagnostic-scenarios.json",
                "WhisplyCore/Tests/WhisplyCoreTests/WhisplyDiagnosticScenarioTests.swift",
            ],
        },
        remediation: "Keep every deterministic diagnostic scenario non-authoritative, registry-backed, and bound to the reviewed Swift presentation dispatcher.",
        failure_summary: "Deterministic diagnostic fixture contract verification did not complete.",
    },
    VerificationCheck {
        id: "static-tui-direct-egress-contract",
        suite: VerifySuite::Static,
        availability: CheckAvailability::Runnable,
        action: CheckAction::Run {
            program: "python3",
            args: &[
                "-m",
                "unittest",
                "discover",
                "-s",
                "tests",
                "-p",
                "test_tui_direct_egress_contract.py",
            ],
            working_directory: ".",
            required_paths: &["tests/test_tui_direct_egress_contract.py"],
        },
        remediation: "Keep normal TUI launch on bundled/static tooltips until a reviewed release-owned announcement source is packaged.",
        failure_summary: "TUI direct-egress contract verification did not complete.",
    },
    VerificationCheck {
        id: "static-release-hygiene",
        suite: VerifySuite::Static,
        availability: CheckAvailability::Runnable,
        action: CheckAction::Run {
            program: "python3",
            args: &[
                "-m",
                "unittest",
                "discover",
                "-s",
                "tests",
                "-p",
                "test_verify_release_hygiene.py",
            ],
            working_directory: ".",
            required_paths: &[
                "scripts/verify_release_hygiene.py",
                "tests/test_verify_release_hygiene.py",
            ],
        },
        remediation: "Keep release-hygiene verification offline, explicit-input, redacted, and unable to inspect homes or live processes.",
        failure_summary: "Release hygiene verification did not complete.",
    },
    VerificationCheck {
        id: "static-ship-full-verification-gate",
        suite: VerifySuite::Static,
        availability: CheckAvailability::Runnable,
        action: CheckAction::Run {
            program: "python3",
            args: &[
                "-m",
                "unittest",
                "discover",
                "-s",
                "tests",
                "-p",
                "test_ship_full_verification_gate.py",
            ],
            working_directory: ".",
            required_paths: &["tests/test_ship_full_verification_gate.py"],
        },
        remediation: "Keep the release entry script gated on the fixed current-source CLI verification matrix before secrets or packaging.",
        failure_summary: "Ship full-verification gate did not complete.",
    },
    VerificationCheck {
        id: "rust-cli-full",
        suite: VerifySuite::Rust,
        availability: CheckAvailability::Runnable,
        action: CheckAction::Run {
            program: "cargo",
            args: &["test", "--offline", "-p", "whisply-cli", "--all-targets"],
            working_directory: "Runtime/whisply-codex/whisply-rs",
            required_paths: &[
                "Runtime/whisply-codex/whisply-rs/Cargo.lock",
                "Runtime/whisply-codex/whisply-rs/cli/tests/mcp_list.rs",
                "Runtime/whisply-codex/whisply-rs/cli/src/whisply_skills.rs",
            ],
        },
        remediation: "Restore the full fixed CLI test target and repair the failing public-command contract without weakening custom MCP, plugin, or skill support.",
        failure_summary: "Full CLI verification did not complete.",
    },
    VerificationCheck {
        id: "rust-app-server-helper-cli",
        suite: VerifySuite::Rust,
        availability: CheckAvailability::Runnable,
        action: CheckAction::Run {
            program: "cargo",
            args: &[
                "build",
                "--offline",
                "-p",
                "whisply-cli",
                "--bin",
                "whisply",
            ],
            working_directory: "Runtime/whisply-codex/whisply-rs",
            required_paths: &["Runtime/whisply-codex/whisply-rs/cli/Cargo.toml"],
        },
        remediation: "Restore the fixed Whisply CLI helper binary required by the complete Core and app-server protocol suites.",
        failure_summary: "App-server CLI helper build did not complete.",
    },
    VerificationCheck {
        id: "rust-app-server-helper-code-mode",
        suite: VerifySuite::Rust,
        availability: CheckAvailability::Runnable,
        action: CheckAction::Run {
            program: "cargo",
            args: &[
                "build",
                "--offline",
                "-p",
                "whisply-code-mode-host",
                "--bin",
                "codex-code-mode-host",
            ],
            working_directory: "Runtime/whisply-codex/whisply-rs",
            required_paths: &["Runtime/whisply-codex/whisply-rs/code-mode-host/Cargo.toml"],
        },
        remediation: "Restore the fixed local code-mode helper binary required by the complete Core and app-server protocol suites.",
        failure_summary: "Code-mode helper build did not complete.",
    },
    VerificationCheck {
        id: "rust-app-server-helper-stdio",
        suite: VerifySuite::Rust,
        availability: CheckAvailability::Runnable,
        action: CheckAction::Run {
            program: "cargo",
            args: &[
                "build",
                "--offline",
                "-p",
                "whisply-rmcp-client",
                "--bin",
                "test_stdio_server",
                "--bin",
                "test_streamable_http_server",
            ],
            working_directory: "Runtime/whisply-codex/whisply-rs",
            required_paths: &["Runtime/whisply-codex/whisply-rs/rmcp-client/Cargo.toml"],
        },
        remediation: "Restore the fixed local stdio MCP helper binary required by the complete Core and app-server protocol suites.",
        failure_summary: "App-server stdio helper build did not complete.",
    },
    VerificationCheck {
        id: "rust-core-full",
        suite: VerifySuite::Rust,
        availability: CheckAvailability::Runnable,
        action: CheckAction::Run {
            program: "cargo",
            // This complete integration suite owns shared process, filesystem,
            // and fixture state. Run it deterministically, like the full v2
            // app-server suite, so parallel test scheduling cannot turn a
            // release check into a stack-overflow abort.
            args: &[
                "test",
                "--offline",
                "-p",
                "whisply-core",
                "--all-targets",
                "--",
                "--test-threads=1",
            ],
            working_directory: "Runtime/whisply-codex/whisply-rs",
            required_paths: &[
                "Runtime/whisply-codex/whisply-rs/Cargo.lock",
                "Runtime/whisply-codex/whisply-rs/core/src/config/config_tests.rs",
                "Runtime/whisply-codex/whisply-rs/core/src/realtime_conversation_tests.rs",
                "Runtime/whisply-codex/whisply-rs/core/src/tools/spec_plan_tests.rs",
                "Runtime/whisply-codex/whisply-rs/core/tests/suite/tool_harness.rs",
                "Runtime/whisply-codex/whisply-rs/core/tests/suite/hooks.rs",
                "Runtime/whisply-codex/whisply-rs/core/tests/suite/hooks_mcp.rs",
                "Runtime/whisply-codex/whisply-rs/core/tests/suite/rmcp_client.rs",
                "Runtime/whisply-codex/whisply-rs/core/tests/suite/plugins.rs",
                "Runtime/whisply-codex/whisply-rs/core/tests/suite/skills.rs",
                "Runtime/whisply-codex/whisply-rs/core/tests/suite/skills_extension.rs",
                "Runtime/whisply-codex/whisply-rs/core/tests/suite/multi_agent_resume.rs",
                "Runtime/whisply-codex/whisply-rs/core/tests/suite/agent_execution.rs",
                "Runtime/whisply-codex/whisply-rs/core/tests/suite/apply_patch_cli.rs",
                "Runtime/whisply-codex/whisply-rs/core/tests/suite/unified_exec.rs",
                "Runtime/whisply-codex/whisply-rs/core/tests/suite/view_image.rs",
            ],
        },
        remediation: "Repair the complete core behavior and BrokerOnly contracts; do not restore direct hosted-provider authority merely to satisfy a legacy fixture.",
        failure_summary: "Full core verification did not complete.",
    },
    VerificationCheck {
        id: "rust-config-full",
        suite: VerifySuite::Rust,
        availability: CheckAvailability::Runnable,
        action: CheckAction::Run {
            program: "cargo",
            args: &["test", "--offline", "-p", "whisply-config", "--lib"],
            working_directory: "Runtime/whisply-codex/whisply-rs",
            required_paths: &[
                "Runtime/whisply-codex/whisply-rs/Cargo.lock",
                "Runtime/whisply-codex/whisply-rs/config/src/loader/mod.rs",
            ],
        },
        remediation: "Repair configuration and trusted-project behavior while preserving user-local Codex, Claude, and Cursor compatibility roots.",
        failure_summary: "Full configuration verification did not complete.",
    },
    VerificationCheck {
        id: "rust-goal-extension-full",
        suite: VerifySuite::Rust,
        availability: CheckAvailability::Runnable,
        action: CheckAction::Run {
            program: "cargo",
            args: &[
                "test",
                "--offline",
                "-p",
                "whisply-goal-extension",
                "--all-targets",
            ],
            working_directory: "Runtime/whisply-codex/whisply-rs",
            required_paths: &[
                "Runtime/whisply-codex/whisply-rs/Cargo.lock",
                "Runtime/whisply-codex/whisply-rs/ext/goal/src/tool.rs",
                "Runtime/whisply-codex/whisply-rs/ext/goal/tests/goal_extension_backend.rs",
            ],
        },
        remediation: "Repair user-controlled goal lifecycle, accounting, and tool behavior without removing goal or plan-update capability.",
        failure_summary: "Full goal-extension verification did not complete.",
    },
    VerificationCheck {
        id: "rust-model-provider-full",
        suite: VerifySuite::Rust,
        availability: CheckAvailability::Runnable,
        action: CheckAction::Run {
            program: "cargo",
            args: &["test", "--offline", "-p", "whisply-model-provider", "--lib"],
            working_directory: "Runtime/whisply-codex/whisply-rs",
            required_paths: &[
                "Runtime/whisply-codex/whisply-rs/Cargo.lock",
                "Runtime/whisply-codex/whisply-rs/model-provider/src/whisply.rs",
            ],
        },
        remediation: "Repair the managed model-provider projection without permitting a direct provider endpoint or credentials in user configuration.",
        failure_summary: "Full model-provider verification did not complete.",
    },
    VerificationCheck {
        id: "rust-native-broker-full",
        suite: VerifySuite::Rust,
        availability: CheckAvailability::Runnable,
        action: CheckAction::Run {
            program: "cargo",
            args: &["test", "--offline", "-p", "codex-whisply", "--lib"],
            working_directory: "Runtime/whisply-codex/whisply-rs",
            required_paths: &[
                "Runtime/whisply-codex/whisply-rs/Cargo.lock",
                "Runtime/whisply-codex/whisply-rs/whisply-runtime/src/broker.rs",
            ],
        },
        remediation: "Repair the typed native-broker wire contract, descriptor lifetime checks, and received-FD close-on-exec handling.",
        failure_summary: "Full native-broker verification did not complete.",
    },
    VerificationCheck {
        id: "rust-rmcp-client-full",
        suite: VerifySuite::Rust,
        availability: CheckAvailability::Runnable,
        action: CheckAction::Run {
            program: "cargo",
            args: &[
                "test",
                "--offline",
                "-p",
                "whisply-rmcp-client",
                "--all-targets",
            ],
            working_directory: "Runtime/whisply-codex/whisply-rs",
            required_paths: &[
                "Runtime/whisply-codex/whisply-rs/Cargo.lock",
                "Runtime/whisply-codex/whisply-rs/rmcp-client/tests/streamable_http_oauth_startup.rs",
                "Runtime/whisply-codex/whisply-rs/rmcp-client/tests/resources.rs",
            ],
        },
        remediation: "Repair custom MCP stdio, HTTP, OAuth, resource, elicitation, and recovery behavior using only test-owned loopback servers.",
        failure_summary: "Full custom MCP client verification did not complete.",
    },
    VerificationCheck {
        id: "rust-codex-mcp-full",
        suite: VerifySuite::Rust,
        availability: CheckAvailability::Runnable,
        action: CheckAction::Run {
            program: "cargo",
            args: &["test", "--offline", "-p", "whisply-mcp", "--lib"],
            working_directory: "Runtime/whisply-codex/whisply-rs",
            required_paths: &[
                "Runtime/whisply-codex/whisply-rs/Cargo.lock",
                "Runtime/whisply-codex/whisply-rs/codex-mcp/src/mcp/mod_tests.rs",
            ],
        },
        remediation: "Repair core MCP protocol behavior without removing standard user-configured transports or authentication fields.",
        failure_summary: "Full core MCP verification did not complete.",
    },
    VerificationCheck {
        id: "rust-mcp-extension-full",
        suite: VerifySuite::Rust,
        availability: CheckAvailability::Runnable,
        action: CheckAction::Run {
            program: "cargo",
            args: &[
                "test",
                "--offline",
                "-p",
                "whisply-mcp-extension",
                "--all-targets",
            ],
            working_directory: "Runtime/whisply-codex/whisply-rs",
            required_paths: &[
                "Runtime/whisply-codex/whisply-rs/Cargo.lock",
                "Runtime/whisply-codex/whisply-rs/ext/mcp/tests/executor_plugin_mcp.rs",
            ],
        },
        remediation: "Repair executor and plugin MCP extensions while keeping custom MCP discovery and execution available.",
        failure_summary: "Full MCP extension verification did not complete.",
    },
    VerificationCheck {
        id: "rust-mcp-server-full",
        suite: VerifySuite::Rust,
        availability: CheckAvailability::Runnable,
        action: CheckAction::Run {
            program: "cargo",
            args: &[
                "test",
                "--offline",
                "-p",
                "whisply-mcp-server",
                "--all-targets",
                "--",
                "--test-threads=1",
            ],
            working_directory: "Runtime/whisply-codex/whisply-rs",
            required_paths: &[
                "Runtime/whisply-codex/whisply-rs/Cargo.lock",
                "Runtime/whisply-codex/whisply-rs/mcp-server/tests/suite/codex_tool.rs",
                "Runtime/whisply-codex/whisply-rs/app-server/tests/common/managed_whisply_gateway.rs",
            ],
        },
        remediation: "Keep MCP model turns on the native managed-gateway fixture and preserve the Whisply-branded handshake without direct provider configuration.",
        failure_summary: "Full MCP server verification did not complete.",
    },
    VerificationCheck {
        id: "rust-plugin-core-full",
        suite: VerifySuite::Rust,
        availability: CheckAvailability::Runnable,
        action: CheckAction::Run {
            program: "cargo",
            args: &["test", "--offline", "-p", "whisply-core-plugins", "--lib"],
            working_directory: "Runtime/whisply-codex/whisply-rs",
            required_paths: &[
                "Runtime/whisply-codex/whisply-rs/Cargo.lock",
                "Runtime/whisply-codex/whisply-rs/core-plugins/src/store_tests.rs",
                "Runtime/whisply-codex/whisply-rs/core-plugins/src/loader_tests.rs",
            ],
        },
        remediation: "Keep local plugin installation, hooks, skills, custom MCP manifests, and user environment references working without reviving retired host-owned Apps authority.",
        failure_summary: "Full core plugin verification did not complete.",
    },
    VerificationCheck {
        id: "rust-plugin-manifest-full",
        suite: VerifySuite::Rust,
        availability: CheckAvailability::Runnable,
        action: CheckAction::Run {
            program: "cargo",
            args: &["test", "--offline", "-p", "whisply-plugin", "--lib"],
            working_directory: "Runtime/whisply-codex/whisply-rs",
            required_paths: &[
                "Runtime/whisply-codex/whisply-rs/Cargo.lock",
                "Runtime/whisply-codex/whisply-rs/plugin/Cargo.toml",
            ],
        },
        remediation: "Repair Codex plugin manifest parsing and validation without narrowing supported user plugin metadata.",
        failure_summary: "Full plugin manifest verification did not complete.",
    },
    VerificationCheck {
        id: "rust-skills-full",
        suite: VerifySuite::Rust,
        availability: CheckAvailability::Runnable,
        action: CheckAction::Run {
            program: "cargo",
            args: &["test", "--offline", "-p", "whisply-skills", "--lib"],
            working_directory: "Runtime/whisply-codex/whisply-rs",
            required_paths: &[
                "Runtime/whisply-codex/whisply-rs/Cargo.lock",
                "Runtime/whisply-codex/whisply-rs/skills/src/selection_tests.rs",
            ],
        },
        remediation: "Repair built-in and user skill parsing, selection, invocation, and policy behavior without removing compatible skill roots.",
        failure_summary: "Full skills verification did not complete.",
    },
    VerificationCheck {
        id: "rust-core-skills-full",
        suite: VerifySuite::Rust,
        availability: CheckAvailability::Runnable,
        action: CheckAction::Run {
            program: "cargo",
            args: &[
                "test",
                "--offline",
                "-p",
                "whisply-core-skills",
                "--all-targets",
            ],
            working_directory: "Runtime/whisply-codex/whisply-rs",
            required_paths: &[
                "Runtime/whisply-codex/whisply-rs/Cargo.lock",
                "Runtime/whisply-codex/whisply-rs/core-skills/src/loader_tests.rs",
                "Runtime/whisply-codex/whisply-rs/core-skills/tests/environment_loader.rs",
            ],
        },
        remediation: "Repair core skill loading, instruction precedence, environment discovery, and package safety without removing compatible local skill packages.",
        failure_summary: "Full core skills verification did not complete.",
    },
    VerificationCheck {
        id: "rust-skills-extension-full",
        suite: VerifySuite::Rust,
        availability: CheckAvailability::Runnable,
        action: CheckAction::Run {
            program: "cargo",
            args: &[
                "test",
                "--offline",
                "-p",
                "whisply-skills-extension",
                "--all-targets",
            ],
            working_directory: "Runtime/whisply-codex/whisply-rs",
            required_paths: &[
                "Runtime/whisply-codex/whisply-rs/Cargo.lock",
                "Runtime/whisply-codex/whisply-rs/ext/skills/src/host_roots_tests.rs",
                "Runtime/whisply-codex/whisply-rs/ext/skills/tests/skills_extension.rs",
            ],
        },
        remediation: "Repair trusted project, user, and cross-tool skill discovery while preserving .agents, Claude, Cursor, and Whisply compatibility roots.",
        failure_summary: "Full skills extension verification did not complete.",
    },
    VerificationCheck {
        id: "rust-external-agent-migration-full",
        suite: VerifySuite::Rust,
        availability: CheckAvailability::Runnable,
        action: CheckAction::Run {
            program: "cargo",
            args: &[
                "test",
                "--offline",
                "-p",
                "whisply-external-agent-migration",
            ],
            working_directory: "Runtime/whisply-codex/whisply-rs",
            required_paths: &[
                "Runtime/whisply-codex/whisply-rs/Cargo.lock",
                "Runtime/whisply-codex/whisply-rs/external-agent-migration/src/mcp.rs",
                "Runtime/whisply-codex/whisply-rs/external-agent-migration/src/service_tests/plugins/marketplaces.rs",
            ],
        },
        remediation: "Repair Claude, Cursor, and Codex import behavior while preserving selected skills, local plugins, and custom MCP configuration.",
        failure_summary: "Full external-agent migration verification did not complete.",
    },
    VerificationCheck {
        id: "rust-exec-server-full",
        suite: VerifySuite::Rust,
        availability: CheckAvailability::Runnable,
        action: CheckAction::Run {
            program: "cargo",
            args: &[
                "test",
                "--offline",
                "-p",
                "whisply-exec-server",
                "--all-targets",
                "--",
                "--test-threads=1",
            ],
            working_directory: "Runtime/whisply-codex/whisply-rs",
            required_paths: &[
                "Runtime/whisply-codex/whisply-rs/Cargo.lock",
                "Runtime/whisply-codex/whisply-rs/exec-server/tests/websocket.rs",
                "Runtime/whisply-codex/whisply-rs/exec-server/tests/http_request.rs",
            ],
        },
        remediation: "Repair the local execution server, filesystem/process, capability, HTTP, and loopback WebSocket contracts without weakening listener ownership rules.",
        failure_summary: "Full execution-server verification did not complete.",
    },
    VerificationCheck {
        id: "rust-exec-full",
        suite: VerifySuite::Rust,
        availability: CheckAvailability::Runnable,
        action: CheckAction::Run {
            program: "cargo",
            args: &["test", "--offline", "-p", "whisply-exec", "--all-targets"],
            working_directory: "Runtime/whisply-codex/whisply-rs",
            required_paths: &[
                "Runtime/whisply-codex/whisply-rs/Cargo.lock",
                "Runtime/whisply-codex/whisply-rs/exec/tests/all.rs",
            ],
        },
        remediation: "Repair CLI execution flows, local project isolation, and configured executor compatibility without enabling direct hosted-provider credentials.",
        failure_summary: "Full execution CLI verification did not complete.",
    },
    VerificationCheck {
        id: "rust-app-server-transport-full",
        suite: VerifySuite::Rust,
        availability: CheckAvailability::Runnable,
        action: CheckAction::Run {
            program: "cargo",
            args: &[
                "test",
                "--offline",
                "-p",
                "whisply-app-server-transport",
                "--lib",
            ],
            working_directory: "Runtime/whisply-codex/whisply-rs",
            required_paths: &[
                "Runtime/whisply-codex/whisply-rs/Cargo.lock",
                "Runtime/whisply-codex/whisply-rs/app-server-transport/src/lib.rs",
            ],
        },
        remediation: "Repair the local app-server transport and explicit listener security contract before validating higher-level UI protocol behavior.",
        failure_summary: "Full app-server transport verification did not complete.",
    },
    VerificationCheck {
        id: "rust-app-server-v2-full",
        suite: VerifySuite::Rust,
        availability: CheckAvailability::Runnable,
        action: CheckAction::Run {
            program: "cargo",
            args: &[
                "test",
                "--offline",
                "-p",
                "whisply-app-server",
                "--all-targets",
                "--",
                "--test-threads=1",
            ],
            working_directory: "Runtime/whisply-codex/whisply-rs",
            required_paths: &[
                "Runtime/whisply-codex/whisply-rs/Cargo.lock",
                "Runtime/whisply-codex/whisply-rs/app-server/tests/suite/v2/mod.rs",
                "Runtime/whisply-codex/whisply-rs/app-server/tests/suite/v2/thread_resume.rs",
                "Runtime/whisply-codex/whisply-rs/app-server/tests/suite/v2/view_image.rs",
                "Runtime/whisply-codex/whisply-rs/app-server/tests/common/managed_whisply_gateway.rs",
            ],
        },
        remediation: "Repair the complete app-server v2 protocol suite: MCP, plugins, skills, threads, images, execution, and managed account boundaries must all pass together.",
        failure_summary: "Full app-server v2 verification did not complete.",
    },
    VerificationCheck {
        id: "rust-tui-full",
        suite: VerifySuite::Rust,
        availability: CheckAvailability::Runnable,
        action: CheckAction::Run {
            program: "cargo",
            args: &["test", "--offline", "-p", "whisply-tui", "--all-targets"],
            working_directory: "Runtime/whisply-codex/whisply-rs",
            required_paths: &[
                "Runtime/whisply-codex/whisply-rs/Cargo.lock",
                "Runtime/whisply-codex/whisply-rs/tui/tests/all.rs",
                "Runtime/whisply-codex/whisply-rs/tui/tests/suite/vt100_live_commit.rs",
            ],
        },
        remediation: "Repair terminal rendering, keyboard, MCP, plugin, skill, session, and app-server integration behavior without relying on an interactive manual run.",
        failure_summary: "Full terminal UI verification did not complete.",
    },
    VerificationCheck {
        id: "mac-managed-skills-plugins-contracts",
        suite: VerifySuite::Mac,
        availability: CheckAvailability::Runnable,
        action: CheckAction::Run {
            program: "swift",
            args: &[
                "test",
                // SwiftPM otherwise tries to nest its own sandbox inside the
                // fixed outer sandbox-exec policy below. The outer policy
                // remains the authority boundary and denies every external
                // network destination.
                "--disable-sandbox",
                "--skip-update",
                "--quiet",
                "--package-path",
                "WhisplyCore",
                "--filter",
                "ContextualRuntimeContractTests|SkillPluginServiceEndpointTests|SkillCreatorTeachingRecoveryTests|SkillSemanticGenerationTests|SkillsPluginsSettingsVisualContractTests|BuiltinSkillPluginPolicyTests|SkillInvocationRoutingTests|PluginTryNowDeepLinkTests|LocalArtifactWorkflowCatalogTests",
            ],
            working_directory: ".",
            required_paths: &[
                "WhisplyCore/Package.resolved",
                "WhisplyCore/.build/checkouts/Sparkle",
                "WhisplyCore/.build/checkouts/supabase-swift",
                "WhisplyCore/Tests/WhisplyCoreTests/ContextualRuntimeContractTests.swift",
                "WhisplyCore/Tests/WhisplyCoreTests/SkillPluginServiceEndpointTests.swift",
                "WhisplyCore/Tests/WhisplyCoreTests/SkillCreatorTeachingRecoveryTests.swift",
                "WhisplyCore/Tests/WhisplyCoreTests/SkillSemanticGenerationTests.swift",
                "WhisplyCore/Tests/WhisplyCoreTests/SkillsPluginsSettingsVisualContractTests.swift",
                "WhisplyCore/Tests/WhisplyCoreTests/BuiltinSkillPluginPolicyTests.swift",
                "WhisplyCore/Tests/WhisplyCoreTests/SkillInvocationRoutingTests.swift",
                "WhisplyCore/Tests/WhisplyCoreTests/PluginTryNowDeepLinkTests.swift",
                "WhisplyCore/Tests/WhisplyCoreTests/LocalArtifactWorkflowCatalogTests.swift",
            ],
        },
        remediation: "Repair managed skill and plugin account binding, versioning, creation, protected-tool, and UI routing contracts without removing compatible upstream skill or plugin support.",
        failure_summary: "Managed skill or plugin contract verification did not complete.",
    },
    VerificationCheck {
        id: "mac-memory-audio-transcript-contracts",
        suite: VerifySuite::Mac,
        availability: CheckAvailability::Runnable,
        action: CheckAction::Run {
            program: "swift",
            args: &[
                "test",
                // SwiftPM otherwise tries to nest its own sandbox inside the
                // fixed outer sandbox-exec policy below. The outer policy
                // remains the authority boundary and denies every external
                // network destination.
                "--disable-sandbox",
                "--skip-update",
                "--quiet",
                "--package-path",
                "WhisplyCore",
                "--filter",
                "ContextualMemoryCommandSafetyTests|ContextualAccountServiceTests|ConversationContextCompactionBoundaryTests|ConversationContextCompactionTests|ConversationResponseContextTests|RealtimeTranscriptionContractTests|TranscriptPrivacyContractTests|TranscriptPrivacyOverrideStoreTests|TranscriptStopLifecycleTests|TranscriptTaskOpportunityTests|TranscriptionArtifactFilterTests|PreferencesProductContractTests|SessionWorkspaceContractTests|ChatHistoryMutationBarrierTests",
            ],
            working_directory: ".",
            required_paths: &[
                "WhisplyCore/Package.resolved",
                "WhisplyCore/.build/checkouts/Sparkle",
                "WhisplyCore/.build/checkouts/supabase-swift",
                "WhisplyCore/Tests/WhisplyCoreTests/ContextualMemoryCommandSafetyTests.swift",
                "WhisplyCore/Tests/WhisplyCoreTests/ContextualAccountServiceTests.swift",
                "WhisplyCore/Tests/WhisplyCoreTests/ConversationContextCompactionBoundaryTests.swift",
                "WhisplyCore/Tests/WhisplyCoreTests/ConversationContextCompactionTests.swift",
                "WhisplyCore/Tests/WhisplyCoreTests/ConversationResponseContextTests.swift",
                "WhisplyCore/Tests/WhisplyCoreTests/RealtimeTranscriptionContractTests.swift",
                "WhisplyCore/Tests/WhisplyCoreTests/TranscriptPrivacyContractTests.swift",
                "WhisplyCore/Tests/WhisplyCoreTests/TranscriptPrivacyOverrideStoreTests.swift",
                "WhisplyCore/Tests/WhisplyCoreTests/TranscriptStopLifecycleTests.swift",
                "WhisplyCore/Tests/WhisplyCoreTests/TranscriptTaskOpportunityTests.swift",
                "WhisplyCore/Tests/WhisplyCoreTests/TranscriptionArtifactFilterTests.swift",
                "WhisplyCore/Tests/WhisplyCoreTests/PreferencesProductContractTests.swift",
                "WhisplyCore/Tests/WhisplyCoreTests/SessionWorkspaceTests.swift",
                "WhisplyCore/Tests/WhisplyCoreTests/ChatHistoryMutationBarrierTests.swift",
            ],
        },
        remediation: "Repair the fixed offline memory, compaction, audio/transcription, and transcript privacy contracts without adding an unreviewed capture or provider path.",
        failure_summary: "Memory, audio, or transcript contract verification did not complete.",
    },
    VerificationCheck {
        id: "mac-swift-package-full",
        suite: VerifySuite::Mac,
        availability: CheckAvailability::Runnable,
        action: CheckAction::Run {
            program: "swift",
            args: &[
                "test",
                // SwiftPM otherwise tries to nest its own sandbox inside the
                // fixed outer sandbox-exec policy below. The outer policy
                // remains the authority boundary and denies every external
                // network destination.
                "--disable-sandbox",
                "--skip-update",
                "--package-path",
                "WhisplyCore",
            ],
            working_directory: ".",
            required_paths: &[
                "WhisplyCore/Package.resolved",
                "WhisplyCore/.build/checkouts/Sparkle",
                "WhisplyCore/.build/checkouts/supabase-swift",
                "WhisplyCore/Tests/WhisplyCoreTests/BrowserMCPCompatibilityTests.swift",
                "WhisplyCore/Tests/WhisplyCoreTests/BrowserComputerUseTests.swift",
                "WhisplyCore/Tests/WhisplyCoreTests/BrowserLogicalConnectionTests.swift",
                "WhisplyCore/Tests/WhisplyCoreTests/BrowserConnectionPresentationTests.swift",
                "WhisplyCore/Tests/WhisplyCoreTests/ChromePluginIntegrationStatusTests.swift",
                "WhisplyCore/Tests/WhisplyCoreTests/DirectBrowserTaskCoordinatorTests.swift",
                "WhisplyCore/Tests/WhisplyCoreTests/ComputerUsePolicyTests.swift",
                "WhisplyCore/Tests/WhisplyCoreTests/ComputerUseSelectedModelRoutingTests.swift",
                "WhisplyCore/Tests/WhisplyCoreTests/ComputerUseHelperActionRoutingTests.swift",
                "WhisplyCore/Tests/WhisplyCoreTests/ConnectorRuntimeContractTests.swift",
                "WhisplyCore/Tests/WhisplyCoreTests/DirectConnectorChatToolLoopTests.swift",
                "WhisplyCore/Tests/WhisplyCoreTests/LocalFileAccessServiceTests.swift",
                "WhisplyCore/Tests/WhisplyRuntimeIntegrationTests/WhisplyRuntimeIntegrationTests.swift",
                "WhisplyCore/Tests/WhisplyRuntimeIntegrationTests/WhisplyHostControlsRelayTests.swift",
            ],
        },
        remediation: "Use the normal reviewed Swift package bootstrap to restore local checkouts, then fix the full macOS core, visual-contract, runtime-integration, broker, and Host Controls test targets.",
        failure_summary: "Full macOS Swift package verification did not complete.",
    },
    VerificationCheck {
        id: "mac-diagnostic-fixture-scenarios-full",
        suite: VerifySuite::Mac,
        availability: CheckAvailability::Runnable,
        action: CheckAction::Run {
            program: "python3",
            args: &[
                "-m",
                "unittest",
                "discover",
                "-s",
                "scripts",
                "-p",
                "test_whisply_diagnostics.py",
            ],
            working_directory: ".",
            required_paths: &[
                "scripts/test_whisply_diagnostics.py",
                "scripts/whisply_diagnostics.py",
                "docs/contextual-action-layer/diagnostic-scenarios.json",
                "WhisplyCore/Tests/WhisplyCoreTests/WhisplyDiagnosticScenarioTests.swift",
                "WhisplyCore/.build/checkouts/Sparkle",
                "WhisplyCore/.build/checkouts/supabase-swift",
            ],
        },
        remediation: "Repair the fixed zero-authority Swift presentation-fixture wave for chat, activity, Browser/Chrome, Computer Use, connectors, settings, Workspace, account, Usage, and typed error states.",
        failure_summary: "Full deterministic macOS presentation-fixture verification did not complete.",
    },
    VerificationCheck {
        id: "website-managed-skills-plugins-contracts",
        suite: VerifySuite::Website,
        availability: CheckAvailability::Runnable,
        action: CheckAction::Run {
            program: "npm",
            args: &[
                "--offline",
                "test",
                "--",
                "tests/unit/skills-plugins-runtime.test.ts",
                "tests/unit/skills-plugins-route.test.ts",
                "tests/unit/skills-plugin-repository-identifiers.test.ts",
                "tests/unit/skill-claim-replay-contract.test.ts",
                "tests/unit/skill-transform-funding.test.ts",
            ],
            working_directory: ".",
            required_paths: &[
                "package.json",
                "package-lock.json",
                "node_modules/.bin/vitest",
                "src/routes/v1/skills-plugins.ts",
                "src/lib/skills-plugins/service.ts",
                "tests/unit/skills-plugins-runtime.test.ts",
                "tests/unit/skills-plugins-route.test.ts",
                "tests/unit/skills-plugin-repository-identifiers.test.ts",
                "tests/unit/skill-claim-replay-contract.test.ts",
                "tests/unit/skill-transform-funding.test.ts",
            ],
        },
        remediation: "Restore the reviewed local Website Skills/Plugins dependencies, then repair the fixed account-bound service, route, repository, replay, and funding contracts.",
        failure_summary: "Managed Website skill or plugin contract verification did not complete.",
    },
    VerificationCheck {
        id: "website-unit-full",
        suite: VerifySuite::Website,
        availability: CheckAvailability::Runnable,
        action: CheckAction::Run {
            program: "npm",
            args: &["--offline", "test"],
            working_directory: "workers/proxy",
            required_paths: &[
                "workers/proxy/package.json",
                "workers/proxy/node_modules/.bin/vitest",
            ],
        },
        remediation: "Restore the reviewed local website dependencies without credentials, then fix the complete Worker and direct-runtime unit suite.",
        failure_summary: "Full website unit verification did not complete.",
    },
    VerificationCheck {
        id: "website-typecheck",
        suite: VerifySuite::Website,
        availability: CheckAvailability::Runnable,
        action: CheckAction::Run {
            program: "npm",
            args: &["--offline", "run", "typecheck"],
            working_directory: "workers/proxy",
            required_paths: &[
                "workers/proxy/package.json",
                "workers/proxy/node_modules/.bin/tsc",
            ],
        },
        remediation: "Restore the reviewed local website dependencies without credentials, then repair the TypeScript contract before direct-runtime tests.",
        failure_summary: "Website typecheck verification did not complete.",
    },
    VerificationCheck {
        id: "integration-and-database-preflight",
        suite: VerifySuite::Integration,
        availability: CheckAvailability::ExplicitSelectionOnly,
        action: CheckAction::Blocked,
        remediation: "Integration, database, and live release preflights require a separately reviewed isolated environment. debug verify never runs release shipping or a live preflight.",
        failure_summary: "Live integration and release preflights are intentionally blocked.",
    },
];

const CHILD_OUTPUT_WITHHELD: &str = "withheld";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum VerifyFailureKind {
    ManifestBlocked,
    LocalPrerequisiteUnavailable,
    TrustedExecutableUnavailable,
    NetworkSandboxUnavailable,
    IsolatedProductHomeUnavailable,
    ChildExited,
    ChildNoExitStatus,
    ChildLaunchFailed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct VerifyFailure {
    kind: VerifyFailureKind,
    summary: &'static str,
    exit_code: Option<i32>,
    child_output: &'static str,
}

#[derive(Debug, Serialize)]
struct CheckResult {
    id: &'static str,
    suite: VerifySuite,
    status: CheckStatus,
    duration_ms: u64,
    remediation: &'static str,
    failure: Option<VerifyFailure>,
}

#[derive(Serialize)]
struct VerifyReport<'a> {
    manifest_version: u16,
    dry_run: bool,
    selected_features: &'a [VerifyFeature],
    user_configured_feature_coverage: &'a [UserConfiguredFeatureCoverage],
    results: &'a [CheckResult],
}

#[derive(Serialize)]
struct ManifestReport<'a> {
    manifest_version: u16,
    selected_features: &'a [VerifyFeature],
    checks: &'a [ManifestCheck<'a>],
    user_configured_feature_coverage: &'a [UserConfiguredFeatureCoverage],
}

#[derive(Serialize)]
struct ManifestCheck<'a> {
    id: &'a str,
    suite: VerifySuite,
    availability: CheckAvailability,
    remediation: &'a str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ReleaseTestStatus {
    DryRun,
    Blocked,
}

#[derive(Debug, Serialize)]
struct ReleaseTestResult {
    group: ReleaseTestGroup,
    status: ReleaseTestStatus,
    detail: &'static str,
}

#[derive(Serialize)]
struct ReleaseTestManifestReport<'a> {
    manifest_version: u16,
    groups: &'a [ReleaseTestGroupDefinition],
    prohibited_actions: &'a [&'a str],
    user_configured_feature_coverage: &'a [UserConfiguredFeatureCoverage],
}

#[derive(Serialize)]
struct ReleaseTestReport<'a> {
    manifest_version: u16,
    selected_group: ReleaseTestGroup,
    dry_run: bool,
    prohibited_actions: &'a [&'a str],
    user_configured_feature_coverage: &'a [UserConfiguredFeatureCoverage],
    results: &'a [ReleaseTestResult],
}

pub(crate) fn run_test(command: TestCommand) -> anyhow::Result<()> {
    match command.subcommand {
        TestSubcommand::Release(command) => run_release_test(command),
    }
}

/// This command is deliberately a fixed plan rather than an arbitrary command
/// runner. No child process is started here: an unavailable installed fixture
/// host is reported as blocked instead of being substituted with a build,
/// release, provider, or confirmation-capable path.
fn run_release_test(command: ReleaseTestCommand) -> anyhow::Result<()> {
    find_whisply_source_root()?;
    if command.list {
        return print_release_test_manifest(command.json);
    }

    let selected_groups: Vec<_> = RELEASE_TEST_GROUPS
        .iter()
        .filter(|definition| release_group_selects(command.group, definition.group))
        .collect();
    debug_assert!(!selected_groups.is_empty());

    let (status, detail) = if command.dry_run {
        (
            ReleaseTestStatus::DryRun,
            "Fixed release-test group was not executed because --dry-run was supplied.",
        )
    } else {
        (
            ReleaseTestStatus::Blocked,
            "No verified installed fixture host is available; refusing to simulate release evidence.",
        )
    };
    let results: Vec<_> = selected_groups
        .iter()
        .map(|definition| ReleaseTestResult {
            group: definition.group,
            status,
            detail,
        })
        .collect();

    print_release_test_results(command.group, command.dry_run, command.json, &results)?;
    if !command.dry_run {
        anyhow::bail!(
            "Whisply release test plan is blocked: no verified installed fixture host is available"
        );
    }
    Ok(())
}

fn release_group_selects(selected: ReleaseTestGroup, candidate: ReleaseTestGroup) -> bool {
    match selected {
        ReleaseTestGroup::Full => candidate != ReleaseTestGroup::Full,
        _ => selected == candidate,
    }
}

fn print_user_configured_feature_coverage() {
    println!("User-configured feature coverage:");
    for coverage in USER_CONFIGURED_FEATURE_COVERAGE {
        println!(
            "{}\t{:?}\t{}",
            coverage.feature,
            coverage.release_group,
            coverage.automated_check_ids.join(", "),
        );
        println!("  Local fixtures: {}", coverage.fixture_paths.join(", "));
        if let Some(evidence) = coverage.later_integration_evidence {
            println!("  Later integration/UI evidence: {evidence}");
        }
    }
}

fn print_release_test_manifest(json: bool) -> anyhow::Result<()> {
    let report = ReleaseTestManifestReport {
        manifest_version: RELEASE_TEST_MANIFEST_VERSION,
        groups: RELEASE_TEST_GROUPS,
        prohibited_actions: RELEASE_TEST_PROHIBITED_ACTIONS,
        user_configured_feature_coverage: USER_CONFIGURED_FEATURE_COVERAGE,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("Whisply test release manifest v{RELEASE_TEST_MANIFEST_VERSION} (zero authority)");
        for definition in RELEASE_TEST_GROUPS {
            println!("{:?}\t{}", definition.group, definition.description);
        }
        print_user_configured_feature_coverage();
        println!("Prohibited: {}", RELEASE_TEST_PROHIBITED_ACTIONS.join(", "));
    }
    Ok(())
}

fn print_release_test_results(
    selected_group: ReleaseTestGroup,
    dry_run: bool,
    json: bool,
    results: &[ReleaseTestResult],
) -> anyhow::Result<()> {
    let report = ReleaseTestReport {
        manifest_version: RELEASE_TEST_MANIFEST_VERSION,
        selected_group,
        dry_run,
        prohibited_actions: RELEASE_TEST_PROHIBITED_ACTIONS,
        user_configured_feature_coverage: USER_CONFIGURED_FEATURE_COVERAGE,
        results,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "Whisply test release manifest v{RELEASE_TEST_MANIFEST_VERSION} ({selected_group:?})"
        );
        for result in results {
            println!("{:?}\t{:?}\t{}", result.group, result.status, result.detail);
        }
    }
    Ok(())
}

pub(crate) fn run(command: VerifyCommand) -> anyhow::Result<()> {
    let source_root = find_whisply_source_root()?;
    if command.list {
        return print_manifest(command.json, &command.features);
    }

    let selected_checks = select_verify_checks(command.suite, &command.features);
    if selected_checks.is_empty() {
        let selected_features = command
            .features
            .iter()
            .map(|feature| feature.coverage_id())
            .collect::<Vec<_>>()
            .join(", ");
        anyhow::bail!(
            "No fixed verification checks match --suite {:?} and --feature {selected_features}. Choose --suite all or a compatible fixed suite.",
            command.suite
        );
    }

    let mut results = Vec::with_capacity(selected_checks.len());
    for check in selected_checks {
        let result = run_check(&source_root, check, command.dry_run);
        results.push(result);
    }

    print_results(command.dry_run, command.json, &command.features, &results)?;
    let failed = results
        .iter()
        .filter(|result| matches!(result.status, CheckStatus::Failed | CheckStatus::Blocked))
        .count();
    if failed != 0 {
        anyhow::bail!("{failed} Whisply developer verification check(s) did not pass")
    }
    Ok(())
}

fn find_whisply_source_root() -> anyhow::Result<PathBuf> {
    let mut candidate = std::env::current_dir()
        .context("Could not determine the current directory for debug verify")?
        .canonicalize()
        .context("Could not resolve the current directory for debug verify")?;

    loop {
        if ROOT_SENTINELS
            .iter()
            .all(|sentinel| candidate.join(sentinel).is_file())
        {
            return Ok(candidate);
        }
        if !candidate.pop() {
            anyhow::bail!(
                "fixed Whisply verification must run inside a Whisply source tree containing: {}",
                ROOT_SENTINELS.join(", ")
            );
        }
    }
}

fn suite_selects(selected: VerifySuite, check_suite: VerifySuite) -> bool {
    match selected {
        VerifySuite::All => matches!(
            check_suite,
            VerifySuite::Static | VerifySuite::Rust | VerifySuite::Mac | VerifySuite::Website
        ),
        _ => selected == check_suite,
    }
}

fn feature_coverage(feature: VerifyFeature) -> Option<&'static UserConfiguredFeatureCoverage> {
    USER_CONFIGURED_FEATURE_COVERAGE
        .iter()
        .find(|coverage| coverage.feature == feature.coverage_id())
}

fn check_selects_feature(check: &VerificationCheck, features: &[VerifyFeature]) -> bool {
    features.is_empty()
        || check.id == "static-release-feature-coverage"
        || features.iter().any(|feature| {
            feature_coverage(*feature)
                .is_some_and(|coverage| coverage.automated_check_ids.contains(&check.id))
        })
}

fn select_verify_checks(
    suite: VerifySuite,
    features: &[VerifyFeature],
) -> Vec<&'static VerificationCheck> {
    let mut selected_ids = VERIFY_MANIFEST
        .iter()
        .filter(|check| suite_selects(suite, check.suite))
        .filter(|check| check_selects_feature(check, features))
        .map(|check| check.id)
        .collect::<std::collections::HashSet<_>>();

    loop {
        let mut added_prerequisite = false;
        for (dependent_id, prerequisite_ids) in VERIFY_CHECK_PREREQUISITES {
            if !selected_ids.contains(dependent_id) {
                continue;
            }
            for prerequisite_id in *prerequisite_ids {
                let Some(prerequisite) = VERIFY_MANIFEST
                    .iter()
                    .find(|check| check.id == *prerequisite_id)
                else {
                    continue;
                };
                if suite_selects(suite, prerequisite.suite) && selected_ids.insert(prerequisite.id)
                {
                    added_prerequisite = true;
                }
            }
        }
        if !added_prerequisite {
            break;
        }
    }

    VERIFY_MANIFEST
        .iter()
        .filter(|check| suite_selects(suite, check.suite))
        .filter(|check| selected_ids.contains(check.id))
        .collect()
}

fn run_check(source_root: &Path, check: &VerificationCheck, dry_run: bool) -> CheckResult {
    let started = Instant::now();
    // The outer release verifier owns a disposable target tree. Fixed
    // children start with an empty environment, so carry only this explicit
    // artifact location through to prevent full-suite output accumulating in
    // the checked-out runtime source tree. Static projection checks also use
    // the already-built runtime from this target, rather than trying to build
    // one with a deliberately unavailable Cargo toolchain.
    let verifier_cargo_target_dir = std::env::var_os("CARGO_TARGET_DIR");
    let result = match check.action {
        CheckAction::Blocked => CheckResult {
            id: check.id,
            suite: check.suite,
            status: CheckStatus::Blocked,
            duration_ms: 0,
            remediation: check.remediation,
            failure: Some(verify_failure(
                check,
                VerifyFailureKind::ManifestBlocked,
                None,
            )),
        },
        CheckAction::Run {
            program,
            args,
            working_directory,
            required_paths,
        } => {
            if dry_run {
                return CheckResult {
                    id: check.id,
                    suite: check.suite,
                    status: CheckStatus::DryRun,
                    duration_ms: elapsed_ms(started),
                    remediation: check.remediation,
                    failure: None,
                };
            }
            let Some(check_root) = fixed_check_root(source_root, check) else {
                return CheckResult {
                    id: check.id,
                    suite: check.suite,
                    status: CheckStatus::Blocked,
                    duration_ms: elapsed_ms(started),
                    remediation: check.remediation,
                    failure: Some(verify_failure(
                        check,
                        VerifyFailureKind::LocalPrerequisiteUnavailable,
                        None,
                    )),
                };
            };
            if missing_local_prerequisite(&check_root, required_paths) {
                CheckResult {
                    id: check.id,
                    suite: check.suite,
                    status: CheckStatus::Blocked,
                    duration_ms: elapsed_ms(started),
                    remediation: check.remediation,
                    failure: Some(verify_failure(
                        check,
                        VerifyFailureKind::LocalPrerequisiteUnavailable,
                        None,
                    )),
                }
            } else if resolve_trusted_program(program).is_none() {
                CheckResult {
                    id: check.id,
                    suite: check.suite,
                    status: CheckStatus::Blocked,
                    duration_ms: elapsed_ms(started),
                    remediation: check.remediation,
                    failure: Some(verify_failure(
                        check,
                        VerifyFailureKind::TrustedExecutableUnavailable,
                        None,
                    )),
                }
            } else if network_sandbox_unavailable() {
                CheckResult {
                    id: check.id,
                    suite: check.suite,
                    status: CheckStatus::Blocked,
                    duration_ms: elapsed_ms(started),
                    remediation: check.remediation,
                    failure: Some(verify_failure(
                        check,
                        VerifyFailureKind::NetworkSandboxUnavailable,
                        None,
                    )),
                }
            } else {
                let cargo_v8_artifacts = if program == "cargo" {
                    let Some(artifacts) = verified_rusty_v8_from_environment() else {
                        return CheckResult {
                            id: check.id,
                            suite: check.suite,
                            status: CheckStatus::Blocked,
                            duration_ms: elapsed_ms(started),
                            remediation: check.remediation,
                            failure: Some(verify_failure(
                                check,
                                VerifyFailureKind::LocalPrerequisiteUnavailable,
                                None,
                            )),
                        };
                    };
                    Some(artifacts)
                } else {
                    None
                };
                let isolated_home = match IsolatedProductHome::create() {
                    Ok(home) => home,
                    Err(_) => {
                        return CheckResult {
                            id: check.id,
                            suite: check.suite,
                            status: CheckStatus::Blocked,
                            duration_ms: elapsed_ms(started),
                            remediation: check.remediation,
                            failure: Some(verify_failure(
                                check,
                                VerifyFailureKind::IsolatedProductHomeUnavailable,
                                None,
                            )),
                        };
                    }
                };
                let Some(executable) = resolve_trusted_program(program) else {
                    return CheckResult {
                        id: check.id,
                        suite: check.suite,
                        status: CheckStatus::Blocked,
                        duration_ms: elapsed_ms(started),
                        remediation: check.remediation,
                        failure: Some(verify_failure(
                            check,
                            VerifyFailureKind::TrustedExecutableUnavailable,
                            None,
                        )),
                    };
                };
                let Some(tool_home) = developer_tool_home() else {
                    return CheckResult {
                        id: check.id,
                        suite: check.suite,
                        status: CheckStatus::Blocked,
                        duration_ms: elapsed_ms(started),
                        remediation: check.remediation,
                        failure: Some(verify_failure(
                            check,
                            VerifyFailureKind::LocalPrerequisiteUnavailable,
                            None,
                        )),
                    };
                };
                let Some(child_path) = trusted_child_path(program, &executable) else {
                    return CheckResult {
                        id: check.id,
                        suite: check.suite,
                        status: CheckStatus::Blocked,
                        duration_ms: elapsed_ms(started),
                        remediation: check.remediation,
                        failure: Some(verify_failure(
                            check,
                            VerifyFailureKind::TrustedExecutableUnavailable,
                            None,
                        )),
                    };
                };
                let mut child = match verification_sandbox(check) {
                    VerificationSandbox::OuterNetworkDeny => {
                        let mut child = Command::new("/usr/bin/sandbox-exec");
                        child
                            .args(["-p", NETWORK_DENY_SANDBOX_PROFILE, "--"])
                            .arg(&executable);
                        child
                    }
                    VerificationSandbox::TestSuiteOwnsSeatbelt => Command::new(&executable),
                };
                child
                    // Start from an empty environment. This is an allowlist,
                    // not a secret denylist: future credential variables
                    // cannot silently flow into a fixed verification child.
                    .env_clear()
                    .args(args)
                    .current_dir(check_root.join(working_directory))
                    .env("HOME", &isolated_home.home)
                    .env("PATH", child_path)
                    .env("CARGO_NET_OFFLINE", "true")
                    // The fixed coverage run reuses one target tree across
                    // package checks. Incremental artifacts are not useful
                    // for a one-shot release gate and can consume tens of
                    // GiB after a broad test wave.
                    .env("CARGO_INCREMENTAL", "0")
                    // The helper binaries are built with `cargo build`,
                    // which uses Cargo's dev profile rather than the test
                    // profile. They are exercised immediately by the fixed
                    // suite, so debugger symbols only duplicate artifacts
                    // in the disposable release target.
                    .env("CARGO_PROFILE_DEV_DEBUG", "0")
                    // Release verification executes tests; it does not need
                    // debugger symbols in every test binary. Suppress those
                    // test-only artifacts so the complete fixed matrix fits
                    // on a normal developer volume without omitting checks.
                    .env("CARGO_PROFILE_TEST_DEBUG", "0")
                    .env("WHISPLY_VERIFY_NETWORK", "forbidden")
                    .env("SWIFTPM_DISABLE_NETWORK", "1")
                    // Static Python checks must not leave `__pycache__` files
                    // in the shared source tree while this runner is only
                    // collecting fixed, read-only evidence.
                    .env("PYTHONDONTWRITEBYTECODE", "1")
                    .env("WHISPLY_HOME", &isolated_home.whisply_home);
                if let Some(cargo_target_dir) = verifier_cargo_target_dir.as_deref() {
                    child.env("CARGO_TARGET_DIR", cargo_target_dir);
                }
                if program == "cargo" {
                    // Cargo needs the already-installed offline registry and
                    // pinned toolchain, but neither needs to become the child
                    // process's user home.
                    child
                        .env("CARGO_HOME", tool_home.join(".cargo"))
                        .env("RUSTUP_HOME", tool_home.join(".rustup"));
                }
                if check.suite == VerifySuite::Rust {
                    // The complete Rust app-server suite reaches deeply nested
                    // protocol fixture stacks. Keep this test-only override out
                    // of Swift, Python, and website checks.
                    child.env("RUST_MIN_STACK", RUST_VERIFY_STACK_BYTES);
                }
                if let Some(artifacts) = cargo_v8_artifacts {
                    child
                        .env(RUSTY_V8_ARCHIVE_ENV, artifacts.archive)
                        .env(RUSTY_V8_BINDING_ENV, artifacts.binding);
                }
                child.stdout(Stdio::null()).stderr(Stdio::null());

                match child.status() {
                    Ok(exit_status) => {
                        let (status, failure) = if exit_status.success() {
                            (CheckStatus::Passed, None)
                        } else if let Some(code) = exit_status.code() {
                            (
                                CheckStatus::Failed,
                                Some(verify_failure(
                                    check,
                                    VerifyFailureKind::ChildExited,
                                    Some(code),
                                )),
                            )
                        } else {
                            (
                                CheckStatus::Blocked,
                                Some(verify_failure(
                                    check,
                                    VerifyFailureKind::ChildNoExitStatus,
                                    None,
                                )),
                            )
                        };
                        CheckResult {
                            id: check.id,
                            suite: check.suite,
                            status,
                            duration_ms: elapsed_ms(started),
                            remediation: check.remediation,
                            failure,
                        }
                    }
                    Err(_) => CheckResult {
                        id: check.id,
                        suite: check.suite,
                        status: CheckStatus::Blocked,
                        duration_ms: elapsed_ms(started),
                        remediation: check.remediation,
                        failure: Some(verify_failure(
                            check,
                            VerifyFailureKind::ChildLaunchFailed,
                            None,
                        )),
                    },
                }
            }
        }
    };
    result
}

fn verify_failure(
    check: &VerificationCheck,
    kind: VerifyFailureKind,
    exit_code: Option<i32>,
) -> VerifyFailure {
    VerifyFailure {
        kind,
        summary: check.failure_summary,
        exit_code,
        child_output: CHILD_OUTPUT_WITHHELD,
    }
}

fn missing_local_prerequisite(source_root: &Path, required_paths: &[&str]) -> bool {
    required_paths
        .iter()
        .any(|relative_path| !source_root.join(relative_path).exists())
}

/// Resolve the immutable workspace root for a fixed manifest check. The
/// manifest never accepts a caller path; the only non-root workspace is the
/// immediate, canonical Website sibling used for managed Skills/Plugins tests.
fn fixed_check_root(source_root: &Path, check: &VerificationCheck) -> Option<PathBuf> {
    if WEBSITE_SIBLING_CHECK_IDS.contains(&check.id) {
        return fixed_website_sibling_root(source_root);
    }
    Some(source_root.to_path_buf())
}

fn fixed_website_sibling_root(source_root: &Path) -> Option<PathBuf> {
    let source_parent = source_root.parent()?.canonicalize().ok()?;
    let candidate = source_parent
        .join(WEBSITE_SIBLING_DIRECTORY)
        .canonicalize()
        .ok()?;
    (candidate.is_dir()
        && candidate.parent() == Some(source_parent.as_path())
        && candidate
            .file_name()
            .is_some_and(|name| name == WEBSITE_SIBLING_DIRECTORY)
        && WEBSITE_SIBLING_ROOT_SENTINELS
            .iter()
            .all(|sentinel| candidate.join(sentinel).is_file()))
    .then_some(candidate)
}

/// Resolve only executables with a fixed local provenance. This verifier never
/// executes a name from the caller-controlled `PATH`.
fn resolve_trusted_program(program: &str) -> Option<PathBuf> {
    match program {
        "python3" => trusted_system_program("/usr/bin/python3"),
        "swift" => trusted_system_program("/usr/bin/swift"),
        "cargo" => pinned_cargo_path(),
        "npm" => pinned_npm_path(),
        _ => None,
    }
}

fn trusted_system_program(path: &str) -> Option<PathBuf> {
    let path = PathBuf::from(path);
    path.is_file().then_some(path)
}

fn pinned_cargo_path() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let host = match std::env::consts::ARCH {
            "aarch64" => "aarch64-apple-darwin",
            "x86_64" => "x86_64-apple-darwin",
            _ => return None,
        };
        let home = PathBuf::from(std::env::var_os("HOME")?);
        let toolchain = home
            .join(".rustup")
            .join("toolchains")
            .join(format!("{PINNED_RUST_TOOLCHAIN}-{host}"));
        let candidate = toolchain.join("bin").join("cargo");
        let canonical_toolchain = toolchain.canonicalize().ok()?;
        let canonical_candidate = candidate.canonicalize().ok()?;
        (canonical_candidate.is_file() && canonical_candidate.starts_with(canonical_toolchain))
            .then_some(canonical_candidate)
    }

    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// Website checks use the locally installed, version-pinned Homebrew Node
/// runtime. The npm launcher is a script with an `/usr/bin/env node` shebang,
/// and Homebrew links that launcher to its managed npm module directory. The
/// child path still prepends the exact Cellar bin directory so the launcher
/// resolves the matching Node executable.
/// A Homebrew upgrade fails closed until this source pin is reviewed and
/// updated; the verifier never falls back to caller `PATH` or a global npm.
fn pinned_npm_path() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let installation = Path::new("/opt/homebrew/Cellar/node").join(PINNED_NODE_VERSION);
        let canonical_installation = installation.canonicalize().ok()?;
        let node = canonical_installation
            .join("bin")
            .join("node")
            .canonicalize()
            .ok()?;
        let npm_launcher = canonical_installation.join("bin").join("npm");
        let npm = npm_launcher.canonicalize().ok()?;
        let canonical_npm_root = Path::new("/opt/homebrew/lib/node_modules/npm")
            .canonicalize()
            .ok()?;
        (node.is_file()
            && npm_launcher.is_file()
            && npm.is_file()
            && node.starts_with(&canonical_installation)
            && npm.starts_with(canonical_npm_root))
        .then_some(npm_launcher)
    }

    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

fn developer_tool_home() -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    (home.is_absolute() && home.is_dir()).then_some(home)
}

fn trusted_child_path(program: &str, executable: &Path) -> Option<std::ffi::OsString> {
    match program {
        "cargo" => {
            let toolchain_bin = executable.parent()?;
            std::env::join_paths([
                toolchain_bin,
                Path::new("/usr/bin"),
                Path::new("/bin"),
                Path::new("/usr/sbin"),
                Path::new("/sbin"),
            ])
            .ok()
        }
        "npm" => {
            let node_bin = executable.parent()?;
            std::env::join_paths([
                node_bin,
                Path::new("/usr/bin"),
                Path::new("/bin"),
                Path::new("/usr/sbin"),
                Path::new("/sbin"),
            ])
            .ok()
        }
        "python3" | "swift" => Some(TRUSTED_CHILD_SYSTEM_PATH.into()),
        _ => None,
    }
}

fn verified_rusty_v8_from_environment() -> Option<VerifiedRustyV8Artifacts> {
    let archive = PathBuf::from(std::env::var_os(RUSTY_V8_ARCHIVE_ENV)?);
    let binding = PathBuf::from(std::env::var_os(RUSTY_V8_BINDING_ENV)?);
    verified_rusty_v8_artifacts(&archive, &binding, native_v8_target()?)
}

fn native_v8_target() -> Option<&'static str> {
    #[cfg(target_os = "macos")]
    {
        match std::env::consts::ARCH {
            "aarch64" => Some("aarch64-apple-darwin"),
            "x86_64" => Some("x86_64-apple-darwin"),
            _ => None,
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

fn verified_rusty_v8_artifacts(
    archive_override: &Path,
    binding_override: &Path,
    target: &str,
) -> Option<VerifiedRustyV8Artifacts> {
    let archive = canonical_regular_file(archive_override)?;
    let binding = canonical_regular_file(binding_override)?;
    let artifact_directory = archive.parent()?;
    if binding.parent()? != artifact_directory {
        return None;
    }

    let archive_name = format!("librusty_v8_{RUSTY_V8_ARTIFACT_PROFILE}_{target}.a.gz");
    let binding_name = format!("src_binding_{RUSTY_V8_ARTIFACT_PROFILE}_{target}.rs");
    if archive.file_name()?.to_str()? != archive_name
        || binding.file_name()?.to_str()? != binding_name
    {
        return None;
    }

    let checksums_name = format!("rusty_v8_{RUSTY_V8_ARTIFACT_PROFILE}_{target}.sha256");
    let checksums = canonical_regular_file(&artifact_directory.join(checksums_name))?;
    if checksums.parent()? != artifact_directory {
        return None;
    }
    let expected_digests = parse_rusty_v8_checksums(&checksums, [&archive_name, &binding_name])?;
    if !matches_expected_sha256(&archive, expected_digests.get(&archive_name)?)
        || !matches_expected_sha256(&binding, expected_digests.get(&binding_name)?)
    {
        return None;
    }

    Some(VerifiedRustyV8Artifacts { archive, binding })
}

fn canonical_regular_file(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }
    let canonical = path.canonicalize().ok()?;
    canonical.is_file().then_some(canonical)
}

fn parse_rusty_v8_checksums(
    checksums: &Path,
    expected_names: [&str; 2],
) -> Option<BTreeMap<String, String>> {
    // A tiny, fixed manifest is part of the reviewed V8 artifact contract.
    // Refuse a malformed or unexpectedly large file rather than parsing a
    // caller-controlled general checksum format.
    if checksums.metadata().ok()?.len() > 4096 {
        return None;
    }
    let content = std::fs::read_to_string(checksums).ok()?;
    let mut digests = BTreeMap::new();
    for line in content.lines() {
        let mut fields = line.split_ascii_whitespace();
        let digest = fields.next()?;
        let name = fields.next()?;
        if fields.next().is_some()
            || digest.len() != 64
            || !digest.bytes().all(|byte| byte.is_ascii_hexdigit())
            || !expected_names.contains(&name)
            || digests
                .insert(name.to_owned(), digest.to_ascii_lowercase())
                .is_some()
        {
            return None;
        }
    }
    (digests.len() == expected_names.len()
        && expected_names
            .iter()
            .all(|name| digests.contains_key(*name)))
    .then_some(digests)
}

fn matches_expected_sha256(path: &Path, expected: &str) -> bool {
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = match file.read(&mut buffer) {
            Ok(read) => read,
            Err(_) => return false,
        };
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    format!("{:x}", hasher.finalize()) == expected
}

fn network_sandbox_unavailable() -> bool {
    #[cfg(target_os = "macos")]
    {
        !Path::new("/usr/bin/sandbox-exec").is_file()
    }

    #[cfg(not(target_os = "macos"))]
    {
        true
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn print_manifest(json: bool, selected_features: &[VerifyFeature]) -> anyhow::Result<()> {
    let checks: Vec<_> = select_verify_checks(VerifySuite::All, selected_features)
        .into_iter()
        .map(|check| ManifestCheck {
            id: check.id,
            suite: check.suite,
            availability: check.availability,
            remediation: check.remediation,
        })
        .collect();
    let report = ManifestReport {
        manifest_version: VERIFY_MANIFEST_VERSION,
        selected_features,
        checks: &checks,
        user_configured_feature_coverage: USER_CONFIGURED_FEATURE_COVERAGE,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("Whisply debug verify manifest v{VERIFY_MANIFEST_VERSION}");
        if !selected_features.is_empty() {
            println!(
                "Selected feature lanes: {}",
                selected_features
                    .iter()
                    .map(|feature| feature.coverage_id())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        for check in checks {
            println!(
                "{}\t{:?}\t{:?}\t{}",
                check.id, check.suite, check.availability, check.remediation
            );
        }
        print_user_configured_feature_coverage();
    }
    Ok(())
}

fn print_results(
    dry_run: bool,
    json: bool,
    selected_features: &[VerifyFeature],
    results: &[CheckResult],
) -> anyhow::Result<()> {
    let report = VerifyReport {
        manifest_version: VERIFY_MANIFEST_VERSION,
        dry_run,
        selected_features,
        user_configured_feature_coverage: USER_CONFIGURED_FEATURE_COVERAGE,
        results,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("Whisply debug verify manifest v{VERIFY_MANIFEST_VERSION}");
        if !selected_features.is_empty() {
            println!(
                "Selected feature lanes: {}",
                selected_features
                    .iter()
                    .map(|feature| feature.coverage_id())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        for result in results {
            println!(
                "{}\t{:?}\t{}ms\t{}",
                result.id, result.status, result.duration_ms, result.remediation
            );
            if let Some(failure) = result.failure {
                println!("  {}", failure.summary);
                if let Some(exit_code) = failure.exit_code {
                    println!("  Exit code: {exit_code}");
                }
                println!("  Child output: {}", failure.child_output);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[derive(Debug, Parser)]
    struct VerifyParserHarness {
        #[command(flatten)]
        command: VerifyCommand,
    }

    #[derive(Debug, Parser)]
    struct TestParserHarness {
        #[command(subcommand)]
        command: TestSubcommand,
    }

    #[test]
    fn parser_accepts_only_the_fixed_verify_flags() {
        let parsed = VerifyParserHarness::try_parse_from([
            "verify",
            "--suite",
            "website",
            "--dry-run",
            "--json",
        ])
        .expect("fixed verify flags should parse");
        assert_eq!(parsed.command.suite, VerifySuite::Website);
        assert!(parsed.command.dry_run);
        assert!(parsed.command.json);
        let listed = VerifyParserHarness::try_parse_from(["verify", "--list"])
            .expect("the fixed manifest list flag should parse");
        assert!(listed.command.list);
        assert_eq!(listed.command.suite, VerifySuite::All);
        assert!(VerifyParserHarness::try_parse_from(["verify", "arbitrary-command"]).is_err());
        assert!(VerifyParserHarness::try_parse_from(["verify", "--command", "true"]).is_err());
    }

    #[test]
    fn parser_accepts_only_named_fixed_feature_lanes() {
        let parsed = VerifyParserHarness::try_parse_from([
            "verify",
            "--feature",
            "custom-mcp-stdio-http-oauth",
            "--feature",
            "skills",
            "--dry-run",
        ])
        .expect("fixed feature selectors should parse");
        assert_eq!(
            parsed.command.features,
            vec![
                VerifyFeature::CustomMcpStdioHttpOauth,
                VerifyFeature::Skills,
            ]
        );
        assert!(
            VerifyParserHarness::try_parse_from(["verify", "--feature", "arbitrary-command",])
                .is_err()
        );
    }

    #[test]
    fn release_test_accepts_only_named_fixed_groups() {
        let parsed = TestParserHarness::try_parse_from([
            "test",
            "release",
            "--group",
            "policy",
            "--dry-run",
            "--json",
        ])
        .expect("fixed release-test flags should parse");
        let TestSubcommand::Release(command) = parsed.command;
        assert_eq!(command.group, ReleaseTestGroup::Policy);
        assert!(command.dry_run);
        assert!(command.json);

        let listed = TestParserHarness::try_parse_from(["test", "release", "--list"])
            .expect("the fixed release-test list flag should parse");
        let TestSubcommand::Release(command) = listed.command;
        assert!(command.list);
        assert_eq!(command.group, ReleaseTestGroup::Full);

        assert!(
            TestParserHarness::try_parse_from(["test", "release", "arbitrary-command"]).is_err()
        );
        assert!(
            TestParserHarness::try_parse_from(["test", "release", "--command", "true"]).is_err()
        );
        assert!(
            TestParserHarness::try_parse_from([
                "test",
                "release",
                "--config",
                "runner=/tmp/unsafe",
            ])
            .is_err()
        );
    }

    #[test]
    fn release_test_full_selects_every_defined_group_without_a_child_action() {
        let selected: Vec<_> = RELEASE_TEST_GROUPS
            .iter()
            .filter(|definition| release_group_selects(ReleaseTestGroup::Full, definition.group))
            .map(|definition| definition.group)
            .collect();
        assert_eq!(selected.len(), RELEASE_TEST_GROUPS.len() - 1);
        assert!(!selected.contains(&ReleaseTestGroup::Full));
        assert!(RELEASE_TEST_PROHIBITED_ACTIONS.contains(&"scripts/ship.sh"));
        assert!(RELEASE_TEST_PROHIBITED_ACTIONS.contains(&"authority grant"));
        assert!(RELEASE_TEST_PROHIBITED_ACTIONS.contains(&"final confirmation"));
        assert_eq!(RELEASE_TEST_MANIFEST_VERSION, 2);
        assert_eq!(USER_CONFIGURED_FEATURE_COVERAGE.len(), 11);
        assert!(USER_CONFIGURED_FEATURE_COVERAGE.iter().all(|coverage| {
            !coverage.automated_check_ids.is_empty() && !coverage.fixture_paths.is_empty()
        }));
    }

    #[test]
    fn manifest_is_versioned_fixed_and_never_selects_integration_for_all() {
        assert_eq!(VERIFY_MANIFEST_VERSION, 27);
        let unique_ids: std::collections::HashSet<_> =
            VERIFY_MANIFEST.iter().map(|check| check.id).collect();
        assert_eq!(VERIFY_MANIFEST.len(), unique_ids.len());
        assert!(
            VERIFY_MANIFEST
                .iter()
                .any(|check| check.id == "static-ship-full-verification-gate")
        );
        assert!(
            VERIFY_MANIFEST
                .iter()
                .any(|check| check.id == "static-installed-live-diagnostics-contract")
        );
        assert!(
            VERIFY_MANIFEST
                .iter()
                .any(|check| check.id == "static-live-diagnostics-security")
        );
        assert!(
            VERIFY_MANIFEST
                .iter()
                .any(|check| check.id == "static-diagnostic-fixture-contract")
        );
        assert!(
            VERIFY_MANIFEST
                .iter()
                .any(|check| check.id == "mac-diagnostic-fixture-scenarios-full")
        );
        assert!(VERIFY_MANIFEST.iter().all(|check| match check.action {
            CheckAction::Run { program, args, .. } => {
                program != "sh" && !args.contains(&"scripts/ship.sh")
            }
            CheckAction::Blocked => true,
        }));
        assert!(
            VERIFY_MANIFEST
                .iter()
                .filter(|check| suite_selects(VerifySuite::All, check.suite))
                .all(|check| check.suite != VerifySuite::Integration)
        );
    }

    #[test]
    fn feature_selectors_choose_only_their_fixed_coverage_checks_and_prerequisites() {
        let custom_mcp =
            select_verify_checks(VerifySuite::All, &[VerifyFeature::CustomMcpStdioHttpOauth]);
        let custom_mcp_ids: std::collections::HashSet<_> =
            custom_mcp.iter().map(|check| check.id).collect();
        assert_eq!(
            custom_mcp_ids,
            std::collections::HashSet::from([
                "static-release-feature-coverage",
                "rust-app-server-helper-cli",
                "rust-app-server-helper-stdio",
                "rust-app-server-helper-code-mode",
                "rust-core-full",
                "rust-rmcp-client-full",
                "rust-codex-mcp-full",
                "rust-mcp-extension-full",
                "rust-mcp-server-full",
                "rust-app-server-v2-full",
            ])
        );

        let browser = select_verify_checks(
            VerifySuite::All,
            &[VerifyFeature::BrowserAndComputerUseInterfaces],
        );
        assert_eq!(
            browser.iter().map(|check| check.id).collect::<Vec<_>>(),
            vec![
                "static-release-feature-coverage",
                "static-diagnostic-fixture-contract",
                "mac-swift-package-full",
                "mac-diagnostic-fixture-scenarios-full",
            ]
        );
        let memory_audio = select_verify_checks(
            VerifySuite::All,
            &[VerifyFeature::MemoryAudioAndTranscripts],
        );
        assert_eq!(
            memory_audio
                .iter()
                .map(|check| check.id)
                .collect::<Vec<_>>(),
            vec![
                "static-release-feature-coverage",
                "mac-memory-audio-transcript-contracts",
            ]
        );
        assert_eq!(
            select_verify_checks(VerifySuite::Website, &[VerifyFeature::Skills])
                .iter()
                .map(|check| check.id)
                .collect::<Vec<_>>(),
            vec!["website-managed-skills-plugins-contracts"]
        );

        for feature in [
            VerifyFeature::GoalsAndPlanUpdates,
            VerifyFeature::Hooks,
            VerifyFeature::CustomMcpStdioHttpOauth,
            VerifyFeature::Plugins,
            VerifyFeature::Skills,
            VerifyFeature::Agents,
            VerifyFeature::LocalExecutionAndFileEditing,
            VerifyFeature::MemoryAudioAndTranscripts,
            VerifyFeature::BrowserAndComputerUseInterfaces,
            VerifyFeature::Connectors,
        ] {
            assert_eq!(
                feature_coverage(feature).map(|coverage| coverage.feature),
                Some(feature.coverage_id())
            );
        }
    }

    #[test]
    fn fixed_helper_prerequisites_are_selected_before_their_dependent_suites() {
        for (dependent_id, prerequisite_ids) in VERIFY_CHECK_PREREQUISITES {
            assert!(
                VERIFY_MANIFEST
                    .iter()
                    .any(|check| check.id == *dependent_id)
            );
            for prerequisite_id in *prerequisite_ids {
                let prerequisite_position = VERIFY_MANIFEST
                    .iter()
                    .position(|check| check.id == *prerequisite_id)
                    .expect("fixed prerequisite must be in the manifest");
                let dependent_position = VERIFY_MANIFEST
                    .iter()
                    .position(|check| check.id == *dependent_id)
                    .expect("fixed dependent must be in the manifest");
                assert!(
                    prerequisite_position < dependent_position,
                    "{prerequisite_id} must run before {dependent_id}"
                );
            }
        }

        let custom_mcp =
            select_verify_checks(VerifySuite::Rust, &[VerifyFeature::CustomMcpStdioHttpOauth]);
        let selected_ids = custom_mcp.iter().map(|check| check.id).collect::<Vec<_>>();
        assert!(selected_ids.contains(&"rust-app-server-helper-cli"));
        assert!(selected_ids.contains(&"rust-app-server-helper-code-mode"));
        assert!(selected_ids.contains(&"rust-app-server-helper-stdio"));
        assert!(
            selected_ids
                .iter()
                .position(|id| *id == "rust-app-server-helper-cli")
                < selected_ids.iter().position(|id| *id == "rust-core-full")
        );
        assert!(
            selected_ids
                .iter()
                .position(|id| *id == "rust-app-server-helper-code-mode")
                < selected_ids.iter().position(|id| *id == "rust-core-full")
        );
        assert!(
            selected_ids
                .iter()
                .position(|id| *id == "rust-app-server-helper-stdio")
                < selected_ids.iter().position(|id| *id == "rust-core-full")
        );
    }

    #[test]
    fn only_fixed_sandbox_suites_own_the_nested_seatbelt_boundary() {
        for check in VERIFY_MANIFEST {
            let expected = if matches!(
                check.id,
                "rust-app-server-v2-full"
                    | "rust-core-full"
                    | "rust-exec-server-full"
                    | "rust-exec-full"
                    | "mac-diagnostic-fixture-scenarios-full"
            ) {
                VerificationSandbox::TestSuiteOwnsSeatbelt
            } else {
                VerificationSandbox::OuterNetworkDeny
            };
            assert_eq!(verification_sandbox(check), expected, "{}", check.id);
        }
    }

    #[test]
    fn shared_fixture_rust_suites_run_serially() {
        for id in [
            "rust-core-full",
            "rust-mcp-server-full",
            "rust-exec-server-full",
            "rust-app-server-v2-full",
        ] {
            let check = VERIFY_MANIFEST
                .iter()
                .find(|check| check.id == id)
                .expect("fixed Rust check must be in the manifest");
            let CheckAction::Run { args, .. } = check.action else {
                panic!("{id} must be runnable");
            };
            assert!(
                args.windows(2)
                    .any(|pair| pair == ["--", "--test-threads=1"]),
                "{id} must run serially"
            );
        }
    }

    #[test]
    fn dry_run_needs_no_local_toolchain_or_dependencies() {
        let result = run_check(
            Path::new("/missing-whisply-source-root"),
            &VERIFY_MANIFEST[0],
            true,
        );
        assert_eq!(result.status, CheckStatus::DryRun);
        assert!(result.failure.is_none());
    }

    #[test]
    fn child_product_homes_are_isolated_empty_directories() -> anyhow::Result<()> {
        let isolated_home = IsolatedProductHome::create()?;

        for path in [&isolated_home.home, &isolated_home.whisply_home] {
            assert!(path.is_dir());
            assert!(path.read_dir()?.next().is_none());
            assert!(path.starts_with(isolated_home._root.path()));
        }
        assert_ne!(isolated_home.home, isolated_home.whisply_home);
        Ok(())
    }

    #[test]
    fn cargo_v8_overrides_require_an_exact_verified_artifact_pair() -> anyhow::Result<()> {
        let temporary = tempfile::tempdir()?;
        let target = "aarch64-apple-darwin";
        let archive_name = format!("librusty_v8_{RUSTY_V8_ARTIFACT_PROFILE}_{target}.a.gz");
        let binding_name = format!("src_binding_{RUSTY_V8_ARTIFACT_PROFILE}_{target}.rs");
        let checksums_name = format!("rusty_v8_{RUSTY_V8_ARTIFACT_PROFILE}_{target}.sha256");
        let archive = temporary.path().join(&archive_name);
        let binding = temporary.path().join(&binding_name);
        let checksums = temporary.path().join(checksums_name);
        std::fs::write(&archive, b"verified archive")?;
        std::fs::write(&binding, b"verified binding")?;
        std::fs::write(
            &checksums,
            format!(
                "{}  {archive_name}\n{}  {binding_name}\n",
                sha256_for_test(&archive)?,
                sha256_for_test(&binding)?,
            ),
        )?;

        let verified = verified_rusty_v8_artifacts(&archive, &binding, target)
            .expect("the exact checksummed pair should be accepted");
        assert_eq!(verified.archive, archive.canonicalize()?);
        assert_eq!(verified.binding, binding.canonicalize()?);

        std::fs::write(&binding, b"tampered binding")?;
        assert!(verified_rusty_v8_artifacts(&archive, &binding, target).is_none());
        Ok(())
    }

    fn sha256_for_test(path: &Path) -> anyhow::Result<String> {
        let bytes = std::fs::read(path)?;
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        Ok(format!("{:x}", hasher.finalize()))
    }

    #[test]
    fn reports_expose_only_typed_fixed_failure_metadata() {
        let result = CheckResult {
            id: "test",
            suite: VerifySuite::Static,
            status: CheckStatus::Failed,
            duration_ms: 1,
            remediation: "fixed remediation",
            failure: Some(VerifyFailure {
                kind: VerifyFailureKind::ChildExited,
                summary: "Fixed static check failed.",
                exit_code: Some(1),
                child_output: CHILD_OUTPUT_WITHHELD,
            }),
        };
        let encoded = serde_json::to_string(&result).expect("result serialization should succeed");
        assert!(encoded.contains("\"kind\":\"child_exited\""));
        assert!(encoded.contains("\"exitCode\":1"));
        assert!(encoded.contains("\"childOutput\":\"withheld\""));
        assert!(!encoded.contains("provider-secret"));
    }

    #[test]
    fn failure_metadata_uses_the_manifest_static_summary() {
        let failure = verify_failure(
            &VERIFY_MANIFEST[0],
            VerifyFailureKind::ChildLaunchFailed,
            None,
        );

        assert_eq!(failure.summary, VERIFY_MANIFEST[0].failure_summary);
        assert_eq!(failure.child_output, CHILD_OUTPUT_WITHHELD);
        assert_eq!(failure.exit_code, None);
    }
}

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

#[derive(Debug, Args)]
pub(crate) struct InstalledLiveCommand {
    #[command(subcommand)]
    action: InstalledLiveAction,
}

#[derive(Debug, clap::Subcommand)]
enum InstalledLiveAction {
    /// Create an owner-only session and attach the verified installed app.
    Start(InstalledLiveStartArgs),
    /// Read the installed app's signed live-diagnostic status.
    Status(InstalledLiveSessionArgs),
    /// Start one ordinary, policy-bound app run through the installed app.
    Run(InstalledLiveRunArgs),
    /// Wait for complete signed evidence and validate its release binding.
    Wait(InstalledLiveWaitArgs),
    /// Validate a completed run's signed, local evidence bundle.
    Assert(InstalledLiveRunReferenceArgs),
    /// Ask the installed app to stop one exact active run.
    Stop(InstalledLiveRunReferenceArgs),
    /// Capture one installed-app diagnostic snapshot for an exact active run.
    Snapshot(InstalledLiveRunReferenceArgs),
    /// Return redacted event names from one exact local evidence bundle.
    Tail(InstalledLiveTailArgs),
    /// Inspect the current account's aggregate connector availability.
    AccountProjection(InstalledLiveSessionArgs),
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
    /// Run either of the fixed paired Computer Use efficiency cohorts.
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
struct InstalledLiveTailArgs {
    /// Canonical session UUID; defaults to the owner-only current session.
    #[arg(long)]
    session: Option<String>,
    /// Exact run UUID returned by `diagnostics live run`.
    run_id: String,
    /// Maximum number of redacted event summaries to return.
    #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u16).range(1..=1_000))]
    limit: u16,
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
            InstalledLiveAction::Tail(args) => {
                let session = LiveSession::load_allow_expired(args.session.as_deref())?;
                let summaries = session.event_summaries(&args.run_id, usize::from(args.limit))?;
                print_json(&serde_json::json!({
                    "ok": true,
                    "runID": canonical_uuid(&args.run_id)?,
                    "events": summaries,
                }))
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
            let root = sessions_root()?;
            validate_private_directory(&root)?;
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
            let events = self.event_summaries(&run_id, 1)?;
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
            let event_summaries = self.event_summaries(&run_id, MAX_EVENT_LINES)?;
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

        fn event_summaries(&self, run_id: &str, limit: usize) -> anyhow::Result<Vec<Value>> {
            let run_id = canonical_uuid(run_id)?;
            let run = self.run_directory(&run_id)?;
            let path = run.join("events.jsonl");
            validate_private_file(&path)?;
            let bytes = read_private_bytes(&path, MAX_EVIDENCE_BYTES as usize)?;
            let mut summaries = Vec::new();
            let mut last_sequence = 0_u64;
            for line in bytes
                .split(|byte| *byte == b'\n')
                .filter(|line| !line.is_empty())
            {
                if summaries.len() >= MAX_EVENT_LINES {
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
                summaries.push(serde_json::json!({
                    "sequence": event.get("sequence").cloned().unwrap_or(Value::Null),
                    "name": event.get("name").cloned().unwrap_or(Value::Null),
                    "timestampMs": event.get("timestampMs").cloned().unwrap_or(Value::Null),
                }));
            }
            if summaries.len() > limit {
                let start = summaries.len() - limit;
                Ok(summaries.split_off(start))
            } else {
                Ok(summaries)
            }
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

    fn validate_prompt(prompt: &str) -> anyhow::Result<()> {
        if prompt.trim().is_empty() || prompt.len() > 128 * 1_024 {
            anyhow::bail!("Installed live diagnostic prompt must contain 1 byte through 128 KiB.");
        }
        Ok(())
    }

    fn validate_sources(sources: &[String]) -> anyhow::Result<()> {
        if sources.len() > 3
            || sources.iter().any(|value| {
                value.trim().is_empty()
                    || value.len() > 240
                    || value.chars().any(|character| character.is_control())
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

    fn valid_command_name(value: &str) -> bool {
        matches!(
            value,
            "status" | "run" | "stop" | "snapshot" | "account-projection" | "close"
        )
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
        fn hex_decoder_rejects_noncanonical_values() {
            assert_eq!(decode_hex("00ff"), Some(vec![0, 255]));
            assert_eq!(decode_hex("00FF"), None);
            assert_eq!(decode_hex("0"), None);
        }
    }
}

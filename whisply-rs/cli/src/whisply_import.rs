//! Copy-only import of a Codex home into the managed Whisply home.
//!
//! Three properties define this command, and each is a rule about what it
//! refuses to do rather than a feature.
//!
//! **It is gated by an allowlist, not a denylist.** A `.codex` home holds
//! roughly fifty entries, most of them private runtime storage: rollout
//! transcripts, a session index, several SQLite databases, OAuth locks, caches.
//! Only entries whose format is part of a supported public API are imported;
//! everything else is reported with a reason and left alone. That direction
//! matters more than the specific list. A denylist would import whatever Codex
//! adds next, and the next thing could be a credential.
//!
//! **It never writes to the source.** Every path it opens under `--from` is
//! opened for reading. There is no move, no cleanup, no lockfile, and no
//! "migrated" marker written back, so importing twice is the same as importing
//! once and the Codex install keeps working afterwards.
//!
//! **It never overwrites.** If a destination already exists, the entry is
//! reported as a conflict and skipped. Nothing here is important enough to
//! justify silently replacing something a user already has.
//!
//! It also previews by default. `--apply` is the only thing that copies.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use clap::Args;
use serde::Serialize;
use whisply_config::CONFIG_TOML_FILE;
use whisply_core_plugins::validate_skill_package_contents;

use crate::whisply_skills::managed_home;

/// The instructions file both products read from the home directory.
const INSTRUCTIONS_FILE: &str = "AGENTS.md";
/// The skill package root, in both products.
const SKILLS_DIR: &str = "skills";

/// Preference keys `whisply config` can itself write.
///
/// This is the feasibility gate for configuration, and it is deliberately not a
/// judgement about which keys look harmless. A key is importable exactly when
/// the supported configuration API can already set it, because that is what
/// makes the imported value a supported preference rather than a setting
/// synthesized into a file nothing owns. It also happens to make the secret
/// exclusion structural: none of these can hold one.
///
/// Kept in step with `whisply_config::print_projection`, which reads the same
/// paths back out.
const IMPORTABLE_CONFIG_KEYS: &[&[&str]] = &[
    &["model"],
    &["model_reasoning_effort"],
    &["service_tier"],
    &["tui", "session_picker_view"],
    &["tui", "resume_cwd"],
];

/// A `.codex` entry may be large; a skill tree of this size is already refused
/// by the package validator, so this only bounds the two single files.
const MAX_IMPORTED_FILE_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Args)]
pub(crate) struct ImportCommand {
    #[command(subcommand)]
    action: ImportSubcommand,
}

#[derive(Debug, clap::Subcommand)]
enum ImportSubcommand {
    /// Preview, and optionally copy, the supported parts of a Codex home.
    Codex(ImportCodexArgs),
}

#[derive(Debug, Args)]
struct ImportCodexArgs {
    /// The Codex home to read. Defaults to `~/.codex`.
    #[arg(long, value_name = "DIR")]
    from: Option<PathBuf>,

    /// Copy the importable entries. Without this the command only previews.
    #[arg(long)]
    apply: bool,

    /// Emit the plan as JSON.
    #[arg(long)]
    json: bool,
}

/// Why an entry is not imported.
///
/// These exist to make the preview readable. They are not what decides: the
/// decision is [`plan_entry`]'s allowlist, and an entry nobody has classified
/// falls through to [`Skipped::Unsupported`] rather than being imported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Skipped {
    /// Credentials, tokens, or the account identity behind them.
    Credentials,
    /// Rollout transcripts, the session index, and the internal databases.
    /// Reconstructing these in Whisply's own storage would be synthesizing a
    /// private format rather than importing a supported one.
    PrivateStorage,
    /// Caches, locks, and scratch directories that belong to a running Codex.
    RuntimeState,
    /// Content that runs commands. Importing it would move executable behavior
    /// between products without the user seeing what it does.
    ExecutableContent,
    /// Recognized, but not part of any supported import.
    Unsupported,
}

impl Skipped {
    fn reason(self) -> &'static str {
        match self {
            Self::Credentials => "holds credentials, which are never imported",
            Self::PrivateStorage => "private Codex storage, which Whisply does not synthesize",
            Self::RuntimeState => "runtime state belonging to the Codex install",
            Self::ExecutableContent => "runs commands, so it is not imported unattended",
            Self::Unsupported => "not part of a supported import",
        }
    }
}

/// One planned action, before anything has been copied.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
enum Planned {
    /// Will be created at `destination`.
    Import { destination: String },
    /// Left alone, with a reason the user can read.
    Skip {
        reason: Skipped,
        explanation: String,
    },
    /// Importable, but something is already there. Never overwritten.
    Conflict { destination: String },
}

#[derive(Debug, Clone, Serialize)]
struct PlannedEntry {
    name: String,
    #[serde(flatten)]
    planned: Planned,
}

#[derive(Debug, Serialize)]
struct ImportPlan {
    source: String,
    destination: String,
    applied: bool,
    entries: Vec<PlannedEntry>,
}

pub(crate) fn run_import(command: ImportCommand) -> anyhow::Result<()> {
    match command.action {
        ImportSubcommand::Codex(args) => run_import_codex(args),
    }
}

fn run_import_codex(args: ImportCodexArgs) -> anyhow::Result<()> {
    let source = resolve_source(args.from.as_deref())?;
    let destination = managed_home()?;
    if source == destination {
        anyhow::bail!("the Codex home and the Whisply home are the same directory.");
    }

    let mut entries = plan(&source, &destination)?;
    if args.apply {
        apply(&source, &destination, &mut entries)?;
    }

    let plan = ImportPlan {
        source: source.display().to_string(),
        destination: destination.display().to_string(),
        applied: args.apply,
        entries,
    };
    if args.json {
        println!("{}", serde_json::to_string_pretty(&plan)?);
    } else {
        print_plan(&plan);
    }
    Ok(())
}

fn resolve_source(from: Option<&Path>) -> anyhow::Result<PathBuf> {
    let source = match from {
        Some(path) => path.to_path_buf(),
        None => dirs::home_dir()
            .context("could not resolve a home directory to find ~/.codex")?
            .join(".codex"),
    };
    let metadata = fs::symlink_metadata(&source)
        .with_context(|| format!("no Codex home at {}", source.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        anyhow::bail!(
            "the Codex home must be a real directory, not a link or special file: {}",
            source.display()
        );
    }
    Ok(source)
}

/// Classifies every entry in the source, in name order.
fn plan(source: &Path, destination: &Path) -> anyhow::Result<Vec<PlannedEntry>> {
    let mut names: Vec<String> = Vec::new();
    for entry in fs::read_dir(source).context("failed to read the Codex home")? {
        let entry = entry?;
        names.push(entry.file_name().to_string_lossy().into_owned());
    }
    names.sort();

    let mut planned = Vec::with_capacity(names.len());
    for name in names {
        // Skills are the one entry whose outcome is not uniform: a home can
        // hold a dozen packages and the validator may refuse some of them. A
        // single `skills` row would claim they all imported, so each package
        // gets its own row and the preview stays true.
        if name == SKILLS_DIR {
            planned.extend(plan_skill_packages(source, destination));
            continue;
        }
        planned.push(PlannedEntry {
            planned: plan_entry(&name, source, destination),
            name,
        });
    }
    Ok(planned)
}

/// Plans one row per skill package, running the same validator the copy will.
fn plan_skill_packages(source: &Path, destination: &Path) -> Vec<PlannedEntry> {
    let root = source.join(SKILLS_DIR);
    let Ok(entries) = fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut packages: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
    packages.sort();

    let mut planned = Vec::new();
    for package in packages {
        let Some(file_name) = package.file_name() else {
            continue;
        };
        let name = format!("{SKILLS_DIR}/{}", file_name.to_string_lossy());
        let Ok(metadata) = fs::symlink_metadata(&package) else {
            continue;
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            planned.push(PlannedEntry {
                name,
                planned: Planned::Skip {
                    reason: Skipped::Unsupported,
                    explanation: "not a skill package".to_string(),
                },
            });
            continue;
        }
        let target = destination.join(SKILLS_DIR).join(file_name);
        let planned_outcome = if fs::symlink_metadata(&target).is_ok() {
            Planned::Conflict {
                destination: target.display().to_string(),
            }
        } else if let Err(error) = validate_skill_package_contents(&package) {
            Planned::Skip {
                reason: skip_reason_for_package_error(&error),
                explanation: error.to_string(),
            }
        } else {
            Planned::Import {
                destination: target.display().to_string(),
            }
        };
        planned.push(PlannedEntry {
            name,
            planned: planned_outcome,
        });
    }
    planned
}

/// The validator refuses packages for two different kinds of reason, and only
/// one of them is about secrets. Reporting them the same way would hide which.
fn skip_reason_for_package_error(error: &std::io::Error) -> Skipped {
    if error.to_string().contains("credential") {
        Skipped::Credentials
    } else {
        Skipped::Unsupported
    }
}

/// The allowlist.
///
/// Only three shapes are importable. Everything else -- including anything a
/// future Codex adds -- falls through to a skip, which is the whole point of
/// writing it this way round.
fn plan_entry(name: &str, source: &Path, destination: &Path) -> Planned {
    match name {
        INSTRUCTIONS_FILE => plan_copy(name, source, destination),
        CONFIG_TOML_FILE => plan_config(source, destination),
        other => {
            let reason = classify_skip(other);
            Planned::Skip {
                reason,
                explanation: reason.reason().to_string(),
            }
        }
    }
}

fn plan_copy(name: &str, source: &Path, destination: &Path) -> Planned {
    let target = destination.join(name);
    if fs::symlink_metadata(&target).is_ok() {
        return Planned::Conflict {
            destination: target.display().to_string(),
        };
    }
    if fs::symlink_metadata(source.join(name)).is_err() {
        return Planned::Skip {
            reason: Skipped::Unsupported,
            explanation: Skipped::Unsupported.reason().to_string(),
        };
    }
    Planned::Import {
        destination: target.display().to_string(),
    }
}

/// Configuration is never copied as a file: only the supported keys are read
/// out of it, so an unsupported or secret-bearing key cannot ride along.
fn plan_config(source: &Path, destination: &Path) -> Planned {
    let target = destination.join(CONFIG_TOML_FILE);
    match read_importable_config(&source.join(CONFIG_TOML_FILE)) {
        Ok(values) if values.is_empty() => Planned::Skip {
            reason: Skipped::Unsupported,
            explanation: "holds no preference Whisply can set".to_string(),
        },
        Ok(_) if fs::symlink_metadata(&target).is_ok() => Planned::Conflict {
            destination: target.display().to_string(),
        },
        Ok(_) => Planned::Import {
            destination: target.display().to_string(),
        },
        Err(_) => Planned::Skip {
            reason: Skipped::Unsupported,
            explanation: "could not be read as configuration".to_string(),
        },
    }
}

/// Reads only [`IMPORTABLE_CONFIG_KEYS`] out of a Codex `config.toml`.
///
/// Everything else in that file stays where it is, including `mcp_servers`
/// (whose `env` and headers carry tokens), `projects` (trust decisions about
/// specific directories on this machine, which are the user's to make again),
/// and `notify` (a command line).
fn read_importable_config(path: &Path) -> anyhow::Result<BTreeMap<String, toml::Value>> {
    let text = read_bounded(path)?;
    let parsed: toml::Value = toml::from_str(&text).context("config.toml is not valid TOML")?;
    let mut importable = BTreeMap::new();
    for key_path in IMPORTABLE_CONFIG_KEYS {
        let Some(value) = lookup(&parsed, key_path) else {
            continue;
        };
        // A table or array here would mean the key does not have the shape the
        // supported API writes, so it is not the key we think it is.
        if !value.is_str() && !value.is_bool() && !value.is_integer() {
            continue;
        }
        importable.insert(key_path.join("."), value.clone());
    }
    Ok(importable)
}

fn lookup<'a>(root: &'a toml::Value, key_path: &[&str]) -> Option<&'a toml::Value> {
    let mut current = root;
    for key in key_path {
        current = current.as_table()?.get(*key)?;
    }
    Some(current)
}

/// Reads a bounded amount of text without following a link.
///
/// `O_NOFOLLOW` rather than only a `symlink_metadata` check: the source is
/// someone else's directory, and between the check and the open it could become
/// a link pointing anywhere this process can read.
fn read_bounded(path: &Path) -> anyhow::Result<String> {
    Ok(String::from_utf8(read_bounded_bytes(path)?)?)
}

/// Reads a bounded file without following a link.
///
/// `O_NOFOLLOW` rather than only a `symlink_metadata` check: the source is
/// someone else's directory, and between the check and the open it could become
/// a link pointing anywhere this process can read.
fn read_bounded_bytes(path: &Path) -> anyhow::Result<Vec<u8>> {
    use std::io::Read;

    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        anyhow::bail!("{} is not a regular file", path.display());
    }
    if metadata.len() > MAX_IMPORTED_FILE_BYTES {
        anyhow::bail!("{} is larger than the import limit", path.display());
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut bytes = Vec::new();
    options
        .open(path)?
        .take(MAX_IMPORTED_FILE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_IMPORTED_FILE_BYTES {
        anyhow::bail!("{} is larger than the import limit", path.display());
    }
    Ok(bytes)
}

/// Names a `.codex` home is known to hold, for a readable preview.
///
/// Being wrong here costs a vaguer message and nothing else: an unrecognized
/// name is skipped either way.
fn classify_skip(name: &str) -> Skipped {
    if name.starts_with('.') {
        return Skipped::RuntimeState;
    }
    if name.ends_with(".sqlite")
        || name.contains(".sqlite-")
        || name.ends_with(".jsonl")
        || name == "sessions"
        || name == "archived_sessions"
        || name == "sqlite"
    {
        return Skipped::PrivateStorage;
    }
    match name {
        "auth.json" | "installation_id" | "mcp-oauth-locks" => Skipped::Credentials,
        "hooks.json" | "plugins" | "automations" => Skipped::ExecutableContent,
        "cache"
        | "tmp"
        | "ipc"
        | "process_manager"
        | "thread-writer-locks"
        | "shell_snapshots"
        | "memories"
        | "logs" => Skipped::RuntimeState,
        _ => Skipped::Unsupported,
    }
}

/// Performs the planned imports. Conflicts and skips are left untouched.
fn apply(source: &Path, destination: &Path, entries: &mut [PlannedEntry]) -> anyhow::Result<()> {
    for entry in entries.iter_mut() {
        let Planned::Import { .. } = entry.planned else {
            continue;
        };
        let name = entry.name.clone();
        let result = if name == CONFIG_TOML_FILE {
            write_config(source, destination)
        } else if let Some(package) = name.strip_prefix(&format!("{SKILLS_DIR}/")) {
            copy_skill_package(source, destination, package)
        } else {
            copy_file(&source.join(&name), &destination.join(&name))
        };
        if let Err(error) = result {
            entry.planned = Planned::Skip {
                reason: Skipped::Unsupported,
                explanation: format!("could not be imported: {error:#}"),
            };
            eprintln!("whisply import codex: skipped {name}: {error:#}");
        }
    }
    Ok(())
}

fn write_config(source: &Path, destination: &Path) -> anyhow::Result<()> {
    let values = read_importable_config(&source.join(CONFIG_TOML_FILE))?;
    let mut document = toml::value::Table::new();
    for (key_path, value) in values {
        let mut parts = key_path.split('.').peekable();
        let mut table = &mut document;
        while let Some(part) = parts.next() {
            if parts.peek().is_none() {
                table.insert(part.to_string(), value.clone());
                break;
            }
            table = table
                .entry(part.to_string())
                .or_insert_with(|| toml::Value::Table(toml::value::Table::new()))
                .as_table_mut()
                .context("importable configuration key is not a table")?;
        }
    }
    let rendered = toml::to_string_pretty(&toml::Value::Table(document))?;
    write_new_file(&destination.join(CONFIG_TOML_FILE), rendered.as_bytes())
}

/// Copies one skill package, re-running the validator that planning ran.
///
/// Validating twice is deliberate. Planning may have happened seconds ago
/// against a directory this process does not own, and the validator is what
/// refuses symlinks, oversized trees, credential file names, and literal
/// secrets. Trusting the earlier answer would make the copy depend on the
/// package not having changed in between.
fn copy_skill_package(source: &Path, destination: &Path, package: &str) -> anyhow::Result<()> {
    let from = source.join(SKILLS_DIR).join(package);
    let to = destination.join(SKILLS_DIR).join(package);
    if fs::symlink_metadata(&to).is_ok() {
        anyhow::bail!("a skill named {package} is already installed");
    }
    validate_skill_package_contents(&from)?;
    copy_tree(&from, &to)
}

fn copy_tree(source: &Path, destination: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let from = entry.path();
        let to = destination.join(entry.file_name());
        let metadata = fs::symlink_metadata(&from)?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            copy_tree(&from, &to)?;
        } else {
            copy_file(&from, &to)?;
        }
    }
    Ok(())
}

fn copy_file(source: &Path, destination: &Path) -> anyhow::Result<()> {
    write_new_file(destination, &read_bounded_bytes(source)?)
}

/// Writes only if nothing is there.
///
/// `create_new` is what makes "never overwrite" a property of the syscall
/// rather than of the check above it, which could otherwise lose a race with
/// anything else writing into the managed home.
fn write_new_file(path: &Path, contents: &[u8]) -> anyhow::Result<()> {
    use std::io::Write;

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("{} already exists", path.display()))?;
    file.write_all(contents)?;
    Ok(())
}

/// How many names to show per reason before summarizing the rest.
///
/// A long-lived `.codex` accumulates dozens of near-identical scratch files,
/// and listing every one buries the entries a reader actually has to think
/// about. `--json` still carries all of them.
const MAX_LISTED_PER_REASON: usize = 6;

/// Groups skipped entries by reason, because the reason is the part worth
/// reading and the names repeat.
fn print_skips_by_reason(skipped: &[(&String, &String)]) {
    let mut by_reason: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (name, explanation) in skipped {
        by_reason
            .entry(explanation.as_str())
            .or_default()
            .push(name.as_str());
    }

    for (explanation, names) in by_reason {
        println!("  {explanation}:");
        let shown = names.len().min(MAX_LISTED_PER_REASON);
        println!("    {}", names[..shown].join(", "));
        let remaining = names.len() - shown;
        if remaining > 0 {
            println!("    and {remaining} more (--json lists them)");
        }
    }
}

fn print_plan(plan: &ImportPlan) {
    println!("Codex home: {}", plan.source);
    println!("Whisply home: {}", plan.destination);
    println!();

    let mut imported = Vec::new();
    let mut conflicts = Vec::new();
    let mut skipped = Vec::new();
    for entry in &plan.entries {
        match &entry.planned {
            Planned::Import { destination } => imported.push((&entry.name, destination)),
            Planned::Conflict { destination } => conflicts.push((&entry.name, destination)),
            Planned::Skip { explanation, .. } => skipped.push((&entry.name, explanation)),
        }
    }

    let verb = if plan.applied {
        "Imported"
    } else {
        "Would import"
    };
    if imported.is_empty() {
        println!("{verb}: nothing.");
    } else {
        println!("{verb}:");
        for (name, destination) in imported {
            println!("  {name} -> {destination}");
        }
    }

    if !conflicts.is_empty() {
        println!();
        println!("Already present, so left as they are:");
        for (name, destination) in conflicts {
            println!("  {name} -> {destination}");
        }
    }

    println!();
    println!("Not imported:");
    print_skips_by_reason(&skipped);

    println!();
    if plan.applied {
        println!("The Codex home was only read; nothing in it changed.");
    } else {
        println!("This was a preview. Re-run with --apply to copy.");
    }
}

#[cfg(test)]
#[path = "whisply_import_tests.rs"]
mod tests;

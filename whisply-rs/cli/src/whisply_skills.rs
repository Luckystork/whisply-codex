//! Public Whisply skill management.
//!
//! Skill packages are discovered from the managed Whisply home and an
//! explicitly selected project's `.whisply/skills` plus compatible
//! `.agents/skills` directories. Create and import remain explicitly targeted
//! at Whisply-managed roots, while shared local packages stay visible and
//! editable by their selected origin. This module never invokes package
//! scripts or reads a remote package source.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::io::Write;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use clap::Args;
use codex_whisply::HOME_ENV;
use serde::Deserialize;
use whisply_config::CONFIG_TOML_FILE;
use whisply_core::config::edit::ConfigEdit;
use whisply_core::config::edit::ConfigEditsBuilder;
use whisply_core::config::find_codex_home;
use whisply_core_plugins::validate_skill_package_contents;

const MAX_SKILL_NAME_BYTES: usize = 64;
const MAX_SKILL_DESCRIPTION_BYTES: usize = 1_024;
const MAX_SKILL_MARKDOWN_BYTES: usize = 128 * 1024;
const MAX_SKILL_VERSION_LEN: usize = 64;
const MAX_SKILL_REQUIREMENT_LEN: usize = 128;
const MAX_SKILL_METADATA_BYTES: usize = 64 * 1024;
/// Where the runtime loader reads a package's optional additive metadata. Kept
/// byte-identical to the loader's own constants on purpose: reading a different
/// path would let the CLI report requirements the runtime never honors, which
/// is worse than reporting none.
const SKILL_METADATA_DIR: &str = "agents";
const SKILL_METADATA_FILENAME: &str = "openai.yaml";
/// The package's optional additive Whisply metadata. Additive means a package
/// carrying it stays a valid standard package for any other agent; it may
/// therefore only add presentation, never change what the package is.
const WHISPLY_METADATA_FILENAME: &str = "whisply.yaml";
/// The directories a package may cite. A citation outside them is prose or a
/// path on the machine, not a resource this package ships.
const SKILL_RESOURCE_DIRS: [&str; 4] = ["scripts/", "references/", "assets/", "agents/"];
/// Bounds the citation scan on a package that is mostly paths.
const MAX_CHECKED_REFERENCES: usize = 256;
const MAX_SKILL_PACKAGE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_SKILL_FILE_BYTES: u64 = 512 * 1024;
const MAX_SKILL_FILES: usize = 2_000;
const MAX_SKILL_DEPTH: usize = 6;
const SKILL_TEMPLATE_BODY: &str =
    "# Instructions\n\nDescribe when and how Whisply should use this skill.\n";

#[derive(Debug, Args)]
pub(crate) struct SkillsCommand {
    #[command(subcommand)]
    action: SkillsSubcommand,
}

#[derive(Debug, clap::Subcommand)]
enum SkillsSubcommand {
    /// List discovered user, Whisply-project, and compatible project skill packages.
    List(SkillRootArgs),
    /// Search discovered skill packages by name, description, version, or origin.
    Search(SkillSearchArgs),
    /// Show one validated skill's redacted public instructions.
    Show(SkillNameArgs),
    /// Validate one skill or every discovered skill package.
    Validate(SkillValidateArgs),
    /// Create a disabled-by-default template in the managed skill root.
    Create(SkillCreateArgs),
    /// Update a skill description and/or instruction body without launching an editor.
    Edit(SkillEditArgs),
    /// Enable a uniquely discovered skill in managed config.toml.
    Enable(SkillNameArgs),
    /// Disable a uniquely discovered skill in managed config.toml.
    Disable(SkillNameArgs),
    /// Copy a local skill package after validation. Imported scripts remain disabled.
    Import(SkillImportArgs),
}

#[derive(Debug, Args, Clone, Default)]
struct SkillRootArgs {
    /// Explicit project root whose `.whisply/skills` and `.agents/skills` packages should participate.
    #[arg(long, value_name = "DIR")]
    project_root: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct SkillSearchArgs {
    /// Text to match against a package's name, description, version, or origin.
    #[arg(value_name = "QUERY")]
    query: String,

    #[clap(flatten)]
    roots: SkillRootArgs,
}

#[derive(Debug, Args)]
struct SkillNameArgs {
    /// Exact validated skill name.
    #[arg(value_name = "SKILL")]
    name: String,

    #[clap(flatten)]
    roots: SkillRootArgs,
}

#[derive(Debug, Args)]
struct SkillValidateArgs {
    /// Optional exact skill name. Omit to validate every discovered package.
    #[arg(value_name = "SKILL")]
    name: Option<String>,

    #[clap(flatten)]
    roots: SkillRootArgs,
}

#[derive(Debug, Args)]
struct SkillCreateArgs {
    /// Lowercase kebab-case skill name.
    #[arg(value_name = "SKILL")]
    name: String,

    /// Public description shown during discovery (1-1024 bytes).
    #[arg(long, value_name = "TEXT")]
    description: String,

    /// Read instruction body from this local text file rather than using the template.
    #[arg(long, value_name = "FILE")]
    body_file: Option<PathBuf>,

    #[clap(flatten)]
    roots: SkillRootArgs,
}

#[derive(Debug, Args)]
struct SkillEditArgs {
    /// Exact validated skill name.
    #[arg(value_name = "SKILL")]
    name: String,

    /// Replace the public skill description.
    #[arg(long, value_name = "TEXT")]
    description: Option<String>,

    /// Replace the instruction body from this local text file.
    #[arg(long, value_name = "FILE")]
    body_file: Option<PathBuf>,

    #[clap(flatten)]
    roots: SkillRootArgs,
}

#[derive(Debug, Args)]
struct SkillImportArgs {
    /// Local directory containing a validated SKILL.md package.
    #[arg(value_name = "SOURCE")]
    source: PathBuf,

    /// Copy into this explicit project's `.whisply/skills` root instead of the managed user root.
    #[arg(long, value_name = "DIR")]
    project_root: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SkillRootKind {
    Project,
    ProjectAgentsCompatibility,
    User,
}

impl SkillRootKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::ProjectAgentsCompatibility => "project-agents",
            Self::User => "user",
        }
    }
}

#[derive(Clone, Debug)]
struct SkillRoot {
    kind: SkillRootKind,
    path: PathBuf,
}

#[derive(Clone, Debug)]
struct SkillRecord {
    name: String,
    description: String,
    version: Option<String>,
    root_kind: SkillRootKind,
    package_path: PathBuf,
    skill_markdown: PathBuf,
    has_scripts: bool,
    /// From the package's optional additive Whisply metadata. Absent for the
    /// vast majority of packages, which carry none.
    display_name: Option<String>,
    short_description: Option<String>,
}

impl SkillRecord {
    /// Matches the same fields the listing shows.
    ///
    /// Kept to what is on screen on purpose: a result a user cannot explain
    /// from the row in front of them reads as a bug. Searching the instruction
    /// body would match text no surface displays.
    fn matches_search(&self, query: &str) -> bool {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return true;
        }
        [
            self.name.as_str(),
            self.description.as_str(),
            self.version.as_deref().unwrap_or(""),
            self.root_kind.as_str(),
        ]
        .iter()
        .any(|field| field.to_lowercase().contains(&query))
    }
}

#[derive(Clone, Debug)]
struct ParsedSkillMarkdown {
    name: String,
    description: String,
    version: Option<String>,
    body: String,
    frontmatter: String,
}

/// The standard package format permits arbitrary additive frontmatter (such
/// as the nested `metadata` map used by Codex). Whisply reads only identity
/// and version and deliberately leaves the rest untouched.
#[derive(Deserialize)]
struct StandardSkillFrontmatter {
    name: String,
    description: String,
    /// Read as an opaque scalar: packages version themselves with dates and
    /// commit shorthands, and YAML types a bare `1.0` as a float.
    #[serde(default)]
    version: Option<serde_yaml::Value>,
}

/// Reads the tools a package declares it needs from its optional metadata file.
///
/// Reports declarations, not resolutions. Whether the tool is actually
/// installed here is a separate question, and answering it from a declaration
/// alone would be a guess presented as a diagnosis.
///
/// Fails open: this metadata is additive and optional, so an unreadable or
/// malformed file means "nothing declared" rather than an error. Refusing to
/// show a valid skill because a supplementary file is broken would be worse
/// than omitting a line.
fn declared_requirements(package_path: &Path) -> Vec<String> {
    #[derive(Deserialize)]
    struct MetadataFile {
        #[serde(default)]
        dependencies: Option<Dependencies>,
    }
    #[derive(Deserialize)]
    struct Dependencies {
        #[serde(default)]
        tools: Vec<Tool>,
    }
    #[derive(Deserialize)]
    struct Tool {
        #[serde(default)]
        value: Option<String>,
    }

    let metadata_path = package_path
        .join(SKILL_METADATA_DIR)
        .join(SKILL_METADATA_FILENAME);
    let Ok(contents) = read_bounded_utf8(&metadata_path, MAX_SKILL_METADATA_BYTES) else {
        return Vec::new();
    };
    let Ok(parsed) = serde_yaml::from_str::<MetadataFile>(&contents) else {
        return Vec::new();
    };
    let mut seen = BTreeSet::new();
    parsed
        .dependencies
        .map(|dependencies| dependencies.tools)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|tool| {
            let value = tool.value?.trim().to_string();
            (!value.is_empty() && value.chars().count() <= MAX_SKILL_REQUIREMENT_LEN)
                .then_some(value)
        })
        .filter(|value| seen.insert(value.clone()))
        .collect()
}

/// Renders listing and search results identically.
///
/// Shared deliberately: if the two rendered their own rows, the same package
/// could be described one way when listed and another when found, and a user
/// would have no way to tell which answer to believe.
fn print_skill_rows(home: &Path, all: &[SkillRecord], rows: &[&SkillRecord]) -> anyhow::Result<()> {
    let duplicates = duplicate_names(all);
    for record in rows {
        let enabled = skill_enabled(home, &record.name)?.unwrap_or(true);
        let duplicate = duplicates.contains(&record.name);
        println!(
            "{}\t{}\t{}\t{}\t{}{}",
            record.name,
            record.root_kind.as_str(),
            if enabled { "enabled" } else { "disabled" },
            // A package that declares no version must not be reported as
            // though it declared one.
            record.version.as_deref().unwrap_or("-"),
            record.description,
            if duplicate { "\tduplicate" } else { "" }
        );
    }
    Ok(())
}

pub(crate) fn run(command: SkillsCommand) -> anyhow::Result<()> {
    let home = managed_home()?;
    match command.action {
        SkillsSubcommand::List(roots) => {
            let discovery = discover_skills(&home, roots.project_root.as_deref())?;
            let all = discovery.records.iter().collect::<Vec<_>>();
            print_skill_rows(&home, &discovery.records, &all)?;
            discovery.report_failures();
        }
        SkillsSubcommand::Search(args) => {
            let discovery = discover_skills(&home, args.roots.project_root.as_deref())?;
            let matches = discovery
                .records
                .iter()
                .filter(|record| record.matches_search(&args.query))
                .collect::<Vec<_>>();
            if matches.is_empty() {
                // Silence would be indistinguishable from having no skills at
                // all, which is the wrong conclusion to leave a user with.
                println!("No skill matches `{}`.", args.query);
            } else {
                // Duplicate detection stays over every discovered package. A
                // name that collides with one filtered out of these results is
                // still ambiguous for every later command that takes a name.
                print_skill_rows(&home, &discovery.records, &matches)?;
            }
            // Reported even when nothing matched: a package that failed to load
            // may well be the one being searched for.
            discovery.report_failures();
        }
        SkillsSubcommand::Show(args) => {
            let record = find_unique_skill(&home, &args.name, args.roots.project_root.as_deref())?;
            let markdown = read_bounded_utf8(&record.skill_markdown, MAX_SKILL_MARKDOWN_BYTES)?;
            println!("{}", redact_public_text(&markdown));
            // Printed after the instructions rather than folded into the list
            // row: a package can declare several tools, and a listing wide
            // enough to hold them stops being scannable.
            let requirements = declared_requirements(&record.package_path);
            if !requirements.is_empty() {
                println!("Requires: {}", requirements.join(", "));
            }
            // The package's own additive metadata, shown as what it is: how
            // this package asks to be presented, separate from the standard
            // frontmatter every agent reads.
            if let Some(display_name) = record.display_name.as_deref() {
                println!("Presented as: {}", redact_public_text(display_name));
            }
            if let Some(short_description) = record.short_description.as_deref() {
                println!("Summary: {}", redact_public_text(short_description));
            }
        }
        SkillsSubcommand::Validate(args) => {
            let discovery = discover_skills(&home, args.roots.project_root.as_deref())?;
            let records = &discovery.records;
            let duplicates = duplicate_names(records);
            if let Some(name) = args.name {
                validate_skill_name(&name)?;
                let matching = records
                    .iter()
                    .filter(|record| record.name == name)
                    .collect::<Vec<_>>();
                if matching.is_empty() {
                    anyhow::bail!("{}", not_found_error(&name, &discovery));
                }
                if matching.len() > 1 {
                    anyhow::bail!(duplicate_skill_error(&name, &matching));
                }
                let Some(record) = matching.into_iter().next() else {
                    anyhow::bail!("{}", not_found_error(&name, &discovery));
                };
                validate_skill_package_for_save(&record.package_path, record.root_kind)?;
                println!("Validated skill `{name}`.");
            } else {
                // Reporting validity is this command's whole job, so it names
                // every unreadable package rather than stopping at the first.
                // Stopping meant a user fixed one package, reran, and was shown
                // the next one, with no idea how many remained.
                discovery.report_failures();
                // A package that loads but cites a resource it does not ship is
                // exactly what someone runs this command to find out.
                let mut unsound = 0;
                for record in records {
                    if let Err(error) =
                        validate_skill_package_for_save(&record.package_path, record.root_kind)
                    {
                        unsound += 1;
                        eprintln!("{}: {error}", record.package_path.display());
                    }
                }
                if !duplicates.is_empty() {
                    anyhow::bail!("{}", duplicate_summary(&duplicates));
                }
                if !discovery.failures.is_empty() || unsound > 0 {
                    anyhow::bail!(
                        "{} skill package(s) could not be validated.",
                        discovery.failures.len() + unsound
                    );
                }
                println!("Validated {} managed skill package(s).", records.len());
            }
        }
        SkillsSubcommand::Create(args) => {
            validate_skill_name(&args.name)?;
            validate_description(&args.description)?;
            if discover_skills(&home, args.roots.project_root.as_deref())?
                .records
                .iter()
                .any(|record| record.name == args.name)
            {
                anyhow::bail!(
                    "A skill named `{}` already exists in the selected discovery roots.",
                    args.name
                );
            }
            let body = match args.body_file {
                Some(path) => read_bounded_utf8(&path, MAX_SKILL_MARKDOWN_BYTES)?,
                None => SKILL_TEMPLATE_BODY.to_string(),
            };
            let root = managed_write_skill_root(&home, args.roots.project_root.as_deref())?;
            create_skill(&root, &args.name, &args.description, &body)?;
            set_skill_enabled(&home, &args.name, false)?;
            println!(
                "Created disabled skill `{}`. Review it, then run `whisply skills enable {}`.",
                args.name, args.name
            );
        }
        SkillsSubcommand::Edit(args) => {
            if args.description.is_none() && args.body_file.is_none() {
                anyhow::bail!("Provide --description, --body-file, or both when editing a skill.");
            }
            let record = find_unique_skill(&home, &args.name, args.roots.project_root.as_deref())?;
            let existing = parse_skill_markdown(&read_bounded_utf8(
                &record.skill_markdown,
                MAX_SKILL_MARKDOWN_BYTES,
            )?)?;
            let description = args
                .description
                .unwrap_or_else(|| existing.description.clone());
            validate_description(&description)?;
            let body = match args.body_file {
                Some(path) => read_bounded_utf8(&path, MAX_SKILL_MARKDOWN_BYTES)?,
                None => existing.body.clone(),
            };
            let content = render_edited_skill_markdown(&existing, &description, &body)?;
            validate_skill_markdown_content(&content)?;
            atomic_write_private(&record.skill_markdown, content.as_bytes())?;
            // Revalidate the replacement before reporting success. An edit can
            // introduce a citation as easily as a new package can.
            validate_skill_package_for_save(&record.package_path, record.root_kind)?;
            println!("Updated skill `{}`.", record.name);
        }
        SkillsSubcommand::Enable(args) => {
            let record = find_unique_skill(&home, &args.name, args.roots.project_root.as_deref())?;
            set_skill_enabled(&home, &record.name, true)?;
            println!("Enabled skill `{}`.", record.name);
        }
        SkillsSubcommand::Disable(args) => {
            let record = find_unique_skill(&home, &args.name, args.roots.project_root.as_deref())?;
            set_skill_enabled(&home, &record.name, false)?;
            println!("Disabled skill `{}`.", record.name);
        }
        SkillsSubcommand::Import(args) => {
            let root = managed_write_skill_root(&home, args.project_root.as_deref())?;
            import_skill(&home, &root, &args.source, args.project_root.as_deref())?;
        }
    }
    Ok(())
}

/// The managed launcher must select an account-scoped Whisply home. Do not
/// allow management commands to recreate the upstream `~/.codex` fallback.
pub(crate) fn managed_home() -> anyhow::Result<PathBuf> {
    let configured = std::env::var_os(HOME_ENV)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{HOME_ENV} is required for managed Whisply commands. Start Whisply from the installed app."
            )
    })?;
    let configured = PathBuf::from(configured);
    if !configured.is_absolute() {
        anyhow::bail!("{HOME_ENV} must be an absolute managed runtime path.");
    }
    let metadata =
        fs::symlink_metadata(&configured).context("managed Whisply home is unavailable")?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        anyhow::bail!("managed Whisply home must be a real directory.");
    }
    let home = find_codex_home().context("failed to resolve managed Whisply home")?;
    if home.as_path() != configured.as_path() {
        anyhow::bail!("The managed Whisply home does not match the launcher contract.");
    }
    Ok(home.to_path_buf())
}

/// What discovery found, including what it could not read.
///
/// Failures are carried rather than raised. One malformed directory used to
/// abort every skills command, so a single bad package made every other skill
/// unreachable from the terminal -- including the commands a user would run to
/// find and fix it. The app-server path has always reported per-package errors
/// and kept going; this matches it.
#[derive(Debug, Default)]
struct SkillDiscovery {
    records: Vec<SkillRecord>,
    failures: Vec<SkillDiscoveryFailure>,
}

#[derive(Debug)]
struct SkillDiscoveryFailure {
    path: PathBuf,
    reason: String,
}

impl SkillDiscovery {
    /// Writes failures to stderr so they cannot corrupt the tab-separated
    /// listing on stdout that scripts parse.
    fn report_failures(&self) {
        for failure in &self.failures {
            eprintln!("Skipped {}: {}", failure.path.display(), failure.reason);
        }
    }
}

fn discover_skills(home: &Path, project_root: Option<&Path>) -> anyhow::Result<SkillDiscovery> {
    // An explicit `--project-root` that does not resolve stays fatal. The user
    // named it, so silently ignoring it would answer a different question than
    // the one they asked.
    let roots = skill_roots(home, project_root)?;
    let mut discovery = SkillDiscovery::default();
    for root in roots {
        scan_skill_root(&root, &mut discovery);
    }
    discovery.records.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.root_kind.as_str().cmp(right.root_kind.as_str()))
    });
    discovery
        .failures
        .sort_by(|left, right| left.path.cmp(&right.path));
    Ok(discovery)
}

fn skill_roots(home: &Path, project_root: Option<&Path>) -> anyhow::Result<Vec<SkillRoot>> {
    let mut roots = Vec::new();
    if let Some(project_root) = project_root {
        let project_root = fs::canonicalize(project_root)
            .with_context(|| "The explicit project root does not exist or is not accessible.")?;
        ensure_real_directory(&project_root)?;
        roots.push(SkillRoot {
            kind: SkillRootKind::Project,
            path: project_root.join(".whisply").join("skills"),
        });
        roots.push(SkillRoot {
            kind: SkillRootKind::ProjectAgentsCompatibility,
            path: project_root.join(".agents").join("skills"),
        });
    }
    roots.push(SkillRoot {
        kind: SkillRootKind::User,
        path: home.join("skills"),
    });
    Ok(roots)
}

/// Return the root where Whisply may create or import a package. Compatible
/// `.agents/skills` packages participate in discovery and exact edit/show
/// selection, but are never a default write destination.
fn managed_write_skill_root(home: &Path, project_root: Option<&Path>) -> anyhow::Result<SkillRoot> {
    if let Some(project_root) = project_root {
        let project_root = fs::canonicalize(project_root)
            .with_context(|| "The explicit project root does not exist or is not accessible.")?;
        ensure_real_directory(&project_root)?;
        return Ok(SkillRoot {
            kind: SkillRootKind::Project,
            path: project_root.join(".whisply").join("skills"),
        });
    }
    Ok(SkillRoot {
        kind: SkillRootKind::User,
        path: home.join("skills"),
    })
}

/// Scans one root, recording what it cannot read instead of failing the run.
///
/// A root problem is recorded against the root and the remaining roots are
/// still scanned, so an unreadable project directory never hides the user's own
/// skills. Nothing here decides whether a package may be *used*: every action
/// path still resolves through `validate_skill_package`, so skipping a
/// package from a listing is not the same as trusting it.
fn scan_skill_root(root: &SkillRoot, discovery: &mut SkillDiscovery) {
    let mut fail = |reason: String| {
        discovery.failures.push(SkillDiscoveryFailure {
            path: root.path.clone(),
            reason,
        });
    };

    match fs::symlink_metadata(&root.path) {
        // A root that was never created is the ordinary case, not a failure.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => return fail(format!("failed to inspect the skill root: {error}")),
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return fail("the skill root is not a real directory".to_string());
        }
        Ok(_) => {}
    }

    let mut packages = match fs::read_dir(&root.path) {
        Ok(entries) => match entries.collect::<Result<Vec<_>, _>>() {
            Ok(entries) => entries,
            Err(error) => return fail(format!("failed to list the skill root: {error}")),
        },
        Err(error) => return fail(format!("failed to list the skill root: {error}")),
    };
    packages.sort_by_key(std::fs::DirEntry::file_name);
    if packages.len() > MAX_SKILL_FILES {
        return fail("the skill root contains too many package entries".to_string());
    }

    for package in packages {
        let path = package.path();
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) => {
                discovery.failures.push(SkillDiscoveryFailure {
                    path,
                    reason: format!("failed to inspect the package: {error}"),
                });
                continue;
            }
        };
        if metadata.file_type().is_symlink() {
            discovery.failures.push(SkillDiscoveryFailure {
                path,
                reason: "skill package entries may not be symlinks".to_string(),
            });
            continue;
        }
        if !metadata.is_dir() {
            continue;
        }
        match validate_skill_package(&path, root.kind) {
            Ok(record) => discovery.records.push(record),
            Err(error) => discovery.failures.push(SkillDiscoveryFailure {
                path,
                // The chain, not just the outermost context: "contains unsafe
                // material" alone does not tell anyone which file to look at.
                reason: error
                    .chain()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(": "),
            }),
        }
    }
}

fn validate_skill_package(
    package_path: &Path,
    root_kind: SkillRootKind,
) -> anyhow::Result<SkillRecord> {
    ensure_real_directory(package_path)?;
    validate_skill_package_contents(package_path)
        .context("Skill package contains unsafe or credential-like material.")?;
    let skill_markdown = package_path.join("SKILL.md");
    ensure_regular_file(&skill_markdown, MAX_SKILL_MARKDOWN_BYTES as u64)?;
    let parsed = validate_skill_markdown_content(&read_bounded_utf8(
        &skill_markdown,
        MAX_SKILL_MARKDOWN_BYTES,
    )?)?;
    let directory_name = package_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow::anyhow!("Skill package name is not valid UTF-8."))?;
    if directory_name != parsed.name {
        anyhow::bail!("Skill package directory must exactly match frontmatter name.");
    }

    let mut budget = PackageBudget::default();
    inspect_skill_tree(package_path, package_path, 0, &mut budget)?;
    let metadata = whisply_metadata(package_path);
    Ok(SkillRecord {
        name: parsed.name,
        description: parsed.description,
        version: parsed.version,
        root_kind,
        package_path: package_path.to_path_buf(),
        skill_markdown,
        has_scripts: budget.has_scripts,
        display_name: metadata.display_name,
        short_description: metadata.short_description,
    })
}

/// Everything `validate_skill_package` checks, plus the two checks a package
/// only has to survive when someone is writing it.
///
/// The split is deliberate. Discovery runs the first set on every package it
/// finds, and a package that fails discovery disappears from the listing — so
/// a rule added there retroactively hides packages that have been working.
/// These two rules are about a package being *written correctly*, and the
/// moment to enforce them is while the author is standing there: creating,
/// editing, or importing. `whisply skills validate` runs them too, because
/// reporting is that command's whole job, and it names a package rather than
/// hiding it.
fn validate_skill_package_for_save(
    package_path: &Path,
    root_kind: SkillRootKind,
) -> anyhow::Result<SkillRecord> {
    let record = validate_skill_package(package_path, root_kind)?;
    let markdown = read_bounded_utf8(&record.skill_markdown, MAX_SKILL_MARKDOWN_BYTES)?;
    validate_declared_references(package_path, &markdown)?;
    validate_whisply_metadata(package_path)?;
    Ok(record)
}

/// Refuses a package that tells the model to read something it does not ship.
///
/// A missing reference is not a cosmetic error. The instructions say to open
/// `references/style.md`, the model tries, the read fails, and what the person
/// sees is a skill that half works for reasons nothing explains. Catching it at
/// the moment the package is written costs one directory lookup.
fn validate_declared_references(package_path: &Path, markdown: &str) -> anyhow::Result<()> {
    for reference in declared_package_references(markdown) {
        if Path::new(&reference)
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
        {
            anyhow::bail!("SKILL.md cites `{reference}`, which leaves the package directory.");
        }
        let cited = package_path.join(&reference);
        match fs::symlink_metadata(&cited) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                anyhow::bail!("SKILL.md cites `{reference}`, which is a link rather than a file.");
            }
            Ok(_) => {}
            Err(_) => {
                anyhow::bail!("SKILL.md cites `{reference}`, which this package does not contain.")
            }
        }
    }
    Ok(())
}

/// The package-relative paths a `SKILL.md` tells the model to open.
///
/// Only paths under the package's own resource directories count. Anything
/// else in the text is prose, a URL, or a path on the machine, and treating one
/// of those as a broken reference would refuse a package for describing the
/// world accurately. A citation must also start at a boundary, so `myscripts/x`
/// is not read as `scripts/x`.
fn declared_package_references(markdown: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let bytes = markdown.as_bytes();
    for (index, _) in markdown.char_indices() {
        if found.len() >= MAX_CHECKED_REFERENCES {
            break;
        }
        let rest = &markdown[index..];
        let Some(prefix) = SKILL_RESOURCE_DIRS
            .iter()
            .find(|prefix| rest.starts_with(**prefix))
        else {
            continue;
        };
        let preceded_by_path_character = index
            .checked_sub(1)
            .and_then(|previous| bytes.get(previous))
            .is_some_and(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'-' | b'_' | b'~')
            });
        if preceded_by_path_character {
            continue;
        }
        let end = rest
            .find(|character: char| {
                character.is_whitespace()
                    || character.is_control()
                    || matches!(
                        character,
                        '`' | '"'
                            | '\''
                            | '('
                            | ')'
                            | '['
                            | ']'
                            | '<'
                            | '>'
                            | '|'
                            | '*'
                            | ','
                            | ';'
                    )
            })
            .unwrap_or(rest.len());
        let citation = rest[..end].trim_end_matches(['.', ':', '/']);
        // The bare directory name is not a citation of anything readable.
        if citation.len() <= prefix.len() {
            continue;
        }
        found.insert(citation.to_string());
    }
    found
}

/// A package's optional additive Whisply metadata.
///
/// Kept to presentation on purpose. This file may not change what a package is
/// or what it is allowed to do, because a standard package carrying it has to
/// remain the same package for an agent that has never heard of Whisply.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct WhisplyPackageMetadata {
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    short_description: Option<String>,
}

/// Checks the additive metadata against its schema before a package is saved.
///
/// Unknown keys are refused here and only here. While the author is present, a
/// key nothing reads is a mistake worth naming — silently ignoring it is how
/// someone ends up believing a package is configured. Discovery stays lenient
/// for the opposite reason: a package written for a later Whisply must not
/// vanish from an earlier one.
fn validate_whisply_metadata(package_path: &Path) -> anyhow::Result<()> {
    let path = package_path.join(WHISPLY_METADATA_FILENAME);
    let contents = match read_bounded_utf8(&path, MAX_SKILL_METADATA_BYTES) {
        Ok(contents) => contents,
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound) =>
        {
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    let metadata: WhisplyPackageMetadata = serde_yaml::from_str(&contents).with_context(|| {
        format!("{WHISPLY_METADATA_FILENAME} must be a YAML mapping of known Whisply fields.")
    })?;
    if let Some(display_name) = metadata.display_name.as_deref() {
        validate_metadata_text("display_name", display_name, MAX_SKILL_NAME_BYTES)?;
    }
    if let Some(short_description) = metadata.short_description.as_deref() {
        validate_metadata_text(
            "short_description",
            short_description,
            MAX_SKILL_DESCRIPTION_BYTES,
        )?;
    }
    Ok(())
}

fn validate_metadata_text(field: &str, value: &str, max_bytes: usize) -> anyhow::Result<()> {
    if value.trim().is_empty() {
        anyhow::bail!("{WHISPLY_METADATA_FILENAME} `{field}` must not be empty.");
    }
    if value.len() > max_bytes {
        anyhow::bail!("{WHISPLY_METADATA_FILENAME} `{field}` exceeds {max_bytes} bytes.");
    }
    if value.chars().any(char::is_control) {
        anyhow::bail!("{WHISPLY_METADATA_FILENAME} `{field}` must be a single line of text.");
    }
    Ok(())
}

/// Reads the additive metadata for display.
///
/// Fails open for the same reason `declared_requirements` does: this file is
/// optional and supplementary, so an unreadable one means "nothing declared"
/// rather than hiding a valid package. Anything malformed here was already
/// refused when the package was written.
fn whisply_metadata(package_path: &Path) -> WhisplyPackageMetadata {
    read_bounded_utf8(
        &package_path.join(WHISPLY_METADATA_FILENAME),
        MAX_SKILL_METADATA_BYTES,
    )
    .ok()
    .and_then(|contents| serde_yaml::from_str(&contents).ok())
    .unwrap_or_default()
}

#[derive(Default)]
struct PackageBudget {
    files: usize,
    bytes: u64,
    has_scripts: bool,
}

fn inspect_skill_tree(
    package_root: &Path,
    current: &Path,
    depth: usize,
    budget: &mut PackageBudget,
) -> anyhow::Result<()> {
    if depth > MAX_SKILL_DEPTH {
        anyhow::bail!("Skill package exceeds the maximum directory depth.");
    }
    let entries = fs::read_dir(current)?.collect::<Result<Vec<_>, _>>()?;
    for entry in entries {
        let path = entry.path();
        let relative = path
            .strip_prefix(package_root)
            .map_err(|_| anyhow::anyhow!("Skill package path escaped its root."))?;
        validate_relative_path(relative)?;
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            anyhow::bail!("Skill packages may not contain symlinks.");
        }
        let first = relative.components().next().and_then(|part| match part {
            Component::Normal(value) => value.to_str(),
            _ => None,
        });
        let allowed = matches!(
            first,
            Some("SKILL.md" | "whisply.yaml" | "scripts" | "references" | "assets" | "agents")
        );
        if !allowed {
            anyhow::bail!("Skill package contains an unsupported top-level entry.");
        }
        if first == Some("agents") && relative.components().count() > 2 {
            anyhow::bail!("Skill packages support only agents/openai.yaml metadata.");
        }
        if first == Some("agents")
            && relative.components().count() == 2
            && relative.file_name().and_then(|value| value.to_str()) != Some("openai.yaml")
        {
            anyhow::bail!("Skill packages support only agents/openai.yaml metadata.");
        }
        if metadata.is_dir() {
            inspect_skill_tree(package_root, &path, depth + 1, budget)?;
            continue;
        }
        if !metadata.is_file() {
            anyhow::bail!("Skill package contains an unsupported special file.");
        }
        budget.files += 1;
        budget.bytes = budget.bytes.saturating_add(metadata.len());
        if budget.files > MAX_SKILL_FILES || budget.bytes > MAX_SKILL_PACKAGE_BYTES {
            anyhow::bail!("Skill package exceeds its bounded file or byte limit.");
        }
        if metadata.len() > MAX_SKILL_FILE_BYTES {
            anyhow::bail!("Skill package contains a file that exceeds its size limit.");
        }
        if first == Some("scripts") {
            budget.has_scripts = true;
        }
        if matches!(first, Some("SKILL.md" | "whisply.yaml" | "agents"))
            && line_has_sensitive_material(&read_bounded_utf8(
                &path,
                MAX_SKILL_FILE_BYTES as usize,
            )?)
        {
            anyhow::bail!("Skill package metadata appears to contain credential material.");
        }
    }
    Ok(())
}

fn parse_skill_markdown(markdown: &str) -> anyhow::Result<ParsedSkillMarkdown> {
    if markdown.contains('\0') || !markdown.starts_with("---\n") {
        anyhow::bail!("SKILL.md must begin with a UTF-8 YAML frontmatter delimiter.");
    }
    let mut lines = markdown.split_inclusive('\n');
    let _opening = lines.next();
    let mut frontmatter = Vec::new();
    let mut body = String::new();
    let mut closed = false;
    for line in lines {
        if !closed && line.trim_end_matches(['\r', '\n']) == "---" {
            closed = true;
            continue;
        }
        if closed {
            body.push_str(line);
        } else {
            frontmatter.push(line);
        }
    }
    if !closed {
        anyhow::bail!("SKILL.md frontmatter is missing its closing delimiter.");
    }
    let frontmatter = frontmatter.concat();
    let standard: StandardSkillFrontmatter = serde_yaml::from_str(&frontmatter)
        .context("SKILL.md frontmatter must be valid YAML with name and description.")?;
    validate_skill_name(&standard.name)?;
    validate_description(&standard.description)?;
    if body.trim().is_empty() {
        anyhow::bail!("SKILL.md must include a non-empty instruction body.");
    }
    Ok(ParsedSkillMarkdown {
        name: standard.name,
        description: standard.description,
        version: standard.version.as_ref().and_then(scalar_to_version),
        body,
        frontmatter,
    })
}

/// A declared version is reported, not validated. Refusing an unfamiliar
/// version string would make a package unusable over metadata that affects
/// nothing but presentation.
fn scalar_to_version(value: &serde_yaml::Value) -> Option<String> {
    let raw = match value {
        serde_yaml::Value::String(value) => value.clone(),
        serde_yaml::Value::Number(value) => value.to_string(),
        serde_yaml::Value::Bool(value) => value.to_string(),
        _ => return None,
    };
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    (!collapsed.is_empty() && collapsed.chars().count() <= MAX_SKILL_VERSION_LEN)
        .then_some(collapsed)
}

fn validate_skill_markdown_content(markdown: &str) -> anyhow::Result<ParsedSkillMarkdown> {
    let parsed = parse_skill_markdown(markdown)?;
    if line_has_sensitive_material(markdown) {
        anyhow::bail!("SKILL.md may not contain credential-like material.");
    }
    Ok(parsed)
}

fn render_edited_skill_markdown(
    existing: &ParsedSkillMarkdown,
    description: &str,
    body: &str,
) -> anyhow::Result<String> {
    let encoded_description = serde_json::to_string(description)
        .context("Could not encode the updated skill description.")?;
    let mut frontmatter = String::new();
    let mut replaced = false;
    for line in existing.frontmatter.split_inclusive('\n') {
        if line.starts_with("description:") {
            frontmatter.push_str("description: ");
            frontmatter.push_str(&encoded_description);
            frontmatter.push('\n');
            replaced = true;
        } else {
            frontmatter.push_str(line);
        }
    }
    if !replaced {
        anyhow::bail!("SKILL.md frontmatter must include a top-level description.");
    }
    if !frontmatter.ends_with('\n') {
        frontmatter.push('\n');
    }
    Ok(format!("---\n{frontmatter}---\n{body}"))
}

fn render_skill_markdown(name: &str, description: &str, body: &str) -> String {
    format!(
        "---\nname: {name}\ndescription: {description}\n---\n{}",
        body.trim_end()
    ) + "\n"
}

fn find_unique_skill(
    home: &Path,
    name: &str,
    project_root: Option<&Path>,
) -> anyhow::Result<SkillRecord> {
    validate_skill_name(name)?;
    let discovery = discover_skills(home, project_root)?;
    let matching = discovery
        .records
        .iter()
        .filter(|record| record.name == name)
        .collect::<Vec<_>>();
    if matching.len() > 1 {
        anyhow::bail!(duplicate_skill_error(name, &matching));
    }
    match matching.into_iter().next() {
        Some(record) => Ok(record.clone()),
        None => anyhow::bail!("{}", not_found_error(name, &discovery)),
    }
}

/// Explains a miss without hiding that some packages were unreadable.
///
/// A bare "no skill named X was found" is actively misleading when X is the
/// package that failed to load: it says the skill does not exist when in fact
/// it exists and is broken, which sends the user looking in the wrong place.
fn not_found_error(name: &str, discovery: &SkillDiscovery) -> String {
    let mut message = format!("No skill named `{name}` was found.");
    if !discovery.failures.is_empty() {
        message.push_str(&format!(
            " {} package(s) could not be read and were skipped; run `whisply skills validate` to see why.",
            discovery.failures.len()
        ));
    }
    message
}

fn duplicate_names(records: &[SkillRecord]) -> BTreeSet<String> {
    let mut counts = BTreeMap::new();
    for record in records {
        *counts.entry(record.name.clone()).or_insert(0_usize) += 1;
    }
    counts
        .into_iter()
        .filter_map(|(name, count)| (count > 1).then_some(name))
        .collect()
}

fn duplicate_skill_error(name: &str, records: &[&SkillRecord]) -> String {
    let origins = records
        .iter()
        .map(|record| record.root_kind.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "Skill `{name}` is duplicated across discovery roots ({origins}); choose or remove one package before changing it."
    )
}

fn duplicate_summary(duplicates: &BTreeSet<String>) -> String {
    format!(
        "Duplicate managed skill names are not allowed: {}.",
        duplicates.iter().cloned().collect::<Vec<_>>().join(", ")
    )
}

fn create_skill(root: &SkillRoot, name: &str, description: &str, body: &str) -> anyhow::Result<()> {
    ensure_skill_root_exists(&root.path)?;
    let content = render_skill_markdown(name, description, body);
    validate_skill_markdown_content(&content)?;
    let package = root.path.join(name);
    match fs::symlink_metadata(&package) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Ok(_) => {
            anyhow::bail!("A skill package named `{name}` already exists in this managed root.")
        }
        Err(error) => {
            return Err(error).with_context(|| format!("Failed to inspect {}.", package.display()));
        }
    }
    fs::create_dir(&package)?;
    set_private_directory_mode(&package)?;
    let skill_markdown = package.join("SKILL.md");
    if let Err(error) = write_new_private(&skill_markdown, content.as_bytes()) {
        let _ = fs::remove_dir(&package);
        return Err(error);
    }
    validate_skill_package_for_save(&package, root.kind)?;
    Ok(())
}

fn import_skill(
    home: &Path,
    root: &SkillRoot,
    source: &Path,
    project_root: Option<&Path>,
) -> anyhow::Result<()> {
    let source_metadata = fs::symlink_metadata(source)
        .with_context(|| "The skill import source does not exist or is not accessible.")?;
    if source_metadata.file_type().is_symlink() || !source_metadata.is_dir() {
        anyhow::bail!(
            "The skill import source must be a real directory, not a link or special file."
        );
    }
    let source = fs::canonicalize(source)
        .with_context(|| "The skill import source does not exist or is not accessible.")?;
    // Judged at full strength before anything is copied: a package that cites
    // resources it does not ship is broken wherever it lands.
    let source_record = validate_skill_package_for_save(&source, root.kind)?;
    if discover_skills(home, project_root)?
        .records
        .iter()
        .any(|record| record.name == source_record.name)
    {
        anyhow::bail!(
            "A managed skill named `{}` already exists in the selected discovery roots.",
            source_record.name
        );
    }
    ensure_skill_root_exists(&root.path)?;
    let destination = root.path.join(&source_record.name);
    if destination.exists() {
        anyhow::bail!(
            "A skill package named `{}` already exists in this managed root.",
            source_record.name
        );
    }
    copy_skill_tree(&source, &destination)?;
    validate_skill_package_for_save(&destination, root.kind)?;
    set_skill_enabled(home, &source_record.name, false)?;
    let script_notice = if source_record.has_scripts {
        " It contains scripts and remains disabled until explicitly enabled."
    } else {
        " It remains disabled until explicitly enabled."
    };
    println!("Imported skill `{}`.{script_notice}", source_record.name);
    Ok(())
}

fn copy_skill_tree(source: &Path, destination: &Path) -> anyhow::Result<()> {
    ensure_real_directory(source)?;
    fs::create_dir(destination)?;
    set_private_directory_mode(destination)?;
    let result = copy_skill_tree_contents(source, destination, 0);
    if result.is_err() {
        let _ = fs::remove_dir_all(destination);
    }
    result
}

fn copy_skill_tree_contents(source: &Path, destination: &Path, depth: usize) -> anyhow::Result<()> {
    if depth > MAX_SKILL_DEPTH {
        anyhow::bail!("Skill import exceeds the maximum directory depth.");
    }
    for entry in fs::read_dir(source)?.collect::<Result<Vec<_>, _>>()? {
        let source_path = entry.path();
        let name = entry.file_name();
        let destination_path = destination.join(name);
        let metadata = fs::symlink_metadata(&source_path)?;
        if metadata.file_type().is_symlink() {
            anyhow::bail!("Skill import source may not contain symlinks.");
        }
        if metadata.is_dir() {
            fs::create_dir(&destination_path)?;
            set_private_directory_mode(&destination_path)?;
            copy_skill_tree_contents(&source_path, &destination_path, depth + 1)?;
        } else if metadata.is_file() {
            let mut input = open_existing_regular_file_no_follow(&source_path)?;
            let mut output = private_new_file(&destination_path)?;
            std::io::copy(&mut input, &mut output)?;
            output.sync_all()?;
        } else {
            anyhow::bail!("Skill import source contains an unsupported special file.");
        }
    }
    Ok(())
}

fn skill_enabled(home: &Path, name: &str) -> anyhow::Result<Option<bool>> {
    let config = home.join(CONFIG_TOML_FILE);
    let contents = match read_bounded_utf8(&config, 256 * 1024) {
        Ok(contents) => contents,
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    let config: toml::Value =
        toml::from_str(&contents).context("Managed config.toml is invalid.")?;
    let enabled = config
        .get("skills")
        .and_then(toml::Value::as_table)
        .and_then(|skills| skills.get("config"))
        .and_then(toml::Value::as_array)
        .and_then(|entries| {
            entries.iter().find_map(|entry| {
                let table = entry.as_table()?;
                (table.get("name").and_then(toml::Value::as_str) == Some(name))
                    .then(|| table.get("enabled").and_then(toml::Value::as_bool))
                    .flatten()
            })
        });
    Ok(enabled)
}

fn set_skill_enabled(home: &Path, name: &str, enabled: bool) -> anyhow::Result<()> {
    let config_path = managed_skill_config_path(home)?;
    ConfigEditsBuilder::for_config_path(&config_path)
        .with_edits([ConfigEdit::SetSkillConfigByName {
            name: name.to_string(),
            enabled,
        }])
        .apply_blocking()
        .context("Failed to persist the managed skill setting.")
}

fn validate_skill_name(name: &str) -> anyhow::Result<()> {
    if name.is_empty()
        || name.len() > MAX_SKILL_NAME_BYTES
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        || name.starts_with('-')
        || name.ends_with('-')
        || name.contains("--")
    {
        anyhow::bail!(
            "Skill names must be lowercase kebab-case and at most {MAX_SKILL_NAME_BYTES} bytes."
        );
    }
    Ok(())
}

fn validate_description(description: &str) -> anyhow::Result<()> {
    if description.is_empty()
        || description.len() > MAX_SKILL_DESCRIPTION_BYTES
        || description.chars().any(char::is_control)
        || line_has_sensitive_material(description)
    {
        anyhow::bail!(
            "Skill descriptions must be a non-sensitive single line of at most {MAX_SKILL_DESCRIPTION_BYTES} bytes."
        );
    }
    Ok(())
}

fn validate_relative_path(path: &Path) -> anyhow::Result<()> {
    if path
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        anyhow::bail!("Skill package contains an unsafe relative path.");
    }
    Ok(())
}

fn ensure_skill_root_exists(path: &Path) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Managed skill root has no parent directory."))?;
    match fs::symlink_metadata(parent) {
        Ok(metadata) if !metadata.file_type().is_symlink() && metadata.is_dir() => {}
        Ok(_) => anyhow::bail!("Managed skill root parent must be a real directory."),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let grandparent = parent.parent().ok_or_else(|| {
                anyhow::anyhow!("Managed skill root parent has no trusted base directory.")
            })?;
            ensure_real_directory(grandparent)?;
            if parent.file_name().and_then(|name| name.to_str()) != Some(".whisply") {
                anyhow::bail!("Refusing to create an unexpected managed skill root parent.");
            }
            fs::create_dir(parent)?;
            set_private_directory_mode(parent)?;
        }
        Err(error) => {
            return Err(error).with_context(|| format!("Failed to inspect {}.", parent.display()));
        }
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.file_type().is_symlink() && metadata.is_dir() => {}
        Ok(_) => anyhow::bail!("Managed skill root must be a real directory."),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(path)?;
            set_private_directory_mode(path)?;
        }
        Err(error) => {
            return Err(error).with_context(|| format!("Failed to inspect {}.", path.display()));
        }
    }
    ensure_real_directory(path)
}

fn managed_skill_config_path(home: &Path) -> anyhow::Result<PathBuf> {
    ensure_real_directory(home)?;
    let path = home.join(CONFIG_TOML_FILE);
    match fs::symlink_metadata(&path) {
        Ok(metadata) if !metadata.file_type().is_symlink() && metadata.is_file() => Ok(path),
        Ok(_) => {
            anyhow::bail!("Managed config.toml must be a regular file, not a link or special file.")
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(path),
        Err(error) => Err(error).with_context(|| "Failed to inspect managed config.toml."),
    }
}

fn ensure_real_directory(path: &Path) -> anyhow::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        anyhow::bail!("Expected a real directory, not a link or special file.");
    }
    Ok(())
}

fn ensure_regular_file(path: &Path, max_bytes: u64) -> anyhow::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > max_bytes {
        anyhow::bail!("Expected a bounded regular file, not a link or special file.");
    }
    Ok(())
}

fn read_bounded_utf8(path: &Path, max_bytes: usize) -> anyhow::Result<String> {
    ensure_regular_file(path, max_bytes as u64)?;
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(path)?;
    let mut bytes = Vec::with_capacity(max_bytes.min(8 * 1024));
    Read::by_ref(&mut file)
        .take((max_bytes + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > max_bytes {
        anyhow::bail!("File exceeds its permitted size.");
    }
    String::from_utf8(bytes).context("Expected UTF-8 text.")
}

fn open_existing_regular_file_no_follow(path: &Path) -> anyhow::Result<fs::File> {
    ensure_regular_file(path, MAX_SKILL_FILE_BYTES as u64)?;
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    options
        .open(path)
        .with_context(|| format!("Failed to read {}.", path.display()))
}

fn line_has_sensitive_material(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    [
        "authorization:",
        "bearer ",
        "set-cookie:",
        "cookie:",
        "api_key",
        "api-key",
        "access_token",
        "refresh_token",
        "client_secret",
        "password=",
        "token=",
        "secret=",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
        || value.split_whitespace().any(|word| {
            word.starts_with("sk-")
                || word.starts_with("ghp_")
                || word.starts_with("xox")
                || word.starts_with("eyJ")
        })
}

fn redact_public_text(value: &str) -> String {
    value
        .lines()
        .map(|line| {
            if line_has_sensitive_material(line) {
                "[REDACTED SENSITIVE CONTENT]".to_string()
            } else {
                line.chars()
                    .map(|character| {
                        if character.is_control() {
                            '\u{fffd}'
                        } else {
                            character
                        }
                    })
                    .collect()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn write_new_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let mut file = private_new_file(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn private_new_file(path: &Path) -> anyhow::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    options
        .open(path)
        .with_context(|| format!("Failed to create {}.", path.display()))
}

fn atomic_write_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    ensure_regular_file(path, MAX_SKILL_MARKDOWN_BYTES as u64)?;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Skill file has no parent directory."))?;
    ensure_real_directory(parent)?;
    let temporary = parent.join(format!(".skill-edit-{}", std::process::id()));
    let mut temporary = temporary;
    let mut suffix = 0_u32;
    while temporary.exists() {
        suffix = suffix.saturating_add(1);
        if suffix > 100 {
            anyhow::bail!("Could not reserve a private temporary skill file.");
        }
        temporary = parent.join(format!(".skill-edit-{}-{suffix}", std::process::id()));
    }
    write_new_private(&temporary, bytes)?;
    fs::rename(&temporary, path)?;
    Ok(())
}

fn set_private_directory_mode(path: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package_with_metadata(yaml: Option<&str>) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("temp dir");
        if let Some(yaml) = yaml {
            let agents = dir.path().join(SKILL_METADATA_DIR);
            fs::create_dir_all(&agents).expect("metadata dir");
            fs::write(agents.join(SKILL_METADATA_FILENAME), yaml).expect("metadata file");
        }
        dir
    }

    /// The path must stay identical to the runtime loader's. Reading anywhere
    /// else would report requirements the runtime never honors.
    #[test]
    fn requirements_are_read_from_the_path_the_runtime_reads() {
        let package = package_with_metadata(Some(
            "dependencies:\n  tools:\n    - type: mcp\n      value: brave-search\n",
        ));
        assert_eq!(
            declared_requirements(package.path()),
            vec!["brave-search".to_string()]
        );
    }

    /// A duplicate or blank declaration must not become noise in a line the
    /// user reads at a glance.
    #[test]
    fn requirements_drop_duplicates_and_blanks() {
        let package = package_with_metadata(Some(concat!(
            "dependencies:\n  tools:\n",
            "    - type: mcp\n      value: brave-search\n",
            "    - type: mcp\n      value: brave-search\n",
            "    - type: mcp\n      value: '   '\n",
            "    - type: mcp\n      value: fetch\n",
        )));
        assert_eq!(
            declared_requirements(package.path()),
            vec!["brave-search".to_string(), "fetch".to_string()]
        );
    }

    /// This metadata is additive and optional. Refusing to describe a valid
    /// skill because a supplementary file is missing or broken would be worse
    /// than omitting one line.
    #[test]
    fn a_missing_or_broken_metadata_file_reports_nothing_rather_than_failing() {
        assert!(declared_requirements(package_with_metadata(None).path()).is_empty());
        assert!(
            declared_requirements(package_with_metadata(Some("dependencies: [oh no\n")).path())
                .is_empty()
        );
        assert!(
            declared_requirements(package_with_metadata(Some("policy:\n  x: 1\n")).path())
                .is_empty()
        );
    }

    fn record(name: &str, description: &str, version: Option<&str>) -> SkillRecord {
        SkillRecord {
            name: name.to_string(),
            description: description.to_string(),
            version: version.map(str::to_string),
            root_kind: SkillRootKind::User,
            package_path: PathBuf::from("/tmp").join(name),
            skill_markdown: PathBuf::from("/tmp").join(name).join("SKILL.md"),
            has_scripts: false,
            display_name: None,
            short_description: None,
        }
    }

    /// Search must find a package by anything the listing shows, or a user who
    /// can see a row has no reliable way to search for it.
    #[test]
    fn search_matches_every_field_the_listing_displays() {
        let skill = record(
            "db-migrate",
            "Runs database migrations.",
            Some("2026-01-05"),
        );
        for query in ["db-migrate", "database", "2026-01-05", "user"] {
            assert!(skill.matches_search(query), "should match {query:?}");
        }
    }

    /// Matching what is not on screen would produce results the user cannot
    /// explain from the row in front of them.
    #[test]
    fn search_does_not_match_text_the_listing_never_shows() {
        let skill = record(
            "db-migrate",
            "Runs database migrations.",
            Some("2026-01-05"),
        );
        assert!(!skill.matches_search("/tmp/db-migrate"));
        assert!(!skill.matches_search("SKILL.md"));
    }

    #[test]
    fn search_ignores_case_and_surrounding_space() {
        let skill = record("db-migrate", "Runs database migrations.", None);
        assert!(skill.matches_search("  DATABASE  "));
    }

    /// An unversioned package must not be found by searching for the placeholder
    /// the listing prints in its version column. Named without a hyphen so a
    /// match could only have come from the placeholder.
    #[test]
    fn search_does_not_match_the_absent_version_placeholder() {
        let skill = record("notetaker", "Captures meeting notes.", None);
        assert!(!skill.matches_search("-"));
    }

    /// An empty query is a request to see everything, not to see nothing.
    #[test]
    fn an_empty_query_matches_every_package() {
        let skill = record("note-taker", "Captures meeting notes.", None);
        assert!(skill.matches_search(""));
        assert!(skill.matches_search("   "));
    }

    #[test]
    fn parses_valid_template_and_rejects_unsafe_names() -> anyhow::Result<()> {
        let parsed = parse_skill_markdown(&render_skill_markdown(
            "release-notes",
            "Writes release notes.",
            "# Instructions\n\nUse verified facts.\n",
        ))?;
        assert_eq!(parsed.name, "release-notes");
        assert_eq!(parsed.description, "Writes release notes.");
        assert!(validate_skill_name("ReleaseNotes").is_err());
        assert!(validate_skill_name("../escape").is_err());
        Ok(())
    }

    #[test]
    fn redacts_sensitive_show_output() {
        assert_eq!(
            redact_public_text("safe\nAuthorization: Bearer private"),
            "safe\n[REDACTED SENSITIVE CONTENT]"
        );
        assert_eq!(redact_public_text("safe\u{1b}[2J"), "safe�[2J");
    }

    #[test]
    fn rejects_secret_material_before_a_skill_is_written() {
        assert!(
            validate_skill_markdown_content(&render_skill_markdown(
                "release-notes",
                "Writes release notes.",
                "Authorization: Bearer private",
            ))
            .is_err()
        );
    }

    #[test]
    fn standard_workspace_frontmatter_is_preserved_when_editing() -> anyhow::Result<()> {
        let original = r#"---
name: compatible-skill
description: "Initial description."
metadata:
  short-description: "A standard Codex skill"
  maintainer: "Example team"
license: MIT
compatibility: Codex, Claude, Cursor
---
# Original body
"#;
        let parsed = validate_skill_markdown_content(original)?;
        let edited =
            render_edited_skill_markdown(&parsed, "Updated description.", "# Updated body\n")?;

        let reparsed = validate_skill_markdown_content(&edited)?;
        assert_eq!(reparsed.name, "compatible-skill");
        assert_eq!(reparsed.description, "Updated description.");
        assert_eq!(reparsed.body, "# Updated body\n");
        assert!(edited.contains("short-description: \"A standard Codex skill\""));
        assert!(edited.contains("maintainer: \"Example team\""));
        assert!(edited.contains("license: MIT"));
        assert!(edited.contains("compatibility: Codex, Claude, Cursor"));
        Ok(())
    }

    #[test]
    fn standard_workspace_package_accepts_optional_resources_and_agents_metadata()
    -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let package = root.path().join("compatible-skill");
        fs::create_dir_all(package.join("agents"))?;
        fs::create_dir_all(package.join("scripts"))?;
        fs::create_dir_all(package.join("references"))?;
        fs::create_dir_all(package.join("assets"))?;
        write_new_private(
            &package.join("SKILL.md"),
            br#"---
name: compatible-skill
description: "A compatible package."
metadata:
  short-description: "Package metadata"
---
# Instructions
"#,
        )?;
        write_new_private(
            &package.join("whisply.yaml"),
            b"display_name: Compatible Skill\nshort_description: A compatible package.\n",
        )?;
        write_new_private(
            &package.join("agents/openai.yaml"),
            b"display_name: Compatible Skill\n",
        )?;
        write_new_private(&package.join("scripts/run.sh"), b"#!/bin/sh\nexit 0\n")?;
        write_new_private(&package.join("references/guide.md"), b"Use this guide.\n")?;
        write_new_private(&package.join("assets/icon.txt"), b"icon\n")?;

        let record = validate_skill_package_for_save(&package, SkillRootKind::User)?;
        assert_eq!(record.name, "compatible-skill");
        assert!(record.has_scripts);
        // The additive metadata is read, not merely tolerated.
        assert_eq!(record.display_name.as_deref(), Some("Compatible Skill"));
        assert_eq!(
            record.short_description.as_deref(),
            Some("A compatible package.")
        );
        Ok(())
    }

    fn package_citing(root: &Path, body: &str) -> anyhow::Result<PathBuf> {
        let package = root.join("cited-skill");
        fs::create_dir_all(&package)?;
        write_new_private(
            &package.join("SKILL.md"),
            render_skill_markdown("cited-skill", "Cites a resource.", body).as_bytes(),
        )?;
        Ok(package)
    }

    /// A missing reference is not cosmetic. The instructions say to open
    /// `references/style.md`, the read fails mid-turn, and what the person sees
    /// is a skill that half works for reasons nothing explains.
    #[test]
    fn a_package_that_cites_a_resource_it_does_not_ship_is_refused_before_it_is_saved()
    -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let package = package_citing(
            root.path(),
            "# Instructions\n\nRead [the style sheet](references/style.md) first.\n",
        )?;

        let error = validate_skill_package_for_save(&package, SkillRootKind::User)
            .expect_err("a citation with nothing behind it is a broken package");
        let message = error.to_string();
        assert!(message.contains("references/style.md"), "{message}");
        assert!(message.contains("does not contain"), "{message}");

        write_new_private(&package.join("references/style.md"), b"Style.\n").or_else(|_| {
            fs::create_dir_all(package.join("references"))?;
            write_new_private(&package.join("references/style.md"), b"Style.\n")
        })?;
        validate_skill_package_for_save(&package, SkillRootKind::User)?;
        Ok(())
    }

    /// Discovery runs the weaker check on purpose. A rule added there would
    /// retroactively hide packages that have been working, and a package with
    /// one broken citation is still a package someone can read and fix.
    #[test]
    fn a_broken_citation_does_not_hide_the_package_from_discovery() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let package = package_citing(
            root.path(),
            "# Instructions\n\nRead references/style.md first.\n",
        )?;

        let record = validate_skill_package(&package, SkillRootKind::User)?;
        assert_eq!(record.name, "cited-skill");
        assert!(validate_skill_package_for_save(&package, SkillRootKind::User).is_err());
        Ok(())
    }

    #[test]
    fn a_citation_that_leaves_the_package_is_refused() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let package = package_citing(
            root.path(),
            "# Instructions\n\nRead references/../../secrets.md first.\n",
        )?;

        let error = validate_skill_package_for_save(&package, SkillRootKind::User)
            .expect_err("a citation may not walk out of the package");
        assert!(error.to_string().contains("leaves the package"), "{error}");
        Ok(())
    }

    /// Refusing a package for describing the world accurately would be worse
    /// than missing a citation. Only paths under the package's own resource
    /// directories, starting at a boundary, are treated as citations.
    #[test]
    fn prose_and_paths_on_the_machine_are_not_read_as_citations() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let package = package_citing(
            root.path(),
            "# Instructions\n\n\
             Run /usr/local/scripts/deploy.sh, see https://example.com/assets/logo.png,\n\
             read myscripts/notes.md, and keep the scripts directory tidy.\n",
        )?;

        validate_skill_package_for_save(&package, SkillRootKind::User)?;
        Ok(())
    }

    #[test]
    fn whisply_metadata_is_checked_against_its_schema_before_a_package_is_saved()
    -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let package = package_citing(root.path(), SKILL_TEMPLATE_BODY)?;

        write_new_private(&package.join("whisply.yaml"), b"dsiplay_name: Typo\n")?;
        let error = validate_skill_package_for_save(&package, SkillRootKind::User)
            .expect_err("a key nothing reads is a mistake worth naming");
        assert!(error.to_string().contains("whisply.yaml"), "{error}");

        fs::remove_file(package.join("whisply.yaml"))?;
        write_new_private(&package.join("whisply.yaml"), b"display_name: \"  \"\n")?;
        let error = validate_skill_package_for_save(&package, SkillRootKind::User)
            .expect_err("an empty presentation name presents nothing");
        assert!(error.to_string().contains("display_name"), "{error}");
        Ok(())
    }

    /// The other side of refusing unknown keys while the author is present: a
    /// package written for a later Whisply must not vanish from an earlier one.
    #[test]
    fn a_package_written_for_a_later_whisply_still_loads() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let package = package_citing(root.path(), SKILL_TEMPLATE_BODY)?;
        write_new_private(
            &package.join("whisply.yaml"),
            b"display_name: Future\nsomething_new: true\n",
        )?;

        let record = validate_skill_package(&package, SkillRootKind::User)?;
        assert_eq!(record.name, "cited-skill");
        // Nothing is invented from a file this version cannot fully read.
        assert_eq!(record.display_name, None);
        Ok(())
    }

    #[test]
    fn project_root_precedes_user_root_and_duplicates_are_visible() -> anyhow::Result<()> {
        let home = tempfile::tempdir()?;
        let project = tempfile::tempdir()?;
        let user_root = SkillRoot {
            kind: SkillRootKind::User,
            path: home.path().join("skills"),
        };
        let project_root = SkillRoot {
            kind: SkillRootKind::Project,
            path: project.path().join(".whisply").join("skills"),
        };
        create_skill(&user_root, "shared", "User package.", SKILL_TEMPLATE_BODY)?;
        create_skill(
            &project_root,
            "shared",
            "Project package.",
            SKILL_TEMPLATE_BODY,
        )?;
        let records = discover_skills(home.path(), Some(project.path()))?.records;
        assert_eq!(records[0].root_kind, SkillRootKind::Project);
        assert_eq!(
            duplicate_names(&records),
            BTreeSet::from(["shared".to_string()])
        );
        Ok(())
    }

    /// Writes a directory that looks like a package but cannot be read as one.
    fn write_broken_package(root: &Path, name: &str) -> anyhow::Result<PathBuf> {
        let package = root.join(name);
        fs::create_dir_all(&package)?;
        write_new_private(&package.join("SKILL.md"), b"no frontmatter here")?;
        Ok(package)
    }

    #[test]
    fn one_unreadable_package_does_not_hide_the_readable_ones() -> anyhow::Result<()> {
        let home = tempfile::tempdir()?;
        let user_root = SkillRoot {
            kind: SkillRootKind::User,
            path: home.path().join("skills"),
        };
        create_skill(
            &user_root,
            "intact",
            "A readable package.",
            SKILL_TEMPLATE_BODY,
        )?;
        let broken = write_broken_package(&user_root.path, "unreadable")?;

        let discovery = discover_skills(home.path(), None)?;

        assert_eq!(
            discovery
                .records
                .iter()
                .map(|record| record.name.as_str())
                .collect::<Vec<_>>(),
            vec!["intact"]
        );
        assert_eq!(discovery.failures.len(), 1);
        assert_eq!(discovery.failures[0].path, broken);
        Ok(())
    }

    #[test]
    fn every_unreadable_package_is_reported_not_just_the_first() -> anyhow::Result<()> {
        let home = tempfile::tempdir()?;
        let root = home.path().join("skills");
        write_broken_package(&root, "first")?;
        write_broken_package(&root, "second")?;
        write_broken_package(&root, "third")?;

        let discovery = discover_skills(home.path(), None)?;

        assert_eq!(discovery.failures.len(), 3);
        Ok(())
    }

    #[test]
    fn a_failure_says_what_was_wrong_with_the_package() -> anyhow::Result<()> {
        let home = tempfile::tempdir()?;
        write_broken_package(&home.path().join("skills"), "unreadable")?;

        let discovery = discover_skills(home.path(), None)?;

        let reason = &discovery.failures[0].reason;
        assert!(
            reason.contains("frontmatter"),
            "a reason a user cannot act on is no better than silence: {reason}"
        );
        Ok(())
    }

    #[test]
    fn an_unusable_root_does_not_hide_the_skills_in_a_usable_one() -> anyhow::Result<()> {
        let home = tempfile::tempdir()?;
        let project = tempfile::tempdir()?;
        let user_root = SkillRoot {
            kind: SkillRootKind::User,
            path: home.path().join("skills"),
        };
        create_skill(
            &user_root,
            "intact",
            "A readable package.",
            SKILL_TEMPLATE_BODY,
        )?;
        // Canonical: discovery resolves the project root, and on macOS the
        // temporary directory reaches it through a `/var` symlink.
        let project_skills = fs::canonicalize(project.path())?
            .join(".whisply")
            .join("skills");
        fs::create_dir_all(project_skills.parent().expect("parent"))?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(home.path(), &project_skills)?;

        let discovery = discover_skills(home.path(), Some(project.path()))?;

        assert_eq!(
            discovery
                .records
                .iter()
                .map(|record| record.name.as_str())
                .collect::<Vec<_>>(),
            vec!["intact"]
        );
        assert!(
            discovery
                .failures
                .iter()
                .any(|failure| failure.path == project_skills)
        );
        Ok(())
    }

    #[test]
    fn a_symlinked_package_is_skipped_rather_than_followed() -> anyhow::Result<()> {
        let home = tempfile::tempdir()?;
        let elsewhere = tempfile::tempdir()?;
        let user_root = SkillRoot {
            kind: SkillRootKind::User,
            path: home.path().join("skills"),
        };
        create_skill(
            &user_root,
            "intact",
            "A readable package.",
            SKILL_TEMPLATE_BODY,
        )?;
        let outside = SkillRoot {
            kind: SkillRootKind::User,
            path: elsewhere.path().to_path_buf(),
        };
        create_skill(
            &outside,
            "smuggled",
            "Outside the root.",
            SKILL_TEMPLATE_BODY,
        )?;
        let link = user_root.path.join("smuggled");
        #[cfg(unix)]
        std::os::unix::fs::symlink(elsewhere.path().join("smuggled"), &link)?;

        let discovery = discover_skills(home.path(), None)?;

        // Continuing past the link must not mean accepting it. Reporting a
        // skipped package is a usability change; admitting one would be a
        // containment hole.
        assert_eq!(
            discovery
                .records
                .iter()
                .map(|record| record.name.as_str())
                .collect::<Vec<_>>(),
            vec!["intact"]
        );
        assert!(
            discovery
                .failures
                .iter()
                .any(|failure| failure.path == link)
        );
        Ok(())
    }

    #[test]
    fn asking_for_a_missing_skill_admits_that_packages_were_skipped() -> anyhow::Result<()> {
        let home = tempfile::tempdir()?;
        write_broken_package(&home.path().join("skills"), "unreadable")?;

        let error = find_unique_skill(home.path(), "unreadable", None)
            .expect_err("a package that cannot be read cannot be selected");

        let message = error.to_string();
        assert!(
            message.contains("could not be read"),
            "reporting a broken package as simply absent sends the user looking \
             in the wrong place: {message}"
        );
        Ok(())
    }

    #[test]
    fn a_readable_skill_is_still_selectable_alongside_a_broken_one() -> anyhow::Result<()> {
        let home = tempfile::tempdir()?;
        let user_root = SkillRoot {
            kind: SkillRootKind::User,
            path: home.path().join("skills"),
        };
        create_skill(
            &user_root,
            "intact",
            "A readable package.",
            SKILL_TEMPLATE_BODY,
        )?;
        write_broken_package(&user_root.path, "unreadable")?;

        let record = find_unique_skill(home.path(), "intact", None)?;

        assert_eq!(record.name, "intact");
        Ok(())
    }

    #[test]
    fn discovers_agents_compatibility_root_without_using_it_as_a_write_target() -> anyhow::Result<()>
    {
        let home = tempfile::tempdir()?;
        let project = tempfile::tempdir()?;
        let roots = skill_roots(home.path(), Some(project.path()))?;
        assert_eq!(
            roots.iter().map(|root| root.kind).collect::<Vec<_>>(),
            vec![
                SkillRootKind::Project,
                SkillRootKind::ProjectAgentsCompatibility,
                SkillRootKind::User,
            ]
        );

        let compatible_root = roots
            .iter()
            .find(|root| root.kind == SkillRootKind::ProjectAgentsCompatibility)
            .expect("compatibility root");
        // This is an external tool's pre-existing package: discovery must not
        // rely on Whisply's managed-root creation policy to see it.
        let package = compatible_root.path.join("shared-agent-skill");
        fs::create_dir_all(&package)?;
        write_new_private(
            &package.join("SKILL.md"),
            render_skill_markdown(
                "shared-agent-skill",
                "A cross-tool package.",
                SKILL_TEMPLATE_BODY,
            )
            .as_bytes(),
        )?;

        let records = discover_skills(home.path(), Some(project.path()))?.records;
        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0].root_kind,
            SkillRootKind::ProjectAgentsCompatibility
        );

        let write_root = managed_write_skill_root(home.path(), Some(project.path()))?;
        assert_eq!(write_root.kind, SkillRootKind::Project);
        assert_eq!(
            write_root.path,
            fs::canonicalize(project.path())?
                .join(".whisply")
                .join("skills")
        );
        Ok(())
    }

    #[test]
    fn imported_scripts_are_detected_without_execution() -> anyhow::Result<()> {
        let source = tempfile::tempdir()?;
        let package = source.path().join("imported");
        fs::create_dir(&package)?;
        write_new_private(
            &package.join("SKILL.md"),
            render_skill_markdown("imported", "Imported package.", SKILL_TEMPLATE_BODY).as_bytes(),
        )?;
        fs::create_dir(package.join("scripts"))?;
        write_new_private(
            &package.join("scripts").join("run.sh"),
            b"#!/bin/sh\nexit 1\n",
        )?;
        let record = validate_skill_package(&package, SkillRootKind::User)?;
        assert!(record.has_scripts);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn managed_skill_root_refuses_a_symlinked_project_metadata_directory() -> anyhow::Result<()> {
        use std::os::unix::fs::symlink;

        let project = tempfile::tempdir()?;
        let elsewhere = tempfile::tempdir()?;
        symlink(elsewhere.path(), project.path().join(".whisply"))?;
        assert!(ensure_skill_root_exists(&project.path().join(".whisply").join("skills")).is_err());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn managed_skill_config_refuses_a_symlink() -> anyhow::Result<()> {
        use std::os::unix::fs::symlink;

        let home = tempfile::tempdir()?;
        let outside = tempfile::NamedTempFile::new()?;
        symlink(outside.path(), home.path().join(CONFIG_TOML_FILE))?;
        assert!(managed_skill_config_path(home.path()).is_err());
        Ok(())
    }
}

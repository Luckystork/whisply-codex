//! Public Whisply skill management.
//!
//! Skill packages are deliberately rooted only in the managed Whisply home or
//! an explicitly selected project's `.whisply/skills` directory. This module
//! never falls back to Codex-branded homes, invokes package scripts, or reads a
//! remote package source.

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
use codex_config::CONFIG_TOML_FILE;
use codex_core::config::edit::ConfigEdit;
use codex_core::config::edit::ConfigEditsBuilder;
use codex_core::config::find_codex_home;
use codex_core_plugins::validate_skill_package_contents;
use codex_whisply::HOME_ENV;

const MAX_SKILL_NAME_BYTES: usize = 64;
const MAX_SKILL_DESCRIPTION_BYTES: usize = 1_024;
const MAX_SKILL_MARKDOWN_BYTES: usize = 128 * 1024;
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
    /// List discovered user and explicit-project skill packages.
    List(SkillRootArgs),
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
    /// Explicit project root whose `.whisply/skills` packages should participate.
    #[arg(long, value_name = "DIR")]
    project_root: Option<PathBuf>,
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
    User,
}

impl SkillRootKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Project => "project",
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
    root_kind: SkillRootKind,
    package_path: PathBuf,
    skill_markdown: PathBuf,
    has_scripts: bool,
}

#[derive(Clone, Debug)]
struct ParsedSkillMarkdown {
    name: String,
    description: String,
    body: String,
}

pub(crate) fn run(command: SkillsCommand) -> anyhow::Result<()> {
    let home = managed_home()?;
    match command.action {
        SkillsSubcommand::List(roots) => {
            let records = discover_skills(&home, roots.project_root.as_deref())?;
            let duplicates = duplicate_names(&records);
            for record in records {
                let enabled = skill_enabled(&home, &record.name)?.unwrap_or(true);
                let duplicate = duplicates.contains(&record.name);
                println!(
                    "{}\t{}\t{}\t{}{}",
                    record.name,
                    record.root_kind.as_str(),
                    if enabled { "enabled" } else { "disabled" },
                    record.description,
                    if duplicate { "\tduplicate" } else { "" }
                );
            }
        }
        SkillsSubcommand::Show(args) => {
            let record = find_unique_skill(&home, &args.name, args.roots.project_root.as_deref())?;
            let markdown = read_bounded_utf8(&record.skill_markdown, MAX_SKILL_MARKDOWN_BYTES)?;
            println!("{}", redact_public_text(&markdown));
        }
        SkillsSubcommand::Validate(args) => {
            let records = discover_skills(&home, args.roots.project_root.as_deref())?;
            let duplicates = duplicate_names(&records);
            if let Some(name) = args.name {
                validate_skill_name(&name)?;
                let matching = records
                    .iter()
                    .filter(|record| record.name == name)
                    .collect::<Vec<_>>();
                if matching.is_empty() {
                    anyhow::bail!("No managed skill named `{name}` was found.");
                }
                if matching.len() > 1 {
                    anyhow::bail!(duplicate_skill_error(&name, &matching));
                }
                println!("Validated skill `{name}`.");
            } else {
                if !duplicates.is_empty() {
                    anyhow::bail!("{}", duplicate_summary(&duplicates));
                }
                println!("Validated {} managed skill package(s).", records.len());
            }
        }
        SkillsSubcommand::Create(args) => {
            validate_skill_name(&args.name)?;
            validate_description(&args.description)?;
            if discover_skills(&home, args.roots.project_root.as_deref())?
                .iter()
                .any(|record| record.name == args.name)
            {
                anyhow::bail!(
                    "A managed skill named `{}` already exists in the selected discovery roots.",
                    args.name
                );
            }
            let body = match args.body_file {
                Some(path) => read_bounded_utf8(&path, MAX_SKILL_MARKDOWN_BYTES)?,
                None => SKILL_TEMPLATE_BODY.to_string(),
            };
            let root = selected_skill_root(&home, args.roots.project_root.as_deref())?;
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
            let description = args.description.unwrap_or(existing.description);
            validate_description(&description)?;
            let body = match args.body_file {
                Some(path) => read_bounded_utf8(&path, MAX_SKILL_MARKDOWN_BYTES)?,
                None => existing.body,
            };
            let content = render_skill_markdown(&record.name, &description, &body);
            validate_skill_markdown_content(&content)?;
            atomic_write_private(&record.skill_markdown, content.as_bytes())?;
            // Revalidate the replacement before reporting success.
            validate_skill_package(&record.package_path, record.root_kind)?;
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
            let root = selected_skill_root(&home, args.project_root.as_deref())?;
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

fn discover_skills(home: &Path, project_root: Option<&Path>) -> anyhow::Result<Vec<SkillRecord>> {
    let roots = skill_roots(home, project_root)?;
    let mut records = Vec::new();
    for root in roots {
        records.extend(scan_skill_root(&root)?);
    }
    records.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.root_kind.as_str().cmp(right.root_kind.as_str()))
    });
    Ok(records)
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
    }
    roots.push(SkillRoot {
        kind: SkillRootKind::User,
        path: home.join("skills"),
    });
    Ok(roots)
}

fn selected_skill_root(home: &Path, project_root: Option<&Path>) -> anyhow::Result<SkillRoot> {
    skill_roots(home, project_root)?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("No managed skill root is available."))
}

fn scan_skill_root(root: &SkillRoot) -> anyhow::Result<Vec<SkillRecord>> {
    match fs::symlink_metadata(&root.path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("Failed to inspect {}.", root.path.display()));
        }
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            anyhow::bail!("Skill root is not a real directory.");
        }
        Ok(_) => {}
    }
    let mut packages = fs::read_dir(&root.path)
        .with_context(|| format!("Failed to list {}.", root.path.display()))?
        .collect::<Result<Vec<_>, _>>()?;
    packages.sort_by_key(|entry| entry.file_name());
    if packages.len() > MAX_SKILL_FILES {
        anyhow::bail!("Skill root contains too many package entries.");
    }
    let mut records = Vec::new();
    for package in packages {
        let path = package.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            anyhow::bail!("Skill package entries may not be symlinks.");
        }
        if !metadata.is_dir() {
            continue;
        }
        records.push(validate_skill_package(&path, root.kind)?);
    }
    Ok(records)
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
    Ok(SkillRecord {
        name: parsed.name,
        description: parsed.description,
        root_kind,
        package_path: package_path.to_path_buf(),
        skill_markdown,
        has_scripts: budget.has_scripts,
    })
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
    let mut values = BTreeMap::new();
    for line in frontmatter {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line
            .split_once(':')
            .ok_or_else(|| anyhow::anyhow!("SKILL.md frontmatter must use key: value entries."))?;
        if !matches!(key, "name" | "description" | "license" | "compatibility") {
            anyhow::bail!("SKILL.md contains unsupported frontmatter metadata `{key}`.");
        }
        if values
            .insert(key, unquote_frontmatter_value(value.trim()))
            .is_some()
        {
            anyhow::bail!("SKILL.md repeats frontmatter key `{key}`.");
        }
    }
    let name = values
        .remove("name")
        .ok_or_else(|| anyhow::anyhow!("SKILL.md frontmatter must include name."))?;
    let description = values
        .remove("description")
        .ok_or_else(|| anyhow::anyhow!("SKILL.md frontmatter must include description."))?;
    validate_skill_name(&name)?;
    validate_description(&description)?;
    if body.trim().is_empty() {
        anyhow::bail!("SKILL.md must include a non-empty instruction body.");
    }
    Ok(ParsedSkillMarkdown {
        name,
        description,
        body,
    })
}

fn validate_skill_markdown_content(markdown: &str) -> anyhow::Result<ParsedSkillMarkdown> {
    let parsed = parse_skill_markdown(markdown)?;
    if line_has_sensitive_material(markdown) {
        anyhow::bail!("SKILL.md may not contain credential-like material.");
    }
    Ok(parsed)
}

fn unquote_frontmatter_value(value: &str) -> String {
    value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .or_else(|| {
            value
                .strip_prefix('\'')
                .and_then(|value| value.strip_suffix('\''))
        })
        .unwrap_or(value)
        .to_string()
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
    let records = discover_skills(home, project_root)?;
    let matching = records
        .into_iter()
        .filter(|record| record.name == name)
        .collect::<Vec<_>>();
    match matching.len() {
        0 => anyhow::bail!("No managed skill named `{name}` was found."),
        1 => Ok(matching.into_iter().next().expect("one matching skill")),
        _ => anyhow::bail!(duplicate_skill_error(
            name,
            &matching.iter().collect::<Vec<_>>()
        )),
    }
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
        "Skill `{name}` is duplicated across managed roots ({origins}); choose or remove one package before changing it."
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
    validate_skill_package(&package, root.kind)?;
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
    let source_record = validate_skill_package(&source, root.kind)?;
    if discover_skills(home, project_root)?
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
    validate_skill_package(&destination, root.kind)?;
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
        let records = discover_skills(home.path(), Some(project.path()))?;
        assert_eq!(records[0].root_kind, SkillRootKind::Project);
        assert_eq!(
            duplicate_names(&records),
            BTreeSet::from(["shared".to_string()])
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

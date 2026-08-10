//! Narrow, public managed configuration and profile controls.
//!
//! Provider identity, account authority, Usage, permissions, and directory
//! contracts intentionally do not appear here: those are native-broker or
//! app-owned per-thread controls, not writable CLI preferences.

use std::fs;
use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use clap::Args;
use codex_config::CONFIG_TOML_FILE;
use codex_config::types::ResumeCwdMode;
use codex_config::types::SessionPickerViewMode;
use codex_core::config::edit::ConfigEditsBuilder;
use codex_core::config::resolve_profile_v2_config_path;
use codex_protocol::config_types::ProfileV2Name;
use codex_protocol::openai_models::ReasoningEffort;
use serde::Serialize;

use crate::whisply_skills::managed_home;

const MAX_CONFIG_FILE_BYTES: usize = 256 * 1024;
const MAX_MANAGED_PROFILES: usize = 256;

#[derive(Debug, Args)]
pub(crate) struct ConfigCommand {
    #[command(subcommand)]
    action: ConfigSubcommand,
}

#[derive(Debug, clap::Subcommand)]
enum ConfigSubcommand {
    /// Show the public, redacted projection of supported managed preferences.
    Show(ConfigTargetArgs),
    /// Set or clear the default signed-catalog model and optional reasoning effort.
    Model(ConfigModelArgs),
    /// Set or clear the managed service-tier preference.
    ServiceTier(ConfigServiceTierArgs),
    /// Set the resume/fork session-picker density.
    SessionPicker(ConfigSessionPickerArgs),
    /// Set whether resume/fork starts in the current or saved session directory.
    ResumeCwd(ConfigResumeCwdArgs),
}

#[derive(Debug, Args)]
pub(crate) struct ProfileCommand {
    #[command(subcommand)]
    action: ProfileSubcommand,
}

#[derive(Debug, clap::Subcommand)]
enum ProfileSubcommand {
    /// List user-created managed profiles.
    List,
    /// Show one profile's public, redacted managed-preference projection.
    Show(ProfileNameArgs),
    /// Create an empty private profile file.
    Create(ProfileNameArgs),
    /// Delete one profile file. This requires explicit confirmation.
    Delete(ProfileDeleteArgs),
}

#[derive(Debug, Args, Clone)]
struct ConfigTargetArgs {
    /// Apply to or inspect this named managed profile instead of the base config.
    #[arg(long, value_name = "NAME")]
    profile: Option<ProfileV2Name>,
}

#[derive(Debug, Args)]
struct ConfigModelArgs {
    /// Signed-catalog model id. Omit only with --clear.
    #[arg(value_name = "MODEL", required_unless_present = "clear")]
    model: Option<String>,

    /// Clear the stored default model and effort.
    #[arg(long, conflicts_with_all = ["model", "reasoning_effort"])]
    clear: bool,

    /// Optional reasoning effort supported by the selected model.
    #[arg(long)]
    reasoning_effort: Option<ReasoningEffort>,

    #[clap(flatten)]
    target: ConfigTargetArgs,
}

#[derive(Debug, Args)]
struct ConfigServiceTierArgs {
    /// Managed service tier to prefer. Omit only with --clear.
    #[arg(value_enum, required_unless_present = "clear")]
    tier: Option<ServiceTierArg>,

    /// Clear the stored tier preference.
    #[arg(long, conflicts_with = "tier")]
    clear: bool,

    #[clap(flatten)]
    target: ConfigTargetArgs,
}

#[derive(Debug, Args)]
struct ConfigSessionPickerArgs {
    #[arg(value_enum)]
    mode: SessionPickerArg,

    #[clap(flatten)]
    target: ConfigTargetArgs,
}

#[derive(Debug, Args)]
struct ConfigResumeCwdArgs {
    #[arg(value_enum)]
    mode: ResumeCwdArg,

    #[clap(flatten)]
    target: ConfigTargetArgs,
}

#[derive(Debug, Args)]
struct ProfileNameArgs {
    #[arg(value_name = "NAME")]
    name: ProfileV2Name,
}

#[derive(Debug, Args)]
struct ProfileDeleteArgs {
    #[arg(value_name = "NAME")]
    name: ProfileV2Name,

    /// Confirm permanent deletion of this private profile file.
    #[arg(long)]
    yes: bool,
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum ServiceTierArg {
    Auto,
    Fast,
    Flex,
}

impl ServiceTierArg {
    fn as_config_value(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Fast => "fast",
            Self::Flex => "flex",
        }
    }
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum SessionPickerArg {
    Comfortable,
    Dense,
}

impl From<SessionPickerArg> for SessionPickerViewMode {
    fn from(value: SessionPickerArg) -> Self {
        match value {
            SessionPickerArg::Comfortable => Self::Comfortable,
            SessionPickerArg::Dense => Self::Dense,
        }
    }
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum ResumeCwdArg {
    Current,
    Session,
}

impl From<ResumeCwdArg> for ResumeCwdMode {
    fn from(value: ResumeCwdArg) -> Self {
        match value {
            ResumeCwdArg::Current => Self::Current,
            ResumeCwdArg::Session => Self::Session,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ManagedConfigProjection {
    profile: Option<String>,
    model: Option<String>,
    reasoning_effort: Option<String>,
    service_tier: Option<String>,
    session_picker: Option<String>,
    resume_cwd: Option<String>,
    authority: &'static str,
    permissions: &'static str,
    directory: &'static str,
}

pub(crate) async fn run_config(command: ConfigCommand) -> anyhow::Result<()> {
    let home = managed_home()?;
    match command.action {
        ConfigSubcommand::Show(target) => print_projection(&home, target.profile.as_ref())?,
        ConfigSubcommand::Model(args) => {
            let path = managed_config_path(&home, args.target.profile.as_ref())?;
            let model = if args.clear {
                None
            } else {
                args.model.as_deref()
            };
            if let Some(model) = model {
                validate_model_id(model)?;
            }
            let effort = if args.clear {
                None
            } else {
                args.reasoning_effort
            };
            ConfigEditsBuilder::for_config_path(&path)
                .set_model(model, effort)
                .apply()
                .await
                .context("Failed to persist the managed model preference.")?;
            print_projection(&home, args.target.profile.as_ref())?;
        }
        ConfigSubcommand::ServiceTier(args) => {
            let path = managed_config_path(&home, args.target.profile.as_ref())?;
            let tier = if args.clear {
                None
            } else {
                args.tier.map(|tier| tier.as_config_value().to_string())
            };
            ConfigEditsBuilder::for_config_path(&path)
                .set_service_tier(tier)
                .apply()
                .await
                .context("Failed to persist the managed service-tier preference.")?;
            print_projection(&home, args.target.profile.as_ref())?;
        }
        ConfigSubcommand::SessionPicker(args) => {
            let path = managed_config_path(&home, args.target.profile.as_ref())?;
            ConfigEditsBuilder::for_config_path(&path)
                .set_session_picker_view(args.mode.into())
                .apply()
                .await
                .context("Failed to persist the managed session-picker preference.")?;
            print_projection(&home, args.target.profile.as_ref())?;
        }
        ConfigSubcommand::ResumeCwd(args) => {
            let path = managed_config_path(&home, args.target.profile.as_ref())?;
            ConfigEditsBuilder::for_config_path(&path)
                .set_resume_cwd(args.mode.into())
                .apply()
                .await
                .context("Failed to persist the managed resume-directory preference.")?;
            print_projection(&home, args.target.profile.as_ref())?;
        }
    }
    Ok(())
}

pub(crate) async fn run_profile(command: ProfileCommand) -> anyhow::Result<()> {
    let home = managed_home()?;
    match command.action {
        ProfileSubcommand::List => {
            for name in list_profiles(&home)? {
                println!("{name}");
            }
        }
        ProfileSubcommand::Show(args) => print_projection(&home, Some(&args.name))?,
        ProfileSubcommand::Create(args) => {
            let path = managed_config_path(&home, Some(&args.name))?;
            if path.exists() {
                anyhow::bail!("Profile `{}` already exists.", args.name);
            }
            write_new_private(
                path.as_path(),
                format!("# Whisply profile: {}\n", args.name).as_bytes(),
            )?;
            println!("Created private profile `{}`.", args.name);
        }
        ProfileSubcommand::Delete(args) => {
            if !args.yes {
                anyhow::bail!("Profile deletion requires --yes.");
            }
            let path = managed_config_path(&home, Some(&args.name))?;
            let metadata = fs::symlink_metadata(path.as_path())
                .with_context(|| format!("Profile `{}` does not exist.", args.name))?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                anyhow::bail!("Refusing to delete a profile that is not a regular private file.");
            }
            fs::remove_file(path.as_path())?;
            println!("Deleted profile `{}`.", args.name);
        }
    }
    Ok(())
}

fn config_path(home: &Path, profile: Option<&ProfileV2Name>) -> PathBuf {
    profile
        .map(|profile| resolve_profile_v2_config_path(home, profile).to_path_buf())
        .unwrap_or_else(|| home.join(CONFIG_TOML_FILE))
}

fn managed_config_path(home: &Path, profile: Option<&ProfileV2Name>) -> anyhow::Result<PathBuf> {
    let path = config_path(home, profile);
    ensure_safe_config_path(&path)?;
    Ok(path)
}

fn ensure_safe_config_path(path: &Path) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Managed config path has no parent directory."))?;
    let parent_metadata = fs::symlink_metadata(parent)?;
    if parent_metadata.file_type().is_symlink() || !parent_metadata.is_dir() {
        anyhow::bail!("Managed config root must be a real directory.");
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.file_type().is_symlink() && metadata.is_file() => Ok(()),
        Ok(_) => {
            anyhow::bail!("Managed config must be a regular file, not a link or special file.")
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn print_projection(home: &Path, profile: Option<&ProfileV2Name>) -> anyhow::Result<()> {
    let path = managed_config_path(home, profile)?;
    let config = read_toml_or_empty(&path)?;
    let projection = ManagedConfigProjection {
        profile: profile.map(ToString::to_string),
        model: toml_string(&config, &["model"]),
        reasoning_effort: toml_string(&config, &["model_reasoning_effort"]),
        service_tier: toml_string(&config, &["service_tier"]),
        session_picker: toml_string(&config, &["tui", "session_picker_view"]),
        resume_cwd: toml_string(&config, &["tui", "resume_cwd"]),
        // These must remain broker/native-owned. Printing this explicit state
        // avoids a config command being mistaken for an authority bypass.
        authority: "managed-native-broker",
        permissions: "native per-thread contract",
        directory: "native per-thread contract",
    };
    println!("{}", serde_json::to_string_pretty(&projection)?);
    Ok(())
}

fn read_toml_or_empty(path: &Path) -> anyhow::Result<toml::Value> {
    ensure_safe_config_path(path)?;
    let metadata = match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(toml::Value::Table(Default::default()));
        }
        Err(error) => return Err(error).context("Failed to inspect managed config."),
        Ok(metadata) => metadata,
    };
    if metadata.len() > MAX_CONFIG_FILE_BYTES as u64 {
        anyhow::bail!("Managed config exceeds its public command size limit.");
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options
        .open(path)
        .context("Failed to read managed config.")?;
    let mut contents = String::new();
    file.take((MAX_CONFIG_FILE_BYTES + 1) as u64)
        .read_to_string(&mut contents)
        .context("Managed config must contain UTF-8 text.")?;
    if contents.len() > MAX_CONFIG_FILE_BYTES {
        anyhow::bail!("Managed config exceeds its public command size limit.");
    }
    toml::from_str(&contents).context("Managed config.toml is invalid.")
}

fn toml_string(config: &toml::Value, path: &[&str]) -> Option<String> {
    let mut value = config;
    for key in path {
        value = value.get(*key)?;
    }
    value.as_str().map(ToOwned::to_owned)
}

fn validate_model_id(model: &str) -> anyhow::Result<()> {
    if model.is_empty()
        || model.len() > 128
        || !model
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        anyhow::bail!("Model ids must be simple signed-catalog identifiers.");
    }
    Ok(())
}

fn list_profiles(home: &Path) -> anyhow::Result<Vec<String>> {
    let mut profiles = Vec::new();
    for entry in fs::read_dir(home)? {
        let entry = entry?;
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            continue;
        };
        let Some(name) = file_name.strip_suffix(".config.toml") else {
            continue;
        };
        if name.parse::<ProfileV2Name>().is_err() {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path())?;
        if !metadata.file_type().is_symlink() && metadata.is_file() {
            profiles.push(name.to_string());
            if profiles.len() > MAX_MANAGED_PROFILES {
                anyhow::bail!("Managed profile listing exceeds its bounded public command limit.");
            }
        }
    }
    profiles.sort();
    Ok(profiles)
}

fn write_new_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Profile path has no parent directory."))?;
    let parent_metadata = fs::symlink_metadata(parent)?;
    if parent_metadata.file_type().is_symlink() || !parent_metadata.is_dir() {
        anyhow::bail!("Managed profile root is not a real directory.");
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
    fn validates_only_simple_model_ids() {
        assert!(validate_model_id("gpt-5.6-luna").is_ok());
        assert!(validate_model_id("../../provider").is_err());
    }

    #[test]
    fn profile_names_are_bounded_by_the_canonical_parser() {
        assert!("work".parse::<ProfileV2Name>().is_ok());
        assert!("nested/work".parse::<ProfileV2Name>().is_err());
    }

    #[test]
    fn projection_never_dumps_unknown_or_sensitive_config_fields() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        fs::write(
            root.path().join(CONFIG_TOML_FILE),
            "model = 'gpt-5.6-luna'\naccess_token = 'private'\n[tui]\nresume_cwd = 'session'\n",
        )?;
        let config = read_toml_or_empty(&root.path().join(CONFIG_TOML_FILE))?;
        assert_eq!(
            toml_string(&config, &["model"]),
            Some("gpt-5.6-luna".to_string())
        );
        assert_eq!(
            toml_string(&config, &["access_token"]),
            Some("private".to_string())
        );
        assert!(toml_string(&config, &["unknown"]).is_none());
        let projection = ManagedConfigProjection {
            profile: None,
            model: toml_string(&config, &["model"]),
            reasoning_effort: None,
            service_tier: None,
            session_picker: None,
            resume_cwd: toml_string(&config, &["tui", "resume_cwd"]),
            authority: "managed-native-broker",
            permissions: "native per-thread contract",
            directory: "native per-thread contract",
        };
        assert!(!serde_json::to_string(&projection)?.contains("private"));
        Ok(())
    }
}

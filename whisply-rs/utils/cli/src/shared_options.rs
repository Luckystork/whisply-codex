//! Shared command-line flags used by both interactive and non-interactive Whisply entry points.

use crate::CliConfigOverrides;
use crate::SandboxModeCliArg;
use clap::Args;
use std::path::PathBuf;
use whisply_protocol::config_types::ProfileV2Name;

#[derive(Args, Clone, Debug, Default)]
pub struct SharedCliOptions {
    /// Optional image(s) to attach to the initial prompt.
    #[arg(
        long = "image",
        short = 'i',
        value_name = "FILE",
        value_delimiter = ',',
        num_args = 1..
    )]
    pub images: Vec<PathBuf>,

    /// Model the agent should use.
    #[arg(long, short = 'm')]
    pub model: Option<String>,

    /// Use open-source provider.
    #[arg(long = "oss", default_value_t = false)]
    pub oss: bool,

    /// Specify which local provider to use (lmstudio or ollama).
    /// If not specified with --oss, will use config default or show selection.
    #[arg(long = "local-provider")]
    pub oss_provider: Option<String>,

    /// Layer $WHISPLY_HOME/<name>.config.toml on top of the base user config.
    #[arg(long = "profile", short = 'p')]
    pub config_profile_v2: Option<ProfileV2Name>,

    /// Select the sandbox policy to use when executing model-generated shell
    /// commands.
    #[arg(long = "sandbox", short = 's')]
    pub sandbox_mode: Option<SandboxModeCliArg>,

    /// Route approval requests through automatic review using the workspace-write sandbox.
    #[arg(
        long = "approve-for-me",
        alias = "not-so-yolo",
        default_value_t = false,
        conflicts_with_all = ["sandbox_mode", "dangerously_bypass_approvals_and_sandbox"]
    )]
    pub auto_review: bool,

    /// Skip all confirmation prompts and execute commands without sandboxing.
    /// EXTREMELY DANGEROUS. Intended solely for running in environments that are externally sandboxed.
    #[arg(
        long = "dangerously-bypass-approvals-and-sandbox",
        alias = "yolo",
        default_value_t = false
    )]
    pub dangerously_bypass_approvals_and_sandbox: bool,

    /// Run enabled hooks without requiring persisted hook trust for this invocation.
    /// DANGEROUS. Intended only for automation that already vets hook sources.
    #[arg(long = "dangerously-bypass-hook-trust", default_value_t = false)]
    pub bypass_hook_trust: bool,

    /// Tell the agent to use the specified directory as its working root.
    #[clap(long = "cd", short = 'C', alias = "directory", value_name = "DIR")]
    pub cwd: Option<PathBuf>,

    /// Start without a working directory.
    ///
    /// The agent runs in a private app-owned root instead of the directory this
    /// command was launched from, and does not pick up that directory's
    /// repository or project configuration.
    #[arg(
        long = "no-directory",
        default_value_t = false,
        conflicts_with_all = ["cwd", "add_dir"]
    )]
    pub no_directory: bool,

    /// Additional directories that should be writable alongside the primary workspace.
    #[arg(long = "add-dir", value_name = "DIR", value_hint = clap::ValueHint::DirPath)]
    pub add_dir: Vec<PathBuf>,
}

impl SharedCliOptions {
    pub fn take_auto_review_config_overrides(&mut self, overrides: &mut CliConfigOverrides) {
        if self.auto_review {
            overrides
                .raw_overrides
                .push(r#"approvals_reviewer="auto_review""#.to_string());
            overrides
                .raw_overrides
                .push(r#"approval_policy="on-request""#.to_string());
            overrides
                .raw_overrides
                .push(r#"sandbox_mode="workspace-write""#.to_string());
            self.auto_review = false;
        }
    }

    pub fn inherit_exec_root_options(&mut self, root: &Self) {
        let self_selected_sandbox_mode = self.sandbox_mode.is_some()
            || self.auto_review
            || self.dangerously_bypass_approvals_and_sandbox;
        let Self {
            images,
            model,
            oss,
            oss_provider,
            config_profile_v2,
            sandbox_mode,
            auto_review,
            dangerously_bypass_approvals_and_sandbox,
            bypass_hook_trust,
            cwd,
            no_directory,
            add_dir,
        } = self;
        let Self {
            images: root_images,
            model: root_model,
            oss: root_oss,
            oss_provider: root_oss_provider,
            config_profile_v2: root_config_profile_v2,
            sandbox_mode: root_sandbox_mode,
            auto_review: root_auto_review,
            dangerously_bypass_approvals_and_sandbox: root_dangerously_bypass_approvals_and_sandbox,
            bypass_hook_trust: root_bypass_hook_trust,
            cwd: root_cwd,
            no_directory: root_no_directory,
            add_dir: root_add_dir,
        } = root;

        if model.is_none() {
            model.clone_from(root_model);
        }
        if *root_oss {
            *oss = true;
        }
        if oss_provider.is_none() {
            oss_provider.clone_from(root_oss_provider);
        }
        if config_profile_v2.is_none() {
            config_profile_v2.clone_from(root_config_profile_v2);
        }
        if !self_selected_sandbox_mode {
            *sandbox_mode = *root_sandbox_mode;
            *auto_review = *root_auto_review;
            *dangerously_bypass_approvals_and_sandbox =
                *root_dangerously_bypass_approvals_and_sandbox;
        }
        if !*bypass_hook_trust {
            *bypass_hook_trust = *root_bypass_hook_trust;
        }
        // A subcommand that names neither choice inherits the root's, but one
        // that already chose a directory must not also inherit `No directory`.
        if cwd.is_none() && !*no_directory {
            cwd.clone_from(root_cwd);
            *no_directory = *root_no_directory;
        }
        if !root_images.is_empty() {
            let mut merged_images = root_images.clone();
            merged_images.append(images);
            *images = merged_images;
        }
        if !root_add_dir.is_empty() {
            let mut merged_add_dir = root_add_dir.clone();
            merged_add_dir.append(add_dir);
            *add_dir = merged_add_dir;
        }
    }

    pub fn apply_subcommand_overrides(&mut self, subcommand: Self) {
        let subcommand_selected_sandbox_mode = subcommand.sandbox_mode.is_some()
            || subcommand.auto_review
            || subcommand.dangerously_bypass_approvals_and_sandbox;
        let Self {
            images,
            model,
            oss,
            oss_provider,
            config_profile_v2,
            sandbox_mode,
            auto_review,
            dangerously_bypass_approvals_and_sandbox,
            bypass_hook_trust,
            cwd,
            no_directory,
            add_dir,
        } = subcommand;

        if let Some(model) = model {
            self.model = Some(model);
        }
        if oss {
            self.oss = true;
        }
        if let Some(oss_provider) = oss_provider {
            self.oss_provider = Some(oss_provider);
        }
        if let Some(config_profile_v2) = config_profile_v2 {
            self.config_profile_v2 = Some(config_profile_v2);
        }
        if subcommand_selected_sandbox_mode {
            self.sandbox_mode = sandbox_mode;
            self.auto_review = auto_review;
            self.dangerously_bypass_approvals_and_sandbox =
                dangerously_bypass_approvals_and_sandbox;
        }
        if bypass_hook_trust {
            self.bypass_hook_trust = true;
        }
        if let Some(cwd) = cwd {
            self.cwd = Some(cwd);
            self.no_directory = false;
        } else if no_directory {
            self.no_directory = true;
            self.cwd = None;
        }
        if !images.is_empty() {
            self.images = images;
        }
        if !add_dir.is_empty() {
            self.add_dir.extend(add_dir);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use pretty_assertions::assert_eq;

    #[derive(Parser, Debug)]
    struct TestCli {
        #[clap(flatten)]
        shared: SharedCliOptions,
    }

    fn parse(args: &[&str]) -> Result<SharedCliOptions, clap::Error> {
        let mut argv = vec!["whisply"];
        argv.extend_from_slice(args);
        TestCli::try_parse_from(argv).map(|cli| cli.shared)
    }

    #[test]
    fn directory_is_accepted_alongside_the_original_cd_spelling() {
        let by_alias = parse(&["--directory", "/tmp/work"]).expect("--directory");
        let by_original = parse(&["--cd", "/tmp/work"]).expect("--cd");
        let by_short = parse(&["-C", "/tmp/work"]).expect("-C");

        assert_eq!(by_alias.cwd, Some(PathBuf::from("/tmp/work")));
        assert_eq!(by_alias.cwd, by_original.cwd);
        assert_eq!(by_alias.cwd, by_short.cwd);
        assert!(!by_alias.no_directory);
    }

    /// `No directory` is only meaningful if it cannot be combined with a
    /// directory or with extra writable roots; either would hand back the
    /// filesystem authority the choice exists to withhold.
    #[test]
    fn no_directory_refuses_to_be_combined_with_filesystem_authority() {
        assert!(parse(&["--no-directory"]).expect("alone").no_directory);
        assert!(parse(&["--no-directory", "--cd", "/tmp/work"]).is_err());
        assert!(parse(&["--no-directory", "--directory", "/tmp/work"]).is_err());
        assert!(parse(&["--no-directory", "--add-dir", "/tmp/other"]).is_err());
    }

    #[test]
    fn a_subcommand_directory_replaces_an_inherited_no_directory() {
        let mut root = parse(&["--no-directory"]).expect("root");
        root.apply_subcommand_overrides(parse(&["--cd", "/tmp/work"]).expect("subcommand"));

        assert_eq!(root.cwd, Some(PathBuf::from("/tmp/work")));
        assert!(
            !root.no_directory,
            "choosing a directory has to clear the no-directory choice, not stack with it"
        );
    }

    #[test]
    fn a_subcommand_that_chose_a_directory_does_not_inherit_no_directory() {
        let root = parse(&["--no-directory"]).expect("root");
        let mut chose_directory = parse(&["--cd", "/tmp/work"]).expect("subcommand");
        chose_directory.inherit_exec_root_options(&root);

        assert_eq!(chose_directory.cwd, Some(PathBuf::from("/tmp/work")));
        assert!(!chose_directory.no_directory);

        let mut chose_nothing = parse(&[]).expect("subcommand");
        chose_nothing.inherit_exec_root_options(&root);
        assert!(
            chose_nothing.no_directory,
            "silence inherits the root choice"
        );
    }
}

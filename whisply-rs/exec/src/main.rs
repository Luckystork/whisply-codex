//! Entry-point for the internal non-interactive Whisply helper binary.
//!
//! When this helper is invoked normally, it parses the standard Whisply exec
//! options and launches the non-interactive Whisply agent. However, if it is
//! invoked with arg0 as `codex-linux-sandbox`, we instead treat the invocation
//! as a request to run the logic for the standalone `codex-linux-sandbox`
//! executable (i.e., parse any -s args and then run a *sandboxed* command under
//! Landlock + seccomp.
//!
//! This allows us to ship a completely separate set of functionality as part
//! of this internal exec helper. The managed distribution exposes the same
//! behavior through `whisply exec`; it does not publish this target separately.
use clap::Parser;
use whisply_arg0::Arg0DispatchPaths;
use whisply_arg0::arg0_dispatch_or_else;
use whisply_exec::Cli;
use whisply_exec::run_main;
use whisply_utils_cli::CliConfigOverrides;

#[derive(Parser, Debug)]
struct TopCli {
    #[arg(long, global = true, hide = true)]
    psp: bool,

    #[clap(flatten)]
    config_overrides: CliConfigOverrides,

    #[clap(flatten)]
    inner: Cli,
}

fn main() -> anyhow::Result<()> {
    arg0_dispatch_or_else(|arg0_paths: Arg0DispatchPaths| async move {
        let top_cli = TopCli::parse();
        // Merge root-level overrides into inner CLI struct so downstream logic remains unchanged.
        let mut inner = top_cli.inner;
        inner.psp = top_cli.psp;
        inner
            .config_overrides
            .prepend_root_overrides(top_cli.config_overrides);

        run_main(inner, arg0_paths).await?;
        Ok(())
    })
}

#[cfg(test)]
#[path = "main_tests.rs"]
mod tests;

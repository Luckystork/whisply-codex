use clap::Parser;
use std::path::PathBuf;

#[derive(Debug, Parser)]
pub struct AppCommand {
    /// Workspace path to open after launching the managed Whisply app.
    #[arg(value_name = "PATH", default_value = ".")]
    pub path: PathBuf,

    /// Former installer override retained only to return a safe migration error.
    #[arg(long = "download-url", hide = true)]
    pub download_url_override: Option<String>,
}

pub async fn run_app(cmd: AppCommand) -> anyhow::Result<()> {
    let workspace = std::fs::canonicalize(&cmd.path).unwrap_or(cmd.path);
    if cmd.download_url_override.is_some() {
        anyhow::bail!(
            "Whisply does not download or install app bundles from the runtime. Update or install Whisply through its managed distribution."
        );
    }
    println!(
        "Open the installed Whisply app, then choose this workspace: {}",
        workspace.display()
    );
    Ok(())
}

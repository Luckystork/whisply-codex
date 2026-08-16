//! Explicit thread-scoped directory selection.

use std::fs;
use std::path::Path;
use std::path::PathBuf;

use serde::Deserialize;
use serde::Serialize;
use thiserror::Error;

use crate::AccountRuntimeHome;
use crate::RuntimeHomeError;

/// Directory name of the private working root used by `No directory`.
pub const NO_DIRECTORY_WORKSPACE_DIR_NAME: &str = "neutral-workspace";

/// The private working root for a `No directory` thread.
///
/// Upstream execution still needs a real absolute cwd, so `No directory` is
/// given this app-owned one instead of the launcher's. Every entry point
/// resolves it here so a thread started from the Mac app and one started with
/// `--no-directory` share a root rather than two lookalike directories.
pub fn no_directory_working_root(whisply_home: &Path) -> Result<PathBuf, RuntimeHomeError> {
    crate::ensure_private_runtime_directory(&whisply_home.join(NO_DIRECTORY_WORKSPACE_DIR_NAME))
}

/// A command-line entry point's resolved working root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CliWorkingRoot {
    /// The working root to run in, or `None` to keep the launch directory.
    pub cwd: Option<PathBuf>,
    /// Whether the user chose this directory, as opposed to merely starting in
    /// it. `No directory` populates `cwd` but is the opposite of a choice.
    pub workspace_directory_selected: bool,
}

/// Resolves `--directory`/`--cd` and `--no-directory` into a working root.
///
/// Shared by the interactive and non-interactive entry points so the two agree
/// on what `No directory` means, and so neither can treat the private root as a
/// user selection.
pub fn resolve_cli_working_root(
    whisply_home: &Path,
    explicit_cwd: Option<PathBuf>,
    no_directory: bool,
) -> Result<CliWorkingRoot, RuntimeHomeError> {
    if no_directory {
        return Ok(CliWorkingRoot {
            cwd: Some(no_directory_working_root(whisply_home)?),
            workspace_directory_selected: false,
        });
    }
    // A folder named on the command line that is missing, moved, or unreadable
    // has to be reported here. Accepting it starts a session whose every file
    // read and command fails, with nothing on screen connecting the failures to
    // the flag that caused them.
    if let Some(cwd) = explicit_cwd.as_deref()
        && !cwd.is_dir()
    {
        return Err(RuntimeHomeError::WorkingDirectoryUnavailable(
            cwd.to_path_buf(),
        ));
    }
    Ok(CliWorkingRoot {
        workspace_directory_selected: explicit_cwd.is_some(),
        cwd: explicit_cwd,
    })
}

/// The user-visible directory choice for a Whisply thread.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DirectorySelection {
    /// The safe default: no user-selected workspace.
    #[default]
    NoDirectory,
    /// A user explicitly selected this canonical directory.
    Selected { canonical_path: PathBuf },
}

impl DirectorySelection {
    /// Canonicalizes an explicitly selected existing directory.
    pub fn select(path: impl AsRef<Path>) -> Result<Self, DirectorySelectionError> {
        let path = path.as_ref();
        let metadata =
            fs::symlink_metadata(path).map_err(|source| DirectorySelectionError::Io {
                path: path.to_path_buf(),
                source,
            })?;
        if metadata.file_type().is_symlink() {
            return Err(DirectorySelectionError::Symlink(path.to_path_buf()));
        }
        if !metadata.is_dir() {
            return Err(DirectorySelectionError::NotDirectory(path.to_path_buf()));
        }
        let canonical_path = path
            .canonicalize()
            .map_err(|source| DirectorySelectionError::Io {
                path: path.to_path_buf(),
                source,
            })?;
        Ok(Self::Selected { canonical_path })
    }

    /// Resolves the actual cwd to use without inheriting the launcher cwd.
    pub fn runtime_working_directory(
        &self,
        account_home: &AccountRuntimeHome,
    ) -> Result<PathBuf, DirectorySelectionError> {
        match self {
            Self::NoDirectory => account_home
                .neutral_working_root()
                .map_err(DirectorySelectionError::RuntimeHome),
            Self::Selected { canonical_path } => {
                let metadata = fs::symlink_metadata(canonical_path).map_err(|source| {
                    DirectorySelectionError::Io {
                        path: canonical_path.clone(),
                        source,
                    }
                })?;
                if metadata.file_type().is_symlink() {
                    return Err(DirectorySelectionError::Symlink(canonical_path.clone()));
                }
                if !metadata.is_dir() {
                    return Err(DirectorySelectionError::NotDirectory(
                        canonical_path.clone(),
                    ));
                }
                canonical_path
                    .canonicalize()
                    .map_err(|source| DirectorySelectionError::Io {
                        path: canonical_path.clone(),
                        source,
                    })
            }
        }
    }

    /// Returns the public display state without revealing the neutral root.
    pub fn display_label(&self) -> String {
        match self {
            Self::NoDirectory => "No directory".to_string(),
            Self::Selected { canonical_path } => canonical_path
                .file_name()
                .and_then(|name| name.to_str())
                .map_or_else(|| canonical_path.display().to_string(), ToString::to_string),
        }
    }
}

/// Errors emitted when a directory cannot safely become a thread workspace.
#[derive(Debug, Error)]
pub enum DirectorySelectionError {
    #[error("selected directory is a symbolic link and cannot be trusted: {0}")]
    Symlink(PathBuf),
    #[error("selected path is not a directory: {0}")]
    NotDirectory(PathBuf),
    #[error("failed to inspect selected directory {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to create the account-owned neutral working root: {0}")]
    RuntimeHome(#[from] RuntimeHomeError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AccountKey;
    use tempfile::tempdir;

    #[test]
    fn no_directory_uses_account_owned_neutral_root() {
        let temporary = tempdir().expect("temporary root");
        let account_home = AccountRuntimeHome::create(
            temporary.path().join(".whisply"),
            AccountKey::parse("account_602b9c6b036e4f7ca799").expect("key"),
        )
        .expect("account home");

        let cwd = DirectorySelection::NoDirectory
            .runtime_working_directory(&account_home)
            .expect("neutral cwd");

        assert!(cwd.starts_with(account_home.runtime_home()));
        assert_ne!(cwd, std::env::current_dir().expect("current directory"));
    }

    /// WCD-712: `No directory` must not inherit the directory the command was
    /// launched from, which is the whole difference between it and passing no
    /// directory flag at all.
    #[test]
    fn cli_no_directory_runs_in_a_private_root_instead_of_the_launch_directory() {
        let temporary = tempdir().expect("temporary home");
        let whisply_home = temporary.path().join(".whisply");

        let resolved = resolve_cli_working_root(
            &whisply_home,
            /*explicit_cwd*/ None,
            /*no_directory*/ true,
        )
        .expect("no-directory root");

        let cwd = resolved.cwd.expect("execution still needs a real cwd");
        assert!(cwd.starts_with(whisply_home.canonicalize().expect("home")));
        assert_ne!(cwd, std::env::current_dir().expect("current directory"));
        assert!(
            !resolved.workspace_directory_selected,
            "the private root is the opposite of a chosen workspace"
        );
    }

    /// The app-server resolves the same root for a `noDirectory` thread, so a
    /// thread started from the Mac app and one started with `--no-directory`
    /// have to land in one place rather than two lookalike directories.
    #[test]
    fn cli_and_app_server_share_one_no_directory_root() {
        let temporary = tempdir().expect("temporary home");
        let whisply_home = temporary.path().join(".whisply");

        let from_cli = resolve_cli_working_root(&whisply_home, None, /*no_directory*/ true)
            .expect("cli root")
            .cwd
            .expect("cli cwd");
        let from_app_server = no_directory_working_root(&whisply_home).expect("app-server root");

        assert_eq!(from_cli, from_app_server);
        assert!(from_cli.ends_with(NO_DIRECTORY_WORKSPACE_DIR_NAME));
    }

    #[test]
    fn cli_explicit_directory_is_a_selection_and_no_flag_is_not() {
        let temporary = tempdir().expect("temporary home");
        let whisply_home = temporary.path().join(".whisply");
        let chosen = temporary.path().join("workspace");
        fs::create_dir_all(&chosen).expect("workspace");

        let selected = resolve_cli_working_root(
            &whisply_home,
            Some(chosen.clone()),
            /*no_directory*/ false,
        )
        .expect("selected root");
        assert_eq!(selected.cwd.as_deref(), Some(chosen.as_path()));
        assert!(selected.workspace_directory_selected);

        let unset = resolve_cli_working_root(&whisply_home, None, /*no_directory*/ false)
            .expect("unset root");
        assert_eq!(
            unset,
            CliWorkingRoot {
                cwd: None,
                workspace_directory_selected: false,
            },
            "starting in a directory is not choosing it, and must not be rewritten either"
        );
    }

    /// WCD-713: a folder named on the command line that has been moved or
    /// deleted has to be reported as such. Starting anyway produces a session
    /// where every command fails and nothing names the flag responsible.
    #[test]
    fn a_named_directory_that_is_missing_is_refused_by_name() {
        let temporary = tempdir().expect("temporary home");
        let whisply_home = temporary.path().join(".whisply");
        let missing = temporary.path().join("moved-away");

        let error = resolve_cli_working_root(
            &whisply_home,
            Some(missing.clone()),
            /*no_directory*/ false,
        )
        .expect_err("a missing folder cannot become a working root");

        assert!(
            matches!(&error, RuntimeHomeError::WorkingDirectoryUnavailable(path) if path == &missing),
            "expected the missing folder to be named, got {error:?}"
        );
        assert!(
            error.to_string().contains("moved-away"),
            "the person has to be told which folder: {error}"
        );
    }

    #[test]
    fn a_named_path_that_is_a_file_is_refused_too() {
        let temporary = tempdir().expect("temporary home");
        let whisply_home = temporary.path().join(".whisply");
        let file = temporary.path().join("notes.txt");
        fs::write(&file, "not a folder").expect("write file");

        let error = resolve_cli_working_root(
            &whisply_home,
            Some(file.clone()),
            /*no_directory*/ false,
        )
        .expect_err("a file cannot become a working root");
        assert!(
            matches!(&error, RuntimeHomeError::WorkingDirectoryUnavailable(path) if path == &file),
            "expected the file to be refused as a working root, got {error:?}"
        );
    }

    #[test]
    fn selected_directory_is_canonicalized() {
        let temporary = tempdir().expect("temporary root");
        let selection = DirectorySelection::select(temporary.path()).expect("selection");

        assert!(matches!(selection, DirectorySelection::Selected { .. }));
        assert_eq!(
            selection.display_label(),
            temporary
                .path()
                .file_name()
                .expect("name")
                .to_string_lossy()
        );
    }
}

//! Explicit thread-scoped directory selection.

use std::fs;
use std::path::Path;
use std::path::PathBuf;

use serde::Deserialize;
use serde::Serialize;
use thiserror::Error;

use crate::AccountRuntimeHome;
use crate::RuntimeHomeError;

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

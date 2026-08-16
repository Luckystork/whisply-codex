//! Opaque account-scoped runtime storage.

use std::fs;
use std::path::Path;
use std::path::PathBuf;

use thiserror::Error;

/// An opaque local account key. Raw account IDs and email addresses are not
/// valid keys and must never be used as path segments.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AccountKey(String);

impl AccountKey {
    /// Validates an opaque account key created by the Whisply credential owner.
    pub fn parse(value: impl Into<String>) -> Result<Self, RuntimeHomeError> {
        let value = value.into();
        let is_valid = (16..=128).contains(&value.len())
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            && !looks_like_uuid(&value);
        if is_valid {
            Ok(Self(value))
        } else {
            Err(RuntimeHomeError::InvalidAccountKey)
        }
    }

    /// Returns the opaque path-safe value.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn looks_like_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
}

/// The effective storage root used by one signed-in Whisply account.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountRuntimeHome {
    product_home: PathBuf,
    account_key: AccountKey,
    runtime_home: PathBuf,
}

impl AccountRuntimeHome {
    /// Creates the account-isolated runtime home with user-only permissions.
    pub fn create(
        product_home: impl AsRef<Path>,
        account_key: AccountKey,
    ) -> Result<Self, RuntimeHomeError> {
        let product_home = product_home.as_ref();
        if !product_home.is_absolute() {
            return Err(RuntimeHomeError::ProductHomeMustBeAbsolute(
                product_home.to_path_buf(),
            ));
        }
        ensure_private_directory(product_home)?;

        let accounts_home = product_home.join("accounts");
        ensure_private_directory(&accounts_home)?;
        let account_home = accounts_home.join(account_key.as_str());
        ensure_private_directory(&account_home)?;
        let runtime_home = account_home.join("runtime");
        ensure_private_directory(&runtime_home)?;

        Ok(Self {
            product_home: canonical_directory(product_home)?,
            account_key,
            runtime_home: canonical_directory(&runtime_home)?,
        })
    }

    /// The logical Whisply root shared only through explicit immutable assets.
    pub fn product_home(&self) -> &Path {
        &self.product_home
    }

    /// The opaque account key associated with this runtime root.
    pub fn account_key(&self) -> &AccountKey {
        &self.account_key
    }

    /// The only mutable runtime root for this account.
    pub fn runtime_home(&self) -> &Path {
        &self.runtime_home
    }

    /// Creates the app-owned working root for public `No directory` state.
    ///
    /// Callers must disable project/repository discovery and workspace tool
    /// authority when using this path; it is never presented to the user as a
    /// selected directory.
    pub fn neutral_working_root(&self) -> Result<PathBuf, RuntimeHomeError> {
        let root = self.runtime_home.join("neutral-workspace");
        ensure_private_directory(&root)?;
        canonical_directory(&root)
    }
}

/// Errors emitted while creating or resolving account-scoped state.
#[derive(Debug, Error)]
pub enum RuntimeHomeError {
    #[error("the account key is not an opaque path-safe local key")]
    InvalidAccountKey,
    #[error("the Whisply product home must be absolute: {0}")]
    ProductHomeMustBeAbsolute(PathBuf),
    #[error("refusing a symbolic-link storage path: {0}")]
    SymlinkPath(PathBuf),
    #[error("storage path is not a directory: {0}")]
    NotDirectory(PathBuf),
    #[error("the folder {0} does not exist or is not a directory")]
    WorkingDirectoryUnavailable(PathBuf),
    #[error("failed to prepare Whisply storage {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

fn ensure_private_directory(path: &Path) -> Result<(), RuntimeHomeError> {
    if path.exists() {
        let metadata = fs::symlink_metadata(path).map_err(|source| RuntimeHomeError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        if metadata.file_type().is_symlink() {
            return Err(RuntimeHomeError::SymlinkPath(path.to_path_buf()));
        }
        if !metadata.is_dir() {
            return Err(RuntimeHomeError::NotDirectory(path.to_path_buf()));
        }
    } else {
        fs::create_dir_all(path).map_err(|source| RuntimeHomeError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|source| {
            RuntimeHomeError::Io {
                path: path.to_path_buf(),
                source,
            }
        })?;
    }
    Ok(())
}

/// Prepares an app-owned, account-scoped directory without following a final
/// symbolic link. This is used for the no-directory working root supplied to
/// upstream execution code, which still requires an absolute real directory.
pub fn ensure_private_runtime_directory(path: &Path) -> Result<PathBuf, RuntimeHomeError> {
    ensure_private_directory(path)?;
    canonical_directory(path)
}

fn canonical_directory(path: &Path) -> Result<PathBuf, RuntimeHomeError> {
    path.canonicalize().map_err(|source| RuntimeHomeError::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use tempfile::tempdir;

    #[test]
    fn creates_an_account_isolated_neutral_root() {
        let temporary = tempdir().expect("temporary home");
        let product_home = temporary.path().join(".whisply");
        let account_key = AccountKey::parse("account_7cabb4f9150046bfa681").expect("account key");

        let home = AccountRuntimeHome::create(&product_home, account_key.clone()).expect("home");
        let neutral_root = home.neutral_working_root().expect("neutral root");

        assert_eq!(home.account_key(), &account_key);
        assert!(
            home.runtime_home()
                .ends_with("accounts/account_7cabb4f9150046bfa681/runtime")
        );
        assert!(neutral_root.ends_with("neutral-workspace"));
        assert!(neutral_root.is_dir());
    }

    #[test]
    fn rejects_raw_account_like_path_values() {
        assert!(AccountKey::parse("person@example.com").is_err());
        assert!(AccountKey::parse("../../other-account").is_err());
        assert!(AccountKey::parse("550e8400-e29b-41d4-a716-446655440000").is_err());
    }

    #[test]
    fn creates_a_private_app_owned_runtime_directory() {
        let temporary = tempdir().expect("temporary home");
        let runtime_directory = temporary.path().join("runtime").join("neutral-workspace");

        let canonical =
            ensure_private_runtime_directory(&runtime_directory).expect("runtime directory");

        assert_eq!(
            canonical,
            runtime_directory.canonicalize().expect("canonical path")
        );
        assert!(canonical.is_dir());
    }
}

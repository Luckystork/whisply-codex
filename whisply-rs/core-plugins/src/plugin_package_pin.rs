//! Whether a plugin's own files are still the ones that were approved.
//!
//! A plugin's MCP servers start processes outside the sandbox, and the launch
//! pin records the command line the person agreed to. A command line is not a
//! program: `node /…/plugin/server.js` reads identically before and after
//! `server.js` is rewritten, and an installed plugin tree can be edited in
//! place between one session and the next. So a pin on the declaration alone
//! says nothing about the code it starts.
//!
//! The package pin is a hash of the files the plugin ships. It is recorded when
//! the person approves the plugin and compared before its servers launch, which
//! turns an edit to the program into a refusal instead of a silent execution.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::io::Read;
use std::path::Path;

use sha2::Digest;
use sha2::Sha256;

/// The plugin's own scratch directory when the store has not given it one.
/// A server writing state next to where it runs is not a package that changed.
const PLUGIN_DATA_DIR_NAME: &str = ".plugin-data";
/// Version-control metadata is not part of what runs, and a fetch rewrites it
/// without a single shipped file changing.
const VCS_DIR_NAME: &str = ".git";
/// macOS writes one of these into any directory a person opens in Finder.
const FOLDER_METADATA_FILE_NAME: &str = ".DS_Store";

/// Guards against a pathological tree rather than expressing a policy: a plugin
/// large enough to reach either bound cannot be hashed, and says so.
const MAX_PACKAGE_FILES: usize = 100_000;
const MAX_PACKAGE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_PACKAGE_DEPTH: usize = 100;

const CONTENT_CHUNK_BYTES: usize = 64 * 1024;

/// What a recorded package pin says about the files in front of us now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginPackageTrust {
    /// No hash was recorded, which is every plugin installed before package
    /// pins existed. The plugin is loaded, because refusing here would break
    /// working installations to guard against a change that has not happened.
    Unpinned,
    Trusted,
    /// The files changed after they were approved. This is the case the pin
    /// exists for.
    Changed,
    /// A hash was recorded and cannot be computed again. Whatever the package
    /// is now, it is not the thing that was measured, so it is treated as a
    /// change rather than as an absence of evidence.
    Unreadable,
}

impl PluginPackageTrust {
    /// Whether the plugin may start processes outside the sandbox.
    pub fn may_launch(self) -> bool {
        matches!(self, Self::Unpinned | Self::Trusted)
    }
}

/// A stable identity for the files a plugin ships, or `None` when the tree
/// cannot be read or is too large to measure.
pub async fn plugin_package_hash(plugin_root: &Path) -> Option<String> {
    let plugin_root = plugin_root.to_path_buf();
    tokio::task::spawn_blocking(move || hash_plugin_package(&plugin_root))
        .await
        .ok()
        .flatten()
}

/// The same measurement for install paths that do not run inside an async
/// runtime: the curated cache refresh runs on its own thread, and the remote
/// bundle install is already on a blocking one.
pub fn plugin_package_hash_blocking(plugin_root: &Path) -> Option<String> {
    hash_plugin_package(plugin_root)
}

/// How the package in front of us compares with the hash recorded for it.
///
/// Nothing is hashed when no pin was recorded: an answer that cannot change the
/// outcome is not worth walking a tree for.
pub async fn plugin_package_trust(
    plugin_root: &Path,
    trusted_package_hash: Option<&str>,
) -> PluginPackageTrust {
    let Some(trusted) = trusted_package_hash else {
        return PluginPackageTrust::Unpinned;
    };
    match plugin_package_hash(plugin_root).await {
        None => PluginPackageTrust::Unreadable,
        Some(actual) if actual == trusted => PluginPackageTrust::Trusted,
        Some(_) => PluginPackageTrust::Changed,
    }
}

fn hash_plugin_package(plugin_root: &Path) -> Option<String> {
    let metadata = fs::symlink_metadata(plugin_root).ok()?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return None;
    }
    let mut hasher = Sha256::new();
    let mut budget = PackageBudget::default();
    hash_directory(
        plugin_root,
        plugin_root,
        /*depth*/ 0,
        &mut hasher,
        &mut budget,
    )
    .ok()?;
    let digest = hasher.finalize();
    let hex = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Some(format!("sha256:{hex}"))
}

#[derive(Default)]
struct PackageBudget {
    files: usize,
    bytes: u64,
}

impl PackageBudget {
    fn take(&mut self, bytes: u64) -> io::Result<()> {
        self.files += 1;
        self.bytes = self.bytes.saturating_add(bytes);
        if self.files > MAX_PACKAGE_FILES || self.bytes > MAX_PACKAGE_BYTES {
            return Err(io::Error::other("plugin package is too large to pin"));
        }
        Ok(())
    }
}

fn hash_directory(
    plugin_root: &Path,
    current: &Path,
    depth: usize,
    hasher: &mut Sha256,
    budget: &mut PackageBudget,
) -> io::Result<()> {
    if depth > MAX_PACKAGE_DEPTH {
        return Err(io::Error::other(
            "plugin package is nested too deeply to pin",
        ));
    }

    // Read the whole directory before hashing any of it, so the identity does
    // not depend on the order the filesystem happens to return entries in.
    let mut entries = BTreeMap::new();
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        entries.insert(entry.file_name(), entry.path());
    }

    for (name, path) in entries {
        let Some(name) = name.to_str() else {
            // A name that is not text cannot be recorded as one, and a pin that
            // silently skipped it would cover less than it claims to.
            return Err(io::Error::other(
                "plugin package contains a file name that is not valid UTF-8",
            ));
        };
        if is_excluded(name, depth) {
            continue;
        }
        let relative = path
            .strip_prefix(plugin_root)
            .map_err(|_| io::Error::other("plugin package path escaped its root"))?
            .to_str()
            .ok_or_else(|| {
                io::Error::other("plugin package contains a path that is not valid UTF-8")
            })?
            .to_string();

        let metadata = fs::symlink_metadata(&path)?;
        let file_type = metadata.file_type();
        if file_type.is_symlink() {
            // Links are not followed: where one points is part of what runs,
            // and following them would leave the package's own tree.
            let target = fs::read_link(&path)?.into_os_string();
            hash_entry(hasher, b"symlink", &relative);
            hash_bytes(hasher, os_string_bytes(&target)?);
            budget.take(/*bytes*/ 0)?;
        } else if file_type.is_dir() {
            hash_entry(hasher, b"directory", &relative);
            hash_directory(plugin_root, &path, depth + 1, hasher, budget)?;
        } else if file_type.is_file() {
            budget.take(metadata.len())?;
            hash_entry(hasher, b"file", &relative);
            // Whether a shipped file may be executed is part of what the
            // package is, and flipping that bit changes no byte of content.
            hasher.update([u8::from(is_executable(&metadata))]);
            hasher.update(metadata.len().to_le_bytes());
            hash_bytes(hasher, &hash_file_contents(&path)?);
        } else {
            hash_entry(hasher, b"other", &relative);
            budget.take(/*bytes*/ 0)?;
        }
    }

    Ok(())
}

/// Directories whose contents are written by something other than the plugin
/// author, and so cannot stand for the code the person reviewed.
fn is_excluded(name: &str, depth: usize) -> bool {
    match name {
        VCS_DIR_NAME | FOLDER_METADATA_FILE_NAME => true,
        // Only at the top level: this is the fallback data root the loader
        // hands a plugin, not a name a package may not use for its own code.
        PLUGIN_DATA_DIR_NAME => depth == 0,
        _ => false,
    }
}

#[cfg(unix)]
fn is_executable(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_metadata: &fs::Metadata) -> bool {
    false
}

#[cfg(unix)]
fn os_string_bytes(value: &OsString) -> io::Result<&[u8]> {
    use std::os::unix::ffi::OsStrExt;
    Ok(value.as_os_str().as_bytes())
}

#[cfg(not(unix))]
fn os_string_bytes(value: &OsString) -> io::Result<&[u8]> {
    value
        .to_str()
        .map(str::as_bytes)
        .ok_or_else(|| io::Error::other("plugin package contains a link target that is not text"))
}

fn hash_file_contents(path: &Path) -> io::Result<Vec<u8>> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; CONTENT_CHUNK_BYTES];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().to_vec())
}

/// Every part is length-prefixed so no two different trees can produce the same
/// stream of bytes by running one field into the next.
fn hash_entry(hasher: &mut Sha256, kind: &[u8], relative_path: &str) {
    hash_bytes(hasher, kind);
    hash_bytes(hasher, relative_path.as_bytes());
}

fn hash_bytes(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

#[cfg(test)]
#[path = "plugin_package_pin_tests.rs"]
mod tests;

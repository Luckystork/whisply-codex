//! What Whisply keeps on this machine, and what removing it is allowed to touch.
//!
//! Two separate jobs live here. The first is an honest inventory: a person who
//! wants to know what the product stores locally should be able to read it,
//! take a copy, and delete the parts that are accumulation rather than their
//! own work. The second is the part that makes the first one safe. A cleanup
//! feature is one resolution bug away from deleting a home directory, so no
//! caller anywhere supplies a path: callers name a category, the category maps
//! to a fixed relative location, and every resolved target is checked against
//! the canonical home before anything is read or removed.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;

use thiserror::Error;

/// Bounds the inventory walk. Walking without a bound would let a pathological
/// tree hang the command, so a category larger than this is counted as far as
/// the bound and reported as a lower bound rather than as a total.
const MAX_WALK_ENTRIES: usize = 200_000;
const MAX_WALK_DEPTH: usize = 32;

/// One named part of the local store.
///
/// Ids are a closed set because they are the only thing a caller may name. A
/// caller that could pass a path could pass `~`, `$HOME`, `*`, or a path in
/// another product's home, and every one of those is a deletion nobody asked
/// for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum StorageCategoryId {
    Threads,
    ArchivedThreads,
    Workspace,
    Skills,
    Memories,
    Settings,
    Logs,
    PluginCache,
    Temporary,
    Credentials,
    Other,
}

impl StorageCategoryId {
    pub const ALL: [StorageCategoryId; 11] = [
        StorageCategoryId::Threads,
        StorageCategoryId::ArchivedThreads,
        StorageCategoryId::Workspace,
        StorageCategoryId::Skills,
        StorageCategoryId::Memories,
        StorageCategoryId::Settings,
        StorageCategoryId::Logs,
        StorageCategoryId::PluginCache,
        StorageCategoryId::Temporary,
        StorageCategoryId::Credentials,
        StorageCategoryId::Other,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            StorageCategoryId::Threads => "threads",
            StorageCategoryId::ArchivedThreads => "archived-threads",
            StorageCategoryId::Workspace => "workspace",
            StorageCategoryId::Skills => "skills",
            StorageCategoryId::Memories => "memories",
            StorageCategoryId::Settings => "settings",
            StorageCategoryId::Logs => "logs",
            StorageCategoryId::PluginCache => "plugin-cache",
            StorageCategoryId::Temporary => "temporary",
            StorageCategoryId::Credentials => "credentials",
            StorageCategoryId::Other => "other",
        }
    }

    pub fn parse(value: &str) -> Option<StorageCategoryId> {
        StorageCategoryId::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == value)
    }

    /// Where the category lives, relative to the resolved home. Always a
    /// literal: nothing here is built from user input.
    fn relative_paths(self) -> &'static [&'static str] {
        match self {
            // The contracts file is per-conversation runtime state -- which
            // folder each chat runs in and which permission it was given. It
            // belongs with the conversations it describes: counted with them,
            // copied with them, and gone when they are.
            StorageCategoryId::Threads => &["sessions", "thread-runtime-contracts.json"],
            StorageCategoryId::ArchivedThreads => &["archived_sessions"],
            StorageCategoryId::Workspace => &["neutral-workspace"],
            StorageCategoryId::Skills => &["skills"],
            // Memory is three things on disk, not one: the consolidated files,
            // the extension resources beside them, and the database the
            // pipeline extracts into. Counting only the first would tell a
            // person their memory is small and would leave the database out of
            // an export that is supposed to be their data.
            StorageCategoryId::Memories => &[
                "memories",
                "memories_extensions",
                "memories_1.sqlite",
                "memories_1.sqlite-wal",
                "memories_1.sqlite-shm",
            ],
            StorageCategoryId::Settings => &["config.toml", "settings.json", "hooks.json"],
            StorageCategoryId::Logs => &["log"],
            StorageCategoryId::PluginCache => &["plugins/cache"],
            StorageCategoryId::Temporary => &[".tmp"],
            StorageCategoryId::Credentials => &["auth.json"],
            // Found by looking, not by naming: see `measure_the_rest`.
            StorageCategoryId::Other => &[],
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            StorageCategoryId::Threads => "Conversations",
            StorageCategoryId::ArchivedThreads => "Archived conversations",
            StorageCategoryId::Workspace => "Files with no folder",
            StorageCategoryId::Skills => "Skills you added",
            StorageCategoryId::Memories => "Memory",
            StorageCategoryId::Settings => "Settings",
            StorageCategoryId::Logs => "Logs",
            StorageCategoryId::PluginCache => "Plugin cache",
            StorageCategoryId::Temporary => "Temporary files",
            StorageCategoryId::Credentials => "Sign-in",
            StorageCategoryId::Other => "Everything else",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            StorageCategoryId::Threads => {
                "The local record of your conversations, including the turns the model ran."
            }
            StorageCategoryId::ArchivedThreads => "Conversations you archived.",
            StorageCategoryId::Workspace => {
                "Files Whisply wrote for chats that were not pointed at a folder of yours."
            }
            StorageCategoryId::Skills => "Skill packages stored under this home.",
            StorageCategoryId::Memories => {
                "What Whisply remembers between conversations, and the database it is built from."
            }
            StorageCategoryId::Settings => "Your configuration, preferences, and hooks.",
            StorageCategoryId::Logs => "Local diagnostic logs, written only when you turn them on.",
            StorageCategoryId::PluginCache => "Downloaded plugin data Whisply can fetch again.",
            StorageCategoryId::Temporary => "Scratch files from work already finished.",
            StorageCategoryId::Credentials => {
                "Your signed-in session. Sign out to remove it; deleting the file here would \
                 leave the app believing it is still signed in."
            }
            StorageCategoryId::Other => {
                "Anything else Whisply keeps here, counted so this page adds up to what is \
                 really on disk."
            }
        }
    }

    /// Whether `clean` may remove it.
    ///
    /// Skills and settings are the person's own work with their own editing
    /// surfaces, and the sign-in is owned by signing out; deleting any of them
    /// from a storage screen would be a second, quieter path to destroying
    /// something they built. Memory is different: nobody typed it, it
    /// accumulates on its own from past conversations, and until it was
    /// listed here the only way to be rid of it was a hidden debug command.
    pub fn is_removable(self) -> bool {
        matches!(
            self,
            StorageCategoryId::Threads
                | StorageCategoryId::ArchivedThreads
                | StorageCategoryId::Memories
                | StorageCategoryId::Logs
                | StorageCategoryId::PluginCache
                | StorageCategoryId::Temporary
        )
    }

    /// Whether removing it needs more than deleting files.
    ///
    /// Memory lives in a database as well as on disk, and the database is the
    /// half that would rebuild the other. Its removal is owned by the memory
    /// subsystem, not by this file remover.
    pub fn needs_owner_removal(self) -> bool {
        matches!(self, StorageCategoryId::Memories)
    }

    /// Whether `export` copies it out.
    ///
    /// The sign-in is never exported: an export is a file the person may email
    /// themselves, and a live credential does not belong in one. `Other` is
    /// never exported either, for a different reason: it is whatever happens
    /// to be in the home, which is the one thing this file will not copy or
    /// delete from a path it discovered rather than named.
    pub fn is_exportable(self) -> bool {
        !matches!(
            self,
            StorageCategoryId::Credentials
                | StorageCategoryId::PluginCache
                | StorageCategoryId::Temporary
                | StorageCategoryId::Other
        )
    }
}

/// What one category currently holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StorageCategory {
    pub id: StorageCategoryId,
    pub paths: Vec<PathBuf>,
    pub bytes: u64,
    pub items: usize,
    pub present: bool,
    /// Whether the walk reached everything in the category.
    ///
    /// False means `bytes` and `items` are a floor, not a total: the walk hit
    /// its bound, refused to descend further, or could not read something. A
    /// surface that prints a floor as an exact figure tells someone their
    /// conversations take 1.2 GB when they take more, and asks them to agree
    /// to deleting a count that is not the count.
    pub complete: bool,
}

/// The whole local store for one resolved home.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StorageInventory {
    pub home: PathBuf,
    pub categories: Vec<StorageCategory>,
}

impl StorageInventory {
    pub fn total_bytes(&self) -> u64 {
        self.categories.iter().map(|category| category.bytes).sum()
    }

    /// Whether every category was counted in full. A single bounded category
    /// makes the total a floor too.
    pub fn is_complete(&self) -> bool {
        self.categories.iter().all(|category| category.complete)
    }

    pub fn category(&self, id: StorageCategoryId) -> Option<&StorageCategory> {
        self.categories.iter().find(|category| category.id == id)
    }
}

/// What a removal did, or would do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemovalOutcome {
    pub id: StorageCategoryId,
    pub removed_paths: Vec<PathBuf>,
    pub bytes: u64,
    pub items: usize,
    pub performed: bool,
    /// Whether the figures above account for everything removed. The removal
    /// itself is not bounded -- a directory's contents go whether or not the
    /// walk reached them -- so a false here means more was deleted than the
    /// number said, which is the worse way for a deletion figure to be wrong.
    pub complete: bool,
}

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("the Whisply home must be an absolute path: {0}")]
    HomeMustBeAbsolute(PathBuf),
    #[error("refusing to treat {0} as the Whisply home: it is a root or a home directory")]
    HomeTooBroad(PathBuf),
    #[error("refusing a symbolic-link storage path: {0}")]
    SymlinkPath(PathBuf),
    #[error("{0} resolves outside the Whisply home and will not be touched")]
    OutsideHome(PathBuf),
    #[error("refusing to remove the Whisply home itself: {0}")]
    HomeItself(PathBuf),
    #[error("{0} belongs to another product's home and is not Whisply's to delete")]
    ForeignHome(PathBuf),
    #[error("unknown storage category: {0}")]
    UnknownCategory(String),
    #[error("{0} is not something Whisply can delete for you")]
    NotRemovable(&'static str),
    #[error("{0} is removed by the part of Whisply that owns it, not by deleting files")]
    OwnerRemovesIt(&'static str),
    #[error("the export destination must be an absolute path: {0}")]
    ExportMustBeAbsolute(PathBuf),
    #[error("the export destination is inside the Whisply home: {0}")]
    ExportInsideHome(PathBuf),
    #[error("the export destination already has contents: {0}")]
    ExportNotEmpty(PathBuf),
    #[error("failed to read or write {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

fn io_error(path: &Path, source: io::Error) -> StorageError {
    StorageError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Names a real user home directory carries. Two of them together is not a
/// coincidence, and it is not a Whisply home.
const USER_HOME_SIGNATURE: [&str; 6] = [
    "Desktop",
    "Documents",
    "Downloads",
    "Library",
    "Movies",
    "Pictures",
];

/// Canonicalizes the home and refuses the ones no deletion should ever run in.
///
/// A home that turns out to be `/`, or the person's own home directory, means
/// something upstream resolved wrong. Continuing from there is how a storage
/// cleaner becomes the thing that empties a machine.
///
/// The user-home test reads the directory rather than `$HOME`, because the app
/// deliberately launches the runtime with `HOME` set to the account home; a
/// comparison against the environment would refuse every operation the app
/// starts, and would also miss any home resolved by another route.
pub fn resolve_home(home: &Path) -> Result<PathBuf, StorageError> {
    if !home.is_absolute() {
        return Err(StorageError::HomeMustBeAbsolute(home.to_path_buf()));
    }
    let metadata = fs::symlink_metadata(home).map_err(|source| io_error(home, source))?;
    if metadata.file_type().is_symlink() {
        return Err(StorageError::SymlinkPath(home.to_path_buf()));
    }
    let canonical = home
        .canonicalize()
        .map_err(|source| io_error(home, source))?;

    let depth = canonical.components().filter(is_normal_component).count();
    if depth < 2 {
        return Err(StorageError::HomeTooBroad(canonical));
    }
    if looks_like_a_user_home(&canonical) {
        return Err(StorageError::HomeTooBroad(canonical));
    }
    Ok(canonical)
}

fn is_normal_component(component: &Component<'_>) -> bool {
    matches!(component, Component::Normal(_))
}

fn looks_like_a_user_home(path: &Path) -> bool {
    let matches = USER_HOME_SIGNATURE
        .iter()
        .filter(|name| path.join(name).is_dir())
        .count();
    matches >= 2
}

/// Resolves one literal relative location inside the home, or explains why it
/// will not be touched.
///
/// `Ok(None)` means the location simply does not exist yet, which is the
/// ordinary case for a fresh install and is not an error.
fn resolve_within_home(home: &Path, relative: &str) -> Result<Option<PathBuf>, StorageError> {
    // These are literals in this file, not input. Checking them anyway means a
    // future edit that introduces a glob, a variable, or a parent traversal
    // fails here instead of in someone's home directory.
    let rejected = relative.is_empty()
        || relative.starts_with('/')
        || relative.starts_with('~')
        || relative.contains("..")
        || relative.contains(|character: char| {
            matches!(character, '*' | '?' | '[' | ']' | '{' | '}' | '$')
        });
    if rejected {
        return Err(StorageError::OutsideHome(PathBuf::from(relative)));
    }

    let candidate = home.join(relative);
    let Ok(metadata) = fs::symlink_metadata(&candidate) else {
        return Ok(None);
    };
    if metadata.file_type().is_symlink() {
        return Err(StorageError::SymlinkPath(candidate));
    }
    let canonical = candidate
        .canonicalize()
        .map_err(|source| io_error(&candidate, source))?;
    if canonical == home {
        return Err(StorageError::HomeItself(canonical));
    }
    if !canonical.starts_with(home) {
        return Err(StorageError::OutsideHome(canonical));
    }
    if canonical
        .components()
        .any(|component| component.as_os_str() == ".codex")
    {
        return Err(StorageError::ForeignHome(canonical));
    }
    Ok(Some(canonical))
}

/// Reads what each category currently holds.
pub fn inventory(home: &Path) -> Result<StorageInventory, StorageError> {
    let home = resolve_home(home)?;
    let mut categories = Vec::with_capacity(StorageCategoryId::ALL.len());
    for id in StorageCategoryId::ALL {
        if id == StorageCategoryId::Other {
            categories.push(measure_the_rest(&home)?);
            continue;
        }
        let mut paths = Vec::new();
        let mut total = Measurement::default();
        for relative in id.relative_paths() {
            let Some(path) = resolve_within_home(&home, relative)? else {
                continue;
            };
            total.add(measure(&path)?);
            paths.push(path);
        }
        categories.push(StorageCategory {
            id,
            present: !paths.is_empty(),
            paths,
            bytes: total.bytes,
            items: total.items,
            complete: total.complete,
        });
    }
    Ok(StorageInventory { home, categories })
}

/// Counts what is in the home that no category above speaks for.
///
/// Every other category is a literal name written in this file, which is what
/// makes deleting from them safe. It also means a page built only from those
/// names describes the home someone wrote down once, not the home that is
/// there: anything added later -- by another part of the product, by a tool,
/// by a future version -- is simply absent, and the total is quietly short by
/// however much it holds. That is how a person's own working files came to be
/// stored here and shown nowhere.
///
/// So this one is found by looking. Nothing discovered this way is ever
/// deleted or copied: `Other` is neither removable nor exportable, and the
/// paths exist so the count can be explained, not acted on.
///
/// A top-level entry counts as claimed by its name alone. A category that
/// names `plugins/cache` speaks for `plugins`, so the rest of that directory
/// is the plugin subsystem's business and not loose change to report here.
fn measure_the_rest(home: &Path) -> Result<StorageCategory, StorageError> {
    let mut claimed: BTreeSet<OsString> = BTreeSet::new();
    for id in StorageCategoryId::ALL {
        for relative in id.relative_paths() {
            if let Some(first) = Path::new(relative).components().find(is_normal_component) {
                claimed.insert(first.as_os_str().to_os_string());
            }
        }
    }

    let mut paths = Vec::new();
    let mut total = Measurement::default();
    match fs::read_dir(home) {
        Ok(entries) => {
            for entry in entries {
                let Ok(entry) = entry else {
                    total.complete = false;
                    continue;
                };
                if claimed.contains(&entry.file_name()) {
                    continue;
                }
                total.add(measure(&entry.path())?);
                paths.push(entry.path());
            }
        }
        Err(source) => return Err(io_error(home, source)),
    }
    paths.sort();

    Ok(StorageCategory {
        id: StorageCategoryId::Other,
        present: !paths.is_empty(),
        paths,
        bytes: total.bytes,
        items: total.items,
        complete: total.complete,
    })
}

#[derive(Clone, Copy, Debug)]
struct Measurement {
    bytes: u64,
    items: usize,
    complete: bool,
}

impl Default for Measurement {
    fn default() -> Self {
        Self {
            bytes: 0,
            items: 0,
            complete: true,
        }
    }
}

impl Measurement {
    fn add(&mut self, other: Measurement) {
        self.bytes += other.bytes;
        self.items += other.items;
        self.complete &= other.complete;
    }
}

/// Adds up one location, reporting whether it managed to see all of it.
///
/// Anything left out is recorded rather than absorbed: an entry past the
/// bound, a branch too deep to descend, a directory that would not open. The
/// alternative is a number that looks exact and is not, which is worse than a
/// number that admits it is a floor.
fn measure(path: &Path) -> Result<Measurement, StorageError> {
    let mut total = Measurement::default();
    let mut pending = vec![(path.to_path_buf(), 0usize)];
    while let Some((current, depth)) = pending.pop() {
        if total.items >= MAX_WALK_ENTRIES {
            // Everything still queued is real and uncounted, including the
            // entry just taken.
            total.complete = false;
            break;
        }
        if depth > MAX_WALK_DEPTH {
            // Skip this branch, not the rest of the walk. Abandoning the whole
            // queue here would let one deep directory shrink an entire
            // category to whatever happened to be counted first.
            total.complete = false;
            continue;
        }
        let metadata = match fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(_) => {
                total.complete = false;
                continue;
            }
        };
        if metadata.file_type().is_symlink() {
            // Counted as an entry, never followed: a link's target may be
            // outside the home, and its bytes are not Whisply's to claim.
            total.items += 1;
            continue;
        }
        if metadata.is_dir() {
            let entries = match fs::read_dir(&current) {
                Ok(entries) => entries,
                Err(_) => {
                    total.complete = false;
                    continue;
                }
            };
            for entry in entries {
                match entry {
                    Ok(entry) => pending.push((entry.path(), depth + 1)),
                    Err(_) => total.complete = false,
                }
            }
            continue;
        }
        total.items += 1;
        total.bytes += metadata.len();
    }
    Ok(total)
}

/// Removes one category, or reports what removing it would do.
///
/// A directory's contents are removed rather than the directory itself, so the
/// user-only permissions the runtime established stay in place and the next
/// write does not have to recreate them.
pub fn remove_category(
    home: &Path,
    id: StorageCategoryId,
    perform: bool,
) -> Result<RemovalOutcome, StorageError> {
    if !id.is_removable() {
        return Err(StorageError::NotRemovable(id.title()));
    }
    if id.needs_owner_removal() {
        return Err(StorageError::OwnerRemovesIt(id.title()));
    }
    let home = resolve_home(home)?;
    let mut outcome = RemovalOutcome {
        id,
        removed_paths: Vec::new(),
        bytes: 0,
        items: 0,
        performed: perform,
        complete: true,
    };
    for relative in id.relative_paths() {
        let Some(path) = resolve_within_home(&home, relative)? else {
            continue;
        };
        let measured = measure(&path)?;
        outcome.bytes += measured.bytes;
        outcome.items += measured.items;
        outcome.complete &= measured.complete;
        outcome.removed_paths.push(path.clone());
        if !perform {
            continue;
        }
        if path.is_dir() {
            for entry in fs::read_dir(&path)
                .map_err(|source| io_error(&path, source))?
                .flatten()
            {
                remove_entry(&entry.path())?;
            }
        } else {
            remove_entry(&path)?;
        }
    }
    Ok(outcome)
}

fn remove_entry(path: &Path) -> Result<(), StorageError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| io_error(path, source))?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        fs::remove_dir_all(path).map_err(|source| io_error(path, source))
    } else {
        fs::remove_file(path).map_err(|source| io_error(path, source))
    }
}

/// How much local log a machine keeps before the oldest of it is dropped.
pub const MAX_LOG_FILE_BYTES: u64 = 8 * 1024 * 1024;

/// Bounds a local log file by rotating it once it passes `max_bytes`.
///
/// Retention is enforced when the file is opened, so at most one previous run's
/// log survives alongside the current one. This does not bound a single very
/// long session, which keeps appending until it next starts; the reason to do
/// it here anyway is that the unbounded case in practice is a log that has been
/// growing across months of launches.
pub fn enforce_log_retention(path: &Path, max_bytes: u64) -> Result<(), StorageError> {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return Ok(());
    };
    if metadata.file_type().is_symlink() {
        // Rotating through a link would rename or delete whatever it points
        // at, which may not be a log and may not be ours.
        return Err(StorageError::SymlinkPath(path.to_path_buf()));
    }
    if !metadata.is_file() || metadata.len() <= max_bytes {
        return Ok(());
    }
    let previous = PathBuf::from(format!("{}.1", path.display()));
    let _ = fs::remove_file(&previous);
    fs::rename(path, &previous).map_err(|source| io_error(path, source))
}

/// What an export wrote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportOutcome {
    pub destination: PathBuf,
    pub categories: Vec<StorageCategoryId>,
    pub bytes: u64,
    pub items: usize,
}

/// Copies the exportable categories into a new directory outside the home.
pub fn export(home: &Path, destination: &Path) -> Result<ExportOutcome, StorageError> {
    let home = resolve_home(home)?;
    if !destination.is_absolute() {
        return Err(StorageError::ExportMustBeAbsolute(
            destination.to_path_buf(),
        ));
    }
    if let Ok(metadata) = fs::symlink_metadata(destination) {
        if metadata.file_type().is_symlink() {
            return Err(StorageError::SymlinkPath(destination.to_path_buf()));
        }
        let has_contents = fs::read_dir(destination)
            .map_err(|source| io_error(destination, source))?
            .next()
            .is_some();
        if has_contents {
            return Err(StorageError::ExportNotEmpty(destination.to_path_buf()));
        }
    } else {
        fs::create_dir_all(destination).map_err(|source| io_error(destination, source))?;
    }
    let canonical_destination = destination
        .canonicalize()
        .map_err(|source| io_error(destination, source))?;
    if canonical_destination.starts_with(&home) {
        return Err(StorageError::ExportInsideHome(canonical_destination));
    }
    private_directory(&canonical_destination)?;

    let mut outcome = ExportOutcome {
        destination: canonical_destination.clone(),
        categories: Vec::new(),
        bytes: 0,
        items: 0,
    };
    for id in StorageCategoryId::ALL {
        if !id.is_exportable() {
            continue;
        }
        let mut copied_any = false;
        for relative in id.relative_paths() {
            let Some(source) = resolve_within_home(&home, relative)? else {
                continue;
            };
            let target = canonical_destination.join(relative);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(|source| io_error(parent, source))?;
            }
            let measured = copy_tree(&source, &target)?;
            outcome.bytes += measured.bytes;
            outcome.items += measured.items;
            copied_any = true;
        }
        if copied_any {
            outcome.categories.push(id);
        }
    }
    Ok(outcome)
}

fn private_directory(path: &Path) -> Result<(), StorageError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|source| io_error(path, source))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

fn copy_tree(source: &Path, target: &Path) -> Result<Measurement, StorageError> {
    let metadata =
        fs::symlink_metadata(source).map_err(|source_error| io_error(source, source_error))?;
    if metadata.file_type().is_symlink() {
        // Never followed. A link inside the home can point anywhere, and an
        // export that resolves it would copy something the person did not ask
        // to export.
        return Ok(Measurement::default());
    }
    if metadata.is_file() {
        fs::copy(source, target).map_err(|source_error| io_error(source, source_error))?;
        return Ok(Measurement {
            bytes: metadata.len(),
            items: 1,
            complete: true,
        });
    }
    fs::create_dir_all(target).map_err(|source_error| io_error(target, source_error))?;
    private_directory(target)?;
    let mut total = Measurement::default();
    for entry in fs::read_dir(source)
        .map_err(|source_error| io_error(source, source_error))?
        .flatten()
    {
        let child_target = target.join(entry.file_name());
        total.add(copy_tree(&entry.path(), &child_target)?);
    }
    Ok(total)
}

#[cfg(test)]
#[path = "local_storage_tests.rs"]
mod tests;

use super::*;
use std::fs;
use tempfile::TempDir;

fn home_with_contents() -> (TempDir, PathBuf) {
    let temporary = TempDir::new().expect("temporary root");
    let home = temporary.path().join("product").join(".whisply");
    fs::create_dir_all(home.join("sessions")).expect("sessions");
    fs::write(home.join("sessions").join("one.jsonl"), "abcd").expect("thread");
    fs::create_dir_all(home.join("log")).expect("log");
    fs::write(home.join("log").join("tui.log"), "log line").expect("log file");
    fs::create_dir_all(home.join("skills").join("writer")).expect("skills");
    fs::write(
        home.join("skills").join("writer").join("SKILL.md"),
        "# Instructions",
    )
    .expect("skill");
    fs::write(home.join("auth.json"), "{\"token\":\"secret-value\"}").expect("auth");
    fs::write(home.join("config.toml"), "model = \"whisply\"").expect("config");
    (temporary, home)
}

#[test]
fn the_inventory_reports_what_each_category_holds() {
    let (_temporary, home) = home_with_contents();

    let inventory = inventory(&home).expect("inventory");

    let threads = inventory
        .category(StorageCategoryId::Threads)
        .expect("threads");
    assert!(threads.present);
    assert_eq!(threads.items, 1);
    assert_eq!(threads.bytes, 4);

    let memories = inventory
        .category(StorageCategoryId::Memories)
        .expect("memories");
    assert!(!memories.present, "an absent category is not an error");
    assert_eq!(memories.items, 0);

    assert!(inventory.total_bytes() > 0);
}

/// The sign-in file is listed, because pretending it is not there would be a
/// different kind of dishonesty, but it is never copied into an export and
/// never deleted from here.
#[test]
fn the_sign_in_is_visible_but_neither_exported_nor_deleted() {
    let (temporary, home) = home_with_contents();

    let inventory = inventory(&home).expect("inventory");
    let credentials = inventory
        .category(StorageCategoryId::Credentials)
        .expect("credentials");
    assert!(credentials.present);

    let error = remove_category(&home, StorageCategoryId::Credentials, true)
        .expect_err("credentials must not be removable");
    assert!(matches!(error, StorageError::NotRemovable(_)));

    let destination = temporary.path().join("export");
    export(&home, &destination).expect("export");
    assert!(
        !destination.join("auth.json").exists(),
        "an export must never carry a live credential"
    );
    assert!(destination.join("config.toml").exists());
    assert!(destination.join("skills").join("writer").exists());
}

#[test]
fn a_dry_run_reports_the_same_work_it_would_do_and_changes_nothing() {
    let (_temporary, home) = home_with_contents();

    let planned = remove_category(&home, StorageCategoryId::Threads, false).expect("plan");
    assert!(!planned.performed);
    assert_eq!(planned.items, 1);
    assert!(home.join("sessions").join("one.jsonl").exists());

    let performed = remove_category(&home, StorageCategoryId::Threads, true).expect("remove");
    assert!(performed.performed);
    assert_eq!(performed.items, planned.items);
    assert_eq!(performed.bytes, planned.bytes);
    assert!(!home.join("sessions").join("one.jsonl").exists());
    assert!(
        home.join("sessions").is_dir(),
        "the private directory itself stays, so its permissions survive"
    );
}

/// Skills and settings have their own editing surfaces. A storage screen that
/// could delete them would be a second, quieter way to destroy work someone
/// built. Memory is not in that group: nobody typed it.
#[test]
fn a_persons_own_work_is_not_deletable_from_here() {
    let (_temporary, home) = home_with_contents();

    for id in [StorageCategoryId::Skills, StorageCategoryId::Settings] {
        let error =
            remove_category(&home, id, true).expect_err("must not be removable from storage");
        assert!(matches!(error, StorageError::NotRemovable(_)), "{id:?}");
    }
    assert!(home.join("skills").join("writer").join("SKILL.md").exists());
    assert!(home.join("config.toml").exists());
}

#[test]
fn a_home_that_resolved_to_something_too_broad_is_refused() {
    let root = Path::new("/");
    let error = inventory(root).expect_err("the filesystem root is never a home");
    assert!(matches!(error, StorageError::HomeTooBroad(_)));

    let relative = Path::new("relative/home");
    let error = inventory(relative).expect_err("a relative home is never resolved");
    assert!(matches!(error, StorageError::HomeMustBeAbsolute(_)));
}

/// If the resolved home turns out to be a person's own home directory,
/// something upstream is wrong, and continuing is how a cleanup becomes a
/// catastrophe. Recognised by what the directory contains, because the app
/// deliberately runs the runtime with `HOME` set to the account home.
#[test]
fn a_directory_that_is_really_someones_home_is_never_treated_as_the_whisply_home() {
    let temporary = TempDir::new().expect("temporary root");
    let user_home = temporary.path().join("someone");
    fs::create_dir_all(user_home.join("Desktop")).expect("desktop");
    fs::create_dir_all(user_home.join("Documents")).expect("documents");
    fs::create_dir_all(user_home.join("sessions")).expect("sessions");

    let error = inventory(&user_home).expect_err("a user home is not a storage root");
    assert!(matches!(error, StorageError::HomeTooBroad(_)), "{error}");
    assert!(
        user_home.join("sessions").is_dir(),
        "nothing may be removed from a home that was refused"
    );
}

/// A symlink inside the home can point anywhere. Following one during a delete
/// is how a bounded cleanup reaches outside its bounds.
#[cfg(unix)]
#[test]
fn a_symlinked_category_is_refused_rather_than_followed() {
    use std::os::unix::fs::symlink;

    let temporary = TempDir::new().expect("temporary root");
    let home = temporary.path().join("product").join(".whisply");
    fs::create_dir_all(&home).expect("home");
    let elsewhere = temporary.path().join("elsewhere");
    fs::create_dir_all(&elsewhere).expect("elsewhere");
    fs::write(elsewhere.join("precious.txt"), "keep me").expect("file");
    symlink(&elsewhere, home.join("sessions")).expect("symlink");

    let error = inventory(&home).expect_err("a symlinked category is refused");
    assert!(matches!(error, StorageError::SymlinkPath(_)), "{error}");

    let error = remove_category(&home, StorageCategoryId::Threads, true)
        .expect_err("a symlinked category is never deleted through");
    assert!(matches!(error, StorageError::SymlinkPath(_)), "{error}");
    assert!(
        elsewhere.join("precious.txt").exists(),
        "nothing outside the home may be touched"
    );
}

#[test]
fn an_export_refuses_to_write_inside_the_home_it_is_copying() {
    let (_temporary, home) = home_with_contents();

    let error = export(&home, &home.join("export")).expect_err("must refuse");
    assert!(
        matches!(error, StorageError::ExportInsideHome(_)),
        "{error}"
    );
}

#[test]
fn an_export_refuses_a_destination_that_already_has_contents() {
    let (temporary, home) = home_with_contents();
    let destination = temporary.path().join("export");
    fs::create_dir_all(&destination).expect("destination");
    fs::write(destination.join("existing.txt"), "mine").expect("existing");

    let error = export(&home, &destination).expect_err("must refuse");
    assert!(matches!(error, StorageError::ExportNotEmpty(_)), "{error}");
    assert_eq!(
        fs::read_to_string(destination.join("existing.txt")).expect("read"),
        "mine"
    );
}

/// The page is supposed to say what Whisply keeps on this machine. Built only
/// from names written here, it says what someone wrote down once, and anything
/// added to the home since is missing from the list and from the total.
#[test]
fn what_no_category_speaks_for_is_counted_rather_than_left_out() {
    let (_temporary, home) = home_with_contents();
    fs::create_dir_all(home.join("something-added-later")).expect("later");
    fs::write(
        home.join("something-added-later").join("data.bin"),
        vec![b'x'; 500],
    )
    .expect("later file");

    let inventory = inventory(&home).expect("inventory");
    let rest = inventory.category(StorageCategoryId::Other).expect("other");

    assert!(rest.present, "a directory nothing names went unreported");
    assert_eq!(rest.bytes, 500);
    assert!(
        rest.paths
            .contains(&inventory.home.join("something-added-later")),
        "the count cannot be explained: {:?}",
        rest.paths
    );
    assert!(
        inventory.total_bytes() >= 500,
        "the total is short by everything no category names"
    );
}

/// Reporting the same bytes twice would be its own kind of wrong: a person
/// reading the page would see a total larger than the folder.
#[test]
fn what_a_category_already_speaks_for_is_not_counted_twice() {
    let (_temporary, home) = home_with_contents();
    fs::create_dir_all(home.join("plugins").join("cache")).expect("plugin cache");
    fs::write(home.join("plugins").join("cache").join("blob"), "cached").expect("cached");
    fs::write(home.join("plugins").join("installed.json"), "[]").expect("installed");

    let inventory = inventory(&home).expect("inventory");
    let rest = inventory.category(StorageCategoryId::Other).expect("other");

    for claimed in ["sessions", "log", "skills", "auth.json", "config.toml"] {
        assert!(
            !rest.paths.contains(&inventory.home.join(claimed)),
            "{claimed} is already listed under its own heading"
        );
    }
    assert!(
        !rest.paths.contains(&inventory.home.join("plugins")),
        "a directory a category speaks for is that subsystem's, not loose change"
    );
}

/// Everything else is a count, not a target. The safety of this file rests on
/// callers naming a category whose locations are literals written here, and a
/// path that was found by reading a directory is neither.
#[test]
fn what_was_found_by_looking_is_never_deleted_or_copied() {
    let (temporary, home) = home_with_contents();
    fs::write(home.join("unknown-thing"), "not ours to move").expect("unknown");

    let error = remove_category(&home, StorageCategoryId::Other, true).expect_err("must refuse");
    assert!(matches!(error, StorageError::NotRemovable(_)), "{error}");
    assert!(home.join("unknown-thing").exists());

    let destination = temporary.path().join("export");
    export(&home, &destination).expect("export");
    assert!(
        !destination.join("unknown-thing").exists(),
        "an export carried a file this module never named"
    );
}

/// A chat that was not pointed at a folder still does work, and the files it
/// writes are the person's. They were being kept in the runtime home where
/// nothing listed them, nothing exported them, and nothing could clear them.
#[test]
fn work_from_chats_with_no_folder_is_listed_and_exported() {
    let (temporary, home) = home_with_contents();
    fs::create_dir_all(home.join("neutral-workspace")).expect("workspace");
    fs::write(
        home.join("neutral-workspace").join("draft-contract.md"),
        "the work itself",
    )
    .expect("work");

    let inventory = inventory(&home).expect("inventory");
    let workspace = inventory
        .category(StorageCategoryId::Workspace)
        .expect("workspace");
    assert!(workspace.present);
    assert_eq!(workspace.items, 1);

    let error = remove_category(&home, StorageCategoryId::Workspace, true)
        .expect_err("someone's work is not accumulation");
    assert!(matches!(error, StorageError::NotRemovable(_)), "{error}");

    let destination = temporary.path().join("export");
    export(&home, &destination).expect("export");
    assert!(
        destination
            .join("neutral-workspace")
            .join("draft-contract.md")
            .exists(),
        "an export of a person's data left out the work Whisply did for them"
    );
}

/// Which folder each conversation runs in, and what it was allowed to do,
/// belongs to that conversation: counted with it, copied with it, and gone
/// when it is.
#[test]
fn where_each_conversation_runs_is_kept_with_the_conversations() {
    let (_temporary, home) = home_with_contents();
    let contracts = home.join("thread-runtime-contracts.json");
    fs::write(&contracts, "{\"version\":1,\"contracts\":{}}").expect("contracts");

    let inventory = inventory(&home).expect("inventory");
    let threads = inventory
        .category(StorageCategoryId::Threads)
        .expect("threads");
    let canonical = inventory.home.join("thread-runtime-contracts.json");
    assert!(
        threads.paths.contains(&canonical),
        "the per-conversation directory choices are listed nowhere: {:?}",
        threads.paths
    );
    let rest = inventory.category(StorageCategoryId::Other).expect("other");
    assert!(!rest.paths.contains(&canonical));

    remove_category(&home, StorageCategoryId::Threads, true).expect("remove");
    assert!(
        !contracts.exists(),
        "clearing conversations left behind the folders they were pointed at"
    );
}

/// Deleting and copying are only safe because every location they touch is a
/// literal written in this file. A category counted by looking at the home has
/// no such list, so it may be reported and nothing more -- and that has to hold
/// for whatever is counted that way next, not just for the one that is today.
#[test]
fn a_category_with_no_written_locations_may_only_be_counted() {
    for id in StorageCategoryId::ALL {
        if !id.relative_paths().is_empty() {
            continue;
        }
        assert!(
            !id.is_removable(),
            "{id:?} is deletable but names no location, so the deletion would \
             have to act on a path it found by looking"
        );
        assert!(
            !id.is_exportable(),
            "{id:?} is exported but names no location, so the export would have \
             to copy a path it found by looking"
        );
    }
}

#[test]
fn every_category_location_is_a_literal_inside_the_home() {
    for id in StorageCategoryId::ALL {
        for relative in id.relative_paths() {
            assert!(!relative.is_empty(), "{id:?}");
            assert!(!relative.starts_with('/'), "{id:?} {relative}");
            assert!(!relative.starts_with('~'), "{id:?} {relative}");
            assert!(!relative.contains(".."), "{id:?} {relative}");
            assert!(
                !relative.contains(['*', '?', '[', ']', '{', '}', '$']),
                "{id:?} {relative}"
            );
        }
    }
}

#[test]
fn a_log_that_outgrew_its_bound_is_rotated_once_and_no_further() {
    let temporary = TempDir::new().expect("temporary root");
    let log = temporary.path().join("whisply-tui.log");
    fs::write(&log, vec![b'x'; 64]).expect("first log");

    enforce_log_retention(&log, 16).expect("rotate");
    assert!(!log.exists(), "the oversized log is moved aside");
    let previous = temporary.path().join("whisply-tui.log.1");
    assert_eq!(fs::metadata(&previous).expect("previous").len(), 64);

    fs::write(&log, vec![b'y'; 64]).expect("second log");
    enforce_log_retention(&log, 16).expect("rotate again");
    assert_eq!(
        fs::read_dir(temporary.path())
            .expect("read")
            .flatten()
            .count(),
        1,
        "only one previous log is ever kept"
    );
    assert_eq!(
        fs::read(&previous).expect("previous"),
        vec![b'y'; 64],
        "the newer log replaced the older one"
    );
}

#[test]
fn a_log_within_its_bound_is_left_alone() {
    let temporary = TempDir::new().expect("temporary root");
    let log = temporary.path().join("whisply-tui.log");
    fs::write(&log, b"short").expect("log");

    enforce_log_retention(&log, MAX_LOG_FILE_BYTES).expect("no rotation");

    assert_eq!(fs::read(&log).expect("log"), b"short");
    assert!(!temporary.path().join("whisply-tui.log.1").exists());
}

#[cfg(unix)]
#[test]
fn a_symlinked_log_path_is_refused_rather_than_rotated_through() {
    use std::os::unix::fs::symlink;

    let temporary = TempDir::new().expect("temporary root");
    let target = temporary.path().join("someone-elses.txt");
    fs::write(&target, vec![b'z'; 64]).expect("target");
    let log = temporary.path().join("whisply-tui.log");
    symlink(&target, &log).expect("symlink");

    let error = enforce_log_retention(&log, 16).expect_err("must refuse");
    assert!(matches!(error, StorageError::SymlinkPath(_)), "{error}");
    assert!(target.exists());
}

#[test]
fn an_unknown_category_name_resolves_to_nothing() {
    assert!(StorageCategoryId::parse("everything").is_none());
    assert!(StorageCategoryId::parse("../../etc").is_none());
    assert_eq!(
        StorageCategoryId::parse("threads"),
        Some(StorageCategoryId::Threads)
    );
}
/// Memory is not only the consolidated files. Counting the directory alone
/// tells a person their memory is a few kilobytes while the database it was
/// built from sits beside it, and leaves that database out of an export that is
/// supposed to be their data.
#[test]
fn the_memory_a_person_sees_includes_the_database_it_came_from() {
    let (temporary, home) = home_with_contents();
    fs::create_dir_all(home.join("memories")).expect("memories");
    fs::write(home.join("memories").join("MEMORY.md"), "remembered").expect("memory file");
    fs::create_dir_all(home.join("memories_extensions")).expect("memory extensions");
    fs::write(
        home.join("memories_extensions").join("resource.md"),
        "extension resource",
    )
    .expect("extension file");
    fs::write(home.join("memories_1.sqlite"), "sqlite database bytes").expect("memory database");

    let inventory = inventory(&home).expect("inventory");
    let memories = inventory
        .category(StorageCategoryId::Memories)
        .expect("memories");

    assert!(memories.present);
    assert_eq!(memories.items, 3, "all three parts of memory are counted");
    assert_eq!(
        memories.bytes,
        "remembered".len() as u64
            + "extension resource".len() as u64
            + "sqlite database bytes".len() as u64
    );

    let destination = temporary.path().join("export");
    export(&home, &destination).expect("export");
    assert!(destination.join("memories").join("MEMORY.md").exists());
    assert!(
        destination.join("memories_1.sqlite").exists(),
        "an export of a person's data includes the memory they accumulated"
    );
}

/// Memory can be deleted, but not by unlinking files. Its records live in a
/// database that would rebuild them, and that database may be open in another
/// process right now, so the file remover refuses and the memory subsystem does
/// the work.
#[test]
fn memory_is_deletable_but_not_by_this_file_remover() {
    let (_temporary, home) = home_with_contents();
    fs::create_dir_all(home.join("memories")).expect("memories");
    fs::write(home.join("memories").join("MEMORY.md"), "remembered").expect("memory file");
    fs::write(home.join("memories_1.sqlite"), "database").expect("memory database");

    assert!(StorageCategoryId::Memories.is_removable());
    assert!(StorageCategoryId::Memories.needs_owner_removal());

    let error = remove_category(&home, StorageCategoryId::Memories, true)
        .expect_err("the file remover must not touch memory");
    assert!(matches!(error, StorageError::OwnerRemovesIt(_)), "{error}");
    assert!(
        home.join("memories_1.sqlite").exists(),
        "a refused removal leaves the database alone"
    );
}

/// A deep branch is one branch. Skipping it used to abandon everything still
/// queued behind it, so a single nested directory could shrink an entire
/// category to whatever happened to be counted first -- and the person would
/// be told that floor was the total.
#[test]
fn one_branch_too_deep_to_follow_does_not_stop_the_rest_of_the_count() {
    let temporary = TempDir::new().expect("temporary root");
    let home = temporary.path().join("product").join(".whisply");
    let sessions = home.join("sessions");

    let mut deep = sessions.join("deep");
    for _ in 0..(MAX_WALK_DEPTH + 4) {
        deep = deep.join("down");
    }
    fs::create_dir_all(&deep).expect("deep branch");
    fs::write(deep.join("buried.jsonl"), "buried").expect("buried thread");

    // Shallow enough to count, and named so it is walked after the deep
    // branch: the walk is a stack, and the old bug threw away the queue.
    for index in 0..8 {
        fs::write(sessions.join(format!("{index}.jsonl")), "abcd").expect("thread");
    }

    let inventory = inventory(&home).expect("inventory");
    let threads = inventory
        .category(StorageCategoryId::Threads)
        .expect("threads");

    assert_eq!(
        threads.bytes,
        8 * 4,
        "every file the walk could reach is counted"
    );
    assert!(
        !threads.complete,
        "the buried file was not counted, and the figure says so"
    );
    assert!(
        !inventory.is_complete(),
        "one bounded category makes the total a floor too"
    );
}

/// The ordinary case has to stay unqualified, or `at least` appears next to
/// every number and stops meaning anything.
#[test]
fn a_home_that_was_counted_in_full_says_nothing_about_bounds() {
    let (_temporary, home) = home_with_contents();

    let inventory = inventory(&home).expect("inventory");

    assert!(inventory.is_complete());
    for category in &inventory.categories {
        assert!(category.complete, "{} was bounded", category.id.as_str());
    }
}

/// Deletion is not bounded the way counting is: the directory's contents go
/// whether or not the walk reached them. A removal that reports a floor as a
/// total is understating what it just destroyed.
#[test]
fn a_deletion_that_could_not_count_everything_it_removes_says_so() {
    let temporary = TempDir::new().expect("temporary root");
    let home = temporary.path().join("product").join(".whisply");
    let sessions = home.join("sessions");
    let mut deep = sessions.join("deep");
    for _ in 0..(MAX_WALK_DEPTH + 4) {
        deep = deep.join("down");
    }
    fs::create_dir_all(&deep).expect("deep branch");
    fs::write(deep.join("buried.jsonl"), "buried").expect("buried thread");
    fs::write(sessions.join("one.jsonl"), "abcd").expect("thread");

    let planned = remove_category(&home, StorageCategoryId::Threads, /*perform*/ false)
        .expect("planned removal");
    assert!(!planned.complete);

    let performed =
        remove_category(&home, StorageCategoryId::Threads, /*perform*/ true).expect("removal");
    assert!(!performed.complete);
    assert!(
        !deep.join("buried.jsonl").exists(),
        "the uncounted file is removed all the same, which is why the count has to admit it"
    );
}

use std::fs;
use std::path::Path;

use tempfile::TempDir;

use super::*;

fn plugin_package() -> TempDir {
    let package = tempfile::tempdir().expect("create plugin package");
    write(package.path(), "plugin.json", "{\"name\":\"sample\"}");
    write(package.path(), "server.js", "console.log('hello');");
    package
}

fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent directory");
    }
    fs::write(path, contents).expect("write file");
}

async fn hash(root: &Path) -> String {
    plugin_package_hash(root)
        .await
        .expect("hash the plugin package")
}

#[tokio::test]
async fn the_same_files_hash_the_same_way() {
    let package = plugin_package();
    assert_eq!(hash(package.path()).await, hash(package.path()).await);
}

#[tokio::test]
async fn rewriting_the_program_a_reviewed_command_line_starts_is_a_different_package() {
    let package = plugin_package();
    let approved = hash(package.path()).await;

    write(package.path(), "server.js", "require('child_process');");

    assert_ne!(
        approved,
        hash(package.path()).await,
        "an edited program kept the package it was approved as"
    );
}

#[tokio::test]
async fn a_file_added_after_the_review_is_part_of_the_package() {
    let package = plugin_package();
    let approved = hash(package.path()).await;

    write(package.path(), "lib/helper.js", "module.exports = {};");

    assert_ne!(approved, hash(package.path()).await);
}

#[tokio::test]
async fn a_file_removed_after_the_review_is_part_of_the_package() {
    let package = plugin_package();
    write(package.path(), "lib/helper.js", "module.exports = {};");
    let approved = hash(package.path()).await;

    fs::remove_file(package.path().join("lib/helper.js")).expect("remove file");

    assert_ne!(approved, hash(package.path()).await);
}

/// Two files with the same bytes under different names are two different
/// packages, and a hash that ran one field into the next could miss it.
#[tokio::test]
async fn where_a_file_sits_is_part_of_what_the_package_is() {
    let first = tempfile::tempdir().expect("create package");
    write(first.path(), "ab/c.js", "same");
    let second = tempfile::tempdir().expect("create package");
    write(second.path(), "a/bc.js", "same");

    assert_ne!(hash(first.path()).await, hash(second.path()).await);
}

#[cfg(unix)]
#[tokio::test]
async fn making_a_shipped_file_executable_changes_the_package() {
    use std::os::unix::fs::PermissionsExt;

    let package = plugin_package();
    let approved = hash(package.path()).await;

    let path = package.path().join("server.js");
    let mut permissions = fs::metadata(&path).expect("read metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).expect("set permissions");

    assert_ne!(
        approved,
        hash(package.path()).await,
        "a file that became runnable is the same package"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn repointing_a_link_changes_the_package_without_touching_a_file() {
    let package = plugin_package();
    write(package.path(), "real/one.js", "one");
    write(package.path(), "real/two.js", "two");
    std::os::unix::fs::symlink("real/one.js", package.path().join("entry.js"))
        .expect("create symlink");
    let approved = hash(package.path()).await;

    fs::remove_file(package.path().join("entry.js")).expect("remove symlink");
    std::os::unix::fs::symlink("real/two.js", package.path().join("entry.js"))
        .expect("create symlink");

    assert_ne!(approved, hash(package.path()).await);
}

/// The loader hands a plugin this directory to write in when the store has not
/// given it one. A server that writes state where it runs would otherwise
/// disable itself the next time it started.
#[tokio::test]
async fn what_the_plugin_writes_while_running_is_not_the_package_changing() {
    let package = plugin_package();
    write(package.path(), "lib/helper.js", "module.exports = {};");
    let approved = hash(package.path()).await;

    write(package.path(), ".plugin-data/state.json", "{\"runs\":1}");
    write(package.path(), ".git/FETCH_HEAD", "abc123");
    write(package.path(), ".DS_Store", "finder");
    write(package.path(), "lib/.DS_Store", "finder");

    assert_eq!(approved, hash(package.path()).await);
}

/// Only at the top level: the exclusion exists for the directory the loader
/// names, not for a name the package may not use for its own code.
#[tokio::test]
async fn a_plugin_data_directory_deeper_in_the_tree_is_still_code() {
    let package = plugin_package();
    let approved = hash(package.path()).await;

    write(package.path(), "lib/.plugin-data/run.js", "run()");

    assert_ne!(approved, hash(package.path()).await);
}

#[tokio::test]
async fn a_package_that_is_not_there_cannot_be_hashed() {
    let root = tempfile::tempdir().expect("create root");
    assert_eq!(
        plugin_package_hash(&root.path().join("missing")).await,
        None
    );
}

#[tokio::test]
async fn no_recorded_hash_is_not_a_refusal() {
    let package = plugin_package();
    assert_eq!(
        plugin_package_trust(package.path(), /*trusted_package_hash*/ None).await,
        PluginPackageTrust::Unpinned
    );
    assert!(PluginPackageTrust::Unpinned.may_launch());
}

#[tokio::test]
async fn the_recorded_hash_is_what_decides() {
    let package = plugin_package();
    let approved = hash(package.path()).await;

    assert_eq!(
        plugin_package_trust(package.path(), Some(&approved)).await,
        PluginPackageTrust::Trusted
    );
    assert!(PluginPackageTrust::Trusted.may_launch());

    write(package.path(), "server.js", "different");
    assert_eq!(
        plugin_package_trust(package.path(), Some(&approved)).await,
        PluginPackageTrust::Changed
    );
    assert!(!PluginPackageTrust::Changed.may_launch());
}

/// A package that was measured and can no longer be measured is not evidence of
/// nothing having happened.
#[tokio::test]
async fn a_package_that_can_no_longer_be_read_is_not_trusted() {
    let root = tempfile::tempdir().expect("create root");
    let missing = root.path().join("uninstalled");

    assert_eq!(
        plugin_package_trust(&missing, Some("sha256:whatever")).await,
        PluginPackageTrust::Unreadable
    );
    assert!(!PluginPackageTrust::Unreadable.may_launch());
}

#[tokio::test]
async fn the_hash_says_what_it_is() {
    let package = plugin_package();
    let hash = hash(package.path()).await;
    assert!(hash.starts_with("sha256:"), "{hash}");
    assert_eq!(hash.len(), "sha256:".len() + 64);
}

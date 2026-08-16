use super::*;
use pretty_assertions::assert_eq;
use tempfile::tempdir;
use whisply_file_system::CopyOptions;
use whisply_file_system::CreateDirectoryOptions;
use whisply_file_system::ExecutorFileSystemFuture;
use whisply_file_system::FileMetadata;
use whisply_file_system::FileSystemReadStream;
use whisply_file_system::FileSystemSandboxContext;
use whisply_file_system::ReadDirectoryEntry;
use whisply_file_system::RemoveOptions;
use whisply_utils_path_uri::PathUri;

struct TestFileSystem;

impl ExecutorFileSystem for TestFileSystem {
    fn canonicalize<'a>(
        &'a self,
        path: &'a PathUri,
        _sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, PathUri> {
        Box::pin(async move {
            let path = path.to_abs_path()?;
            let canonicalized = path.canonicalize()?;
            Ok(PathUri::from_abs_path(&canonicalized))
        })
    }

    fn read_file<'a>(
        &'a self,
        path: &'a PathUri,
        _sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, Vec<u8>> {
        Box::pin(async move {
            let path = path.to_abs_path()?;
            tokio::fs::read(path.as_path()).await
        })
    }

    fn read_file_stream<'a>(
        &'a self,
        _path: &'a PathUri,
        _sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, FileSystemReadStream> {
        Box::pin(async {
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "test filesystem does not support streaming reads",
            ))
        })
    }

    fn write_file<'a>(
        &'a self,
        _path: &'a PathUri,
        _contents: Vec<u8>,
        _sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, ()> {
        Box::pin(async move { unimplemented!("test filesystem only supports reads") })
    }

    fn create_directory<'a>(
        &'a self,
        _path: &'a PathUri,
        _create_directory_options: CreateDirectoryOptions,
        _sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, ()> {
        Box::pin(async move { unimplemented!("test filesystem only supports reads") })
    }

    fn get_metadata<'a>(
        &'a self,
        path: &'a PathUri,
        _sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, FileMetadata> {
        Box::pin(async move {
            let path = path.to_abs_path()?;
            let metadata = tokio::fs::symlink_metadata(path.as_path()).await?;
            Ok(FileMetadata {
                is_directory: metadata.is_dir(),
                is_file: metadata.is_file(),
                is_symlink: metadata.is_symlink(),
                size: metadata.len(),
                created_at_ms: 0,
                modified_at_ms: 0,
            })
        })
    }

    fn read_directory<'a>(
        &'a self,
        _path: &'a PathUri,
        _sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, Vec<ReadDirectoryEntry>> {
        Box::pin(async move { unimplemented!("test filesystem only supports reads") })
    }

    fn remove<'a>(
        &'a self,
        _path: &'a PathUri,
        _remove_options: RemoveOptions,
        _sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, ()> {
        Box::pin(async move { unimplemented!("test filesystem only supports reads") })
    }

    fn copy<'a>(
        &'a self,
        _source_path: &'a PathUri,
        _destination_path: &'a PathUri,
        _copy_options: CopyOptions,
        _sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, ()> {
        Box::pin(async move { unimplemented!("test filesystem only supports reads") })
    }
}

#[tokio::test]
async fn profile_v2_rejects_matching_legacy_profile_in_base_user_config() {
    let tmp = tempdir().expect("tempdir");
    let selected_config = tmp.path().join("work.config.toml");

    std::fs::write(
        tmp.path().join(CONFIG_TOML_FILE),
        r#"
model = "gpt-main"

[profiles.work]
model = "gpt-work"
"#,
    )
    .expect("write default user config");
    std::fs::write(&selected_config, r#"model = "gpt-work-v2""#)
        .expect("write selected user config");

    let mut overrides = LoaderOverrides::without_managed_config_for_tests();
    overrides.user_config_path = Some(AbsolutePathBuf::resolve_path_against_base(
        "work.config.toml",
        tmp.path(),
    ));
    overrides.user_config_profile = Some("work".parse().expect("profile-v2 name"));

    let err = load_config_layers_state(
        &TestFileSystem,
        tmp.path(),
        /*cwd*/ None,
        &[],
        overrides,
        &crate::NoopThreadConfigLoader,
    )
    .await
    .expect_err("profile-v2 should reject a matching legacy profile in base user config");

    assert_eq!(
        err.kind(),
        io::ErrorKind::InvalidData,
        "a matching legacy profile should be a hard config error"
    );
    let message = err.to_string();
    assert!(
        message.contains("--profile `work` cannot be used"),
        "unexpected error message: {message}"
    );
    assert!(
        message.contains("config.toml"),
        "unexpected error message: {message}"
    );
    assert!(
        message.contains("[profiles.work]"),
        "unexpected error message: {message}"
    );
    // The message has to carry the remedy itself; Whisply publishes no
    // configuration documentation to send the reader to instead.
    assert!(
        message.contains("move those settings into"),
        "unexpected error message: {message}"
    );
    assert!(
        !message.contains("openai.com"),
        "the message sends the reader to another product's documentation: {message}"
    );
}

#[tokio::test]
async fn profile_v2_rejects_matching_legacy_profile_selector_in_base_user_config() {
    let tmp = tempdir().expect("tempdir");
    let selected_config = tmp.path().join("work.config.toml");

    std::fs::write(
        tmp.path().join(CONFIG_TOML_FILE),
        r#"
profile = "work"
model = "gpt-main"
"#,
    )
    .expect("write default user config");
    std::fs::write(&selected_config, r#"model = "gpt-work-v2""#)
        .expect("write selected user config");

    let mut overrides = LoaderOverrides::without_managed_config_for_tests();
    overrides.user_config_path = Some(AbsolutePathBuf::resolve_path_against_base(
        "work.config.toml",
        tmp.path(),
    ));
    overrides.user_config_profile = Some("work".parse().expect("profile-v2 name"));

    let err = load_config_layers_state(
        &TestFileSystem,
        tmp.path(),
        /*cwd*/ None,
        &[],
        overrides,
        &crate::NoopThreadConfigLoader,
    )
    .await
    .expect_err("profile-v2 should reject a matching legacy profile selector");

    assert_eq!(
        err.kind(),
        io::ErrorKind::InvalidData,
        "a matching legacy profile selector should be a hard config error"
    );
    let message = err.to_string();
    assert!(
        message.contains("--profile `work` cannot be used"),
        "unexpected error message: {message}"
    );
    assert!(
        message.contains("profile = \"work\""),
        "unexpected error message: {message}"
    );
    assert!(
        message.contains("work.config.toml"),
        "unexpected error message: {message}"
    );
}

#[tokio::test]
async fn profile_v2_allows_unrelated_legacy_profiles_in_base_user_config() {
    let tmp = tempdir().expect("tempdir");
    let selected_config = tmp.path().join("work.config.toml");

    std::fs::write(
        tmp.path().join(CONFIG_TOML_FILE),
        r#"
model = "gpt-main"

[profiles.dev]
model = "gpt-dev"
"#,
    )
    .expect("write default user config");
    std::fs::write(&selected_config, r#"model = "gpt-work-v2""#)
        .expect("write selected user config");

    let mut overrides = LoaderOverrides::without_managed_config_for_tests();
    overrides.user_config_path = Some(AbsolutePathBuf::resolve_path_against_base(
        "work.config.toml",
        tmp.path(),
    ));
    overrides.user_config_profile = Some("work".parse().expect("profile-v2 name"));

    load_config_layers_state(
        &TestFileSystem,
        tmp.path(),
        /*cwd*/ None,
        &[],
        overrides,
        &crate::NoopThreadConfigLoader,
    )
    .await
    .expect("profile-v2 should allow unrelated legacy profiles in base user config");
}

/// WCD-712: a `No directory` thread runs in an app-owned root inside the
/// Whisply home. Home directories are very often git repositories -- dotfiles
/// checkouts make that ordinary -- so an unbounded upward search for a project
/// marker finds the person's home and adopts it as the project for a thread
/// whose entire promise is that no project is inferred.
#[tokio::test]
async fn no_directory_does_not_adopt_the_home_directory_as_its_project() {
    let tmp = tempdir().expect("tempdir");
    let home = tmp.path();
    std::fs::create_dir_all(home.join(".git")).expect("home checkout");
    let whisply_home = home.join(".whisply");
    let neutral_workspace = whisply_home.join("neutral-workspace");
    std::fs::create_dir_all(&neutral_workspace).expect("neutral workspace");

    let cwd = AbsolutePathBuf::from_absolute_path(&neutral_workspace).expect("cwd");
    let root = find_project_root(&TestFileSystem, &cwd, &[".git".to_string()], &whisply_home)
        .await
        .expect("project root");

    assert_eq!(
        root, cwd,
        "a no-directory thread must not inherit the home checkout above the Whisply runtime home"
    );
    assert_eq!(
        find_git_checkout_root(&TestFileSystem, &cwd, &whisply_home).await,
        None,
        "and must not report that home checkout as its repository"
    );
}

/// The same bound must not touch an ordinary thread: a real project outside the
/// Whisply home is still discovered by walking up from its subdirectory.
#[tokio::test]
async fn an_ordinary_project_is_still_discovered_from_a_subdirectory() {
    let tmp = tempdir().expect("tempdir");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(project.join(".git")).expect("project checkout");
    let nested = project.join("crates").join("thing");
    std::fs::create_dir_all(&nested).expect("nested");
    let whisply_home = tmp.path().join(".whisply");
    std::fs::create_dir_all(&whisply_home).expect("whisply home");

    let cwd = AbsolutePathBuf::from_absolute_path(&nested).expect("cwd");
    let root = find_project_root(&TestFileSystem, &cwd, &[".git".to_string()], &whisply_home)
        .await
        .expect("project root");

    assert_eq!(
        root,
        AbsolutePathBuf::from_absolute_path(&project).expect("project"),
        "bounding no-directory discovery must not stop ordinary project discovery"
    );
    assert_eq!(
        find_git_checkout_root(&TestFileSystem, &cwd, &whisply_home).await,
        Some(AbsolutePathBuf::from_absolute_path(&project).expect("project")),
    );
}

/// The predicate is consulted before every upward search, so what it does when
/// the filesystem cannot answer decides whether containment holds. Canonicalising
/// requires both paths to exist; a home still being created, or a volume briefly
/// away, must not read as "this directory is not in the runtime home" and hand
/// the caller permission to walk above it.
#[test]
fn a_path_that_cannot_be_canonicalised_is_still_recognised_as_inside_the_home() {
    let home = std::path::Path::new("/nonexistent-volume/person/.whisply");
    let inside = AbsolutePathBuf::from_absolute_path(std::path::Path::new(
        "/nonexistent-volume/person/.whisply/neutral-workspace",
    ))
    .expect("absolute path");

    assert!(
        is_inside_runtime_home(&inside, home),
        "an unresolvable path inside the runtime home must not be treated as outside it"
    );
}

#[test]
fn a_path_that_cannot_be_canonicalised_outside_the_home_is_still_outside() {
    let home = std::path::Path::new("/nonexistent-volume/person/.whisply");
    let outside = AbsolutePathBuf::from_absolute_path(std::path::Path::new(
        "/nonexistent-volume/person/code",
    ))
    .expect("absolute path");

    assert!(
        !is_inside_runtime_home(&outside, home),
        "failing safe must not swallow every real project as well"
    );
}

/// `..` must not walk out of the home and back in unnoticed.
#[test]
fn a_traversal_out_of_the_home_is_not_inside_it() {
    let home = std::path::Path::new("/nonexistent-volume/person/.whisply");
    let escaped = AbsolutePathBuf::from_absolute_path(std::path::Path::new(
        "/nonexistent-volume/person/.whisply/../code",
    ))
    .expect("absolute path");

    assert!(
        !is_inside_runtime_home(&escaped, home),
        "a path that traverses out of the home is outside it"
    );
}

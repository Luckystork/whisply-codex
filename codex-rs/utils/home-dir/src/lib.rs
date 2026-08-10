use codex_utils_absolute_path::AbsolutePathBuf;
use dirs::home_dir;
use std::path::PathBuf;

/// Returns the path to the Whisply configuration directory, which can be
/// specified by the `WHISPLY_HOME` environment variable. If not set, defaults
/// to `~/.whisply`.
///
/// - If `WHISPLY_HOME` is set, the value must exist and be a directory. The
///   final path must not be a symlink; the value will be canonicalized and this
///   function will Err otherwise.
/// - If `WHISPLY_HOME` is not set, this function does not verify that the
///   directory exists.
pub fn find_whisply_home() -> std::io::Result<AbsolutePathBuf> {
    let whisply_home_env = std::env::var("WHISPLY_HOME")
        .ok()
        .filter(|val| !val.is_empty());
    find_whisply_home_from_env(whisply_home_env.as_deref())
}

/// Compatibility alias for upstream-internal callers. New code must use
/// [`find_whisply_home`].
pub fn find_codex_home() -> std::io::Result<AbsolutePathBuf> {
    find_whisply_home()
}

fn find_whisply_home_from_env(whisply_home_env: Option<&str>) -> std::io::Result<AbsolutePathBuf> {
    // Honor `WHISPLY_HOME` when it is set to allow users
    // (and tests) to override the default location.
    match whisply_home_env {
        Some(val) => {
            let path = PathBuf::from(val);
            let metadata = std::fs::symlink_metadata(&path).map_err(|err| match err.kind() {
                std::io::ErrorKind::NotFound => std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("WHISPLY_HOME points to {val:?}, but that path does not exist"),
                ),
                _ => std::io::Error::new(
                    err.kind(),
                    format!("failed to read WHISPLY_HOME {val:?}: {err}"),
                ),
            })?;

            if metadata.file_type().is_symlink() {
                Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("WHISPLY_HOME points to symbolic link {val:?}"),
                ))
            } else if !metadata.is_dir() {
                Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("WHISPLY_HOME points to {val:?}, but that path is not a directory"),
                ))
            } else {
                let canonical = path.canonicalize().map_err(|err| {
                    std::io::Error::new(
                        err.kind(),
                        format!("failed to canonicalize WHISPLY_HOME {val:?}: {err}"),
                    )
                })?;
                AbsolutePathBuf::from_absolute_path(canonical)
            }
        }
        None => {
            let mut p = home_dir().ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "Could not find home directory",
                )
            })?;
            p.push(".whisply");
            AbsolutePathBuf::from_absolute_path(p)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::find_whisply_home_from_env;
    use codex_utils_absolute_path::AbsolutePathBuf;
    use dirs::home_dir;
    use pretty_assertions::assert_eq;
    use std::fs;
    use std::io::ErrorKind;
    use tempfile::TempDir;

    #[test]
    fn find_whisply_home_env_missing_path_is_fatal() {
        let temp_home = TempDir::new().expect("temp home");
        let missing = temp_home.path().join("missing-whisply-home");
        let missing_str = missing
            .to_str()
            .expect("missing Whisply home path should be valid utf-8");

        let err = find_whisply_home_from_env(Some(missing_str)).expect_err("missing WHISPLY_HOME");
        assert_eq!(err.kind(), ErrorKind::NotFound);
        assert!(
            err.to_string().contains("WHISPLY_HOME"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn find_whisply_home_env_file_path_is_fatal() {
        let temp_home = TempDir::new().expect("temp home");
        let file_path = temp_home.path().join("whisply-home.txt");
        fs::write(&file_path, "not a directory").expect("write temp file");
        let file_str = file_path
            .to_str()
            .expect("file codex home path should be valid utf-8");

        let err = find_whisply_home_from_env(Some(file_str)).expect_err("file WHISPLY_HOME");
        assert_eq!(err.kind(), ErrorKind::InvalidInput);
        assert!(
            err.to_string().contains("not a directory"),
            "unexpected error: {err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn find_whisply_home_env_symlink_is_fatal() {
        use std::os::unix::fs::symlink;

        let temp_home = TempDir::new().expect("temp home");
        let target = temp_home.path().join("target");
        fs::create_dir(&target).expect("create target");
        let link = temp_home.path().join("whisply-home-link");
        symlink(&target, &link).expect("create link");

        let err = find_whisply_home_from_env(link.to_str()).expect_err("symlink WHISPLY_HOME");
        assert_eq!(err.kind(), ErrorKind::InvalidInput);
        assert!(err.to_string().contains("symbolic link"));
    }

    #[test]
    fn find_whisply_home_env_valid_directory_canonicalizes() {
        let temp_home = TempDir::new().expect("temp home");
        let temp_str = temp_home
            .path()
            .to_str()
            .expect("temp Whisply home path should be valid utf-8");

        let resolved = find_whisply_home_from_env(Some(temp_str)).expect("valid WHISPLY_HOME");
        let expected = temp_home
            .path()
            .canonicalize()
            .expect("canonicalize temp home");
        let expected = AbsolutePathBuf::from_absolute_path(expected).expect("absolute home");
        assert_eq!(resolved, expected);
    }

    #[test]
    fn find_whisply_home_without_env_uses_default_home_dir() {
        let resolved =
            find_whisply_home_from_env(/*whisply_home_env*/ None).expect("default WHISPLY_HOME");
        let mut expected = home_dir().expect("home dir");
        expected.push(".whisply");
        let expected = AbsolutePathBuf::from_absolute_path(expected).expect("absolute home");
        assert_eq!(resolved, expected);
    }
}

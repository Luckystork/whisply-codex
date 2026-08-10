use std::fs;
use std::io;
use std::path::Component;
use std::path::Path;

const MAX_PACKAGE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_FILE_BYTES: u64 = 512 * 1024;
const MAX_FILES: usize = 2_000;
const MAX_DEPTH: usize = 6;

/// Rejects skill packages that could copy credentials or unsafe filesystem
/// entries into a managed skill root. Callers must validate before copying so
/// rejected packages leave no partial destination behind.
pub fn validate_skill_package_contents(package: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(package)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(invalid_data_error(
            "skill package must be a real directory, not a link or special file",
        ));
    }

    let mut budget = PackageBudget::default();
    inspect_tree(package, package, /*depth*/ 0, &mut budget)
}

/// Validates a local plugin package before an external-agent migration copies it
/// into an account-scoped cache. Unlike skill packages, plugins may legitimately
/// contain larger code trees, so this keeps only the credential and filesystem
/// safety checks and does not impose skill-specific size or depth limits.
pub fn validate_plugin_package_contents(package: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(package)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(invalid_data_error(
            "plugin package must be a real directory, not a link or special file",
        ));
    }
    inspect_plugin_tree(package, package)
}

#[derive(Default)]
struct PackageBudget {
    files: usize,
    bytes: u64,
}

fn inspect_tree(
    package_root: &Path,
    current: &Path,
    depth: usize,
    budget: &mut PackageBudget,
) -> io::Result<()> {
    if depth > MAX_DEPTH {
        return Err(invalid_data_error(
            "skill package exceeds the maximum directory depth",
        ));
    }

    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let relative = path
            .strip_prefix(package_root)
            .map_err(|_| invalid_data_error("skill package path escaped its root"))?;
        if relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(invalid_data_error(
                "skill package contains an unsafe relative path",
            ));
        }
        if relative
            .components()
            .filter_map(|component| match component {
                Component::Normal(name) => name.to_str(),
                _ => None,
            })
            .any(is_sensitive_file_name)
        {
            return Err(invalid_data_error(
                "skill package contains a sensitive credential file name",
            ));
        }

        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            return Err(invalid_data_error("skill package may not contain symlinks"));
        }
        if metadata.is_dir() {
            inspect_tree(package_root, &path, depth + 1, budget)?;
            continue;
        }
        if !metadata.is_file() {
            return Err(invalid_data_error(
                "skill package contains an unsupported special file",
            ));
        }

        budget.files = budget.files.saturating_add(1);
        budget.bytes = budget.bytes.saturating_add(metadata.len());
        if budget.files > MAX_FILES || budget.bytes > MAX_PACKAGE_BYTES {
            return Err(invalid_data_error(
                "skill package exceeds its bounded file or byte limit",
            ));
        }
        if metadata.len() > MAX_FILE_BYTES {
            return Err(invalid_data_error(
                "skill package contains a file that exceeds its size limit",
            ));
        }

        validate_skill_document_contents(&fs::read(&path)?)?;
    }

    Ok(())
}

fn inspect_plugin_tree(package_root: &Path, current: &Path) -> io::Result<()> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let relative = path
            .strip_prefix(package_root)
            .map_err(|_| invalid_data_error("plugin package path escaped its root"))?;
        if relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(invalid_data_error(
                "plugin package contains an unsafe relative path",
            ));
        }
        if relative
            .components()
            .filter_map(|component| match component {
                Component::Normal(name) => name.to_str(),
                _ => None,
            })
            .any(is_sensitive_file_name)
        {
            return Err(invalid_data_error(
                "plugin package contains a sensitive credential file name",
            ));
        }

        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            return Err(invalid_data_error(
                "plugin package may not contain symlinks",
            ));
        }
        if metadata.is_dir() {
            inspect_plugin_tree(package_root, &path)?;
            continue;
        }
        if !metadata.is_file() {
            return Err(invalid_data_error(
                "plugin package contains an unsupported special file",
            ));
        }
        validate_skill_document_contents(&fs::read(&path)?)?;
    }
    Ok(())
}

fn is_sensitive_file_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name == ".env"
        || name == ".envrc"
        || name.starts_with(".env.")
        || matches!(
            name.as_str(),
            "credentials"
                | "credentials.json"
                | "credentials.toml"
                | "credentials.yaml"
                | "credentials.yml"
                | "secrets.json"
                | "secrets.toml"
                | "secrets.yaml"
                | "secrets.yml"
                | "id_rsa"
                | "id_dsa"
                | "id_ecdsa"
                | "id_ed25519"
                | "authorized_keys"
                | ".netrc"
                | "_netrc"
        )
        || name.ends_with(".pem")
        || name.ends_with(".key")
        || name.ends_with(".p12")
        || name.ends_with(".pfx")
}

/// Rejects a rendered skill document only when it contains a literal credential
/// value. Mentions such as `API_KEY` or `Bearer <token>` are normal
/// instructional content and deliberately remain valid.
pub fn validate_skill_document_contents(contents: &[u8]) -> io::Result<()> {
    let contents = String::from_utf8_lossy(contents);
    if contents.lines().any(contains_literal_credential_assignment)
        || contains_private_key_marker(&contents)
        || contains_token_shaped_literal(&contents)
    {
        return Err(invalid_data_error(
            "skill package contains a literal credential value",
        ));
    }
    Ok(())
}

fn contains_private_key_marker(contents: &str) -> bool {
    contents.lines().any(|line| {
        let line = line.trim();
        line.starts_with("-----BEGIN ")
            && (line.contains(" PRIVATE KEY-----") || line == "-----BEGIN OPENSSH PRIVATE KEY-----")
    })
}

fn contains_literal_credential_assignment(line: &str) -> bool {
    let line = line.trim();
    let Some((key, value)) = line.split_once(['=', ':']) else {
        return false;
    };
    let normalized_key = key
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .map(|ch| ch.to_ascii_lowercase())
        .collect::<String>();
    if !is_credential_field(&normalized_key) {
        return false;
    }
    let value = value
        .trim()
        .trim_matches(|ch: char| matches!(ch, '"' | '\'' | ',' | ';'))
        .trim();
    !value.is_empty() && !is_placeholder_value(value)
}

fn is_credential_field(normalized_key: &str) -> bool {
    normalized_key.contains("apikey")
        || normalized_key.contains("token")
        || normalized_key.contains("secret")
        || normalized_key.contains("password")
        || normalized_key.contains("authorization")
        || normalized_key.contains("cookie")
}

fn is_placeholder_value(value: &str) -> bool {
    let value = value.trim().to_ascii_lowercase();
    let value = value.strip_prefix("bearer ").unwrap_or(&value).trim();
    value.contains("${")
        || value.starts_with('$')
        || value.starts_with('<')
        || value.starts_with('[')
        || value.starts_with('{')
        || value == "..."
        || value.starts_with("your")
        || value.starts_with("example")
        || value.starts_with("fake")
        || value.starts_with("test")
        || value.starts_with("replace")
        || value.starts_with("redacted")
        || value.starts_with("use ")
        || value.starts_with("set ")
        || value.starts_with("provide ")
        || value.starts_with("enter ")
        || value.starts_with("required")
        || value.starts_with("optional")
        || value.starts_with("the ")
        || value == "token"
        || value == "secret"
        || value == "value"
}

fn contains_token_shaped_literal(contents: &str) -> bool {
    contents
        .split(|ch: char| !(ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.')))
        .any(|word| {
            let word = word.to_ascii_lowercase();
            (word.starts_with("sk-") && word.len() >= 20)
                || (word.starts_with("ghp_") && word.len() >= 24)
                || (word.starts_with("xox") && word.len() >= 20)
                || ((word.starts_with("akia") || word.starts_with("asia")) && word.len() == 20)
                || (word.starts_with("eyj") && word.split('.').count() == 3 && word.len() >= 40)
        })
}

fn invalid_data_error(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permits_ordinary_skill_package_contents() {
        let root = tempfile::tempdir().expect("create tempdir");
        fs::write(root.path().join("SKILL.md"), "# Safe skill\n").expect("write skill");
        fs::create_dir(root.path().join("references")).expect("create references");
        fs::write(root.path().join("references/guide.md"), "Use local facts.")
            .expect("write guide");
        fs::write(
            root.path().join("references/secrets-management.md"),
            "Discuss secret rotation without transferring credentials.",
        )
        .expect("write safe documentation");
        fs::write(
            root.path().join("credential-setup.md"),
            "Explain credential setup.",
        )
        .expect("write safe documentation");

        validate_skill_package_contents(root.path()).expect("validate safe package");
    }

    #[test]
    fn permits_safe_plugin_code_but_rejects_credentials() {
        let root = tempfile::tempdir().expect("create tempdir");
        fs::create_dir_all(root.path().join("src")).expect("create source dir");
        fs::write(
            root.path().join("src/index.js"),
            "console.log('safe plugin')",
        )
        .expect("write plugin code");
        validate_plugin_package_contents(root.path()).expect("safe plugin package");

        fs::write(
            root.path().join(".netrc"),
            "machine api.example password secret",
        )
        .expect("write netrc");
        assert!(validate_plugin_package_contents(root.path()).is_err());
    }

    #[test]
    fn rejects_sensitive_names_and_contents_before_copying() {
        let root = tempfile::tempdir().expect("create tempdir");
        fs::write(root.path().join("SKILL.md"), "# Safe skill\n").expect("write skill");
        fs::write(root.path().join(".env"), "OPENAI_API_KEY=private").expect("write env");
        assert!(validate_skill_package_contents(root.path()).is_err());

        fs::remove_file(root.path().join(".env")).expect("remove env");
        fs::write(
            root.path().join(".netrc"),
            "machine api.example login alice password literal-secret",
        )
        .expect("write netrc");
        assert!(validate_skill_package_contents(root.path()).is_err());

        fs::remove_file(root.path().join(".netrc")).expect("remove netrc");
        fs::write(
            root.path().join("notes.txt"),
            "-----BEGIN OPENSSH PRIVATE KEY-----\\nprivate material",
        )
        .expect("write private key");
        assert!(validate_skill_package_contents(root.path()).is_err());

        fs::remove_file(root.path().join("notes.txt")).expect("remove private key");
        fs::write(
            root.path().join("references.md"),
            "Authorization: Bearer private",
        )
        .expect("write credential");
        assert!(validate_skill_package_contents(root.path()).is_err());
    }

    #[test]
    fn permits_credential_discussion_but_rejects_literal_values() {
        validate_skill_document_contents(b"API_KEY=your-api-key\nAuthorization: Bearer <token>")
            .expect("ordinary instructional content remains valid");
        assert!(validate_skill_document_contents(b"OPENAI_API_KEY=private").is_err());
        assert!(validate_skill_document_contents(b"Authorization: Bearer private").is_err());
        assert!(validate_skill_document_contents(b"AKIA1234567890ABCDEF").is_err());
    }
}

use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use tokio::task;
use toml_edit::DocumentMut;
use toml_edit::Item as TomlItem;
use toml_edit::Table as TomlTable;
use toml_edit::value;
use whisply_utils_path::resolve_symlink_write_paths;
use whisply_utils_path::write_atomically;

use crate::CONFIG_TOML_FILE;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginConfigEdit {
    SetEnabled {
        plugin_key: String,
        enabled: bool,
    },
    /// Record the launch definition this plugin's MCP server was approved with,
    /// so a later change to it is refused instead of started silently.
    SetMcpTrustedLaunchHash {
        plugin_key: String,
        server_name: String,
        launch_hash: String,
    },
    /// Record that this plugin's MCP servers have been reviewed, so a server
    /// that appears later is treated as new rather than as pre-dating pinning.
    MarkMcpLaunchesPinned {
        plugin_key: String,
    },
    /// Record the files this plugin shipped when it was approved, so an edit to
    /// the program a reviewed command line starts is refused rather than run.
    SetTrustedPackageHash {
        plugin_key: String,
        package_hash: String,
    },
    Clear {
        plugin_key: String,
    },
}

pub async fn set_user_plugin_enabled(
    codex_home: &Path,
    plugin_key: String,
    enabled: bool,
) -> std::io::Result<()> {
    apply_user_plugin_config_edits(
        codex_home,
        vec![PluginConfigEdit::SetEnabled {
            plugin_key,
            enabled,
        }],
    )
    .await
}

pub async fn clear_user_plugin(codex_home: &Path, plugin_key: String) -> std::io::Result<()> {
    apply_user_plugin_config_edits(codex_home, vec![PluginConfigEdit::Clear { plugin_key }]).await
}

pub async fn apply_user_plugin_config_edits(
    codex_home: &Path,
    edits: Vec<PluginConfigEdit>,
) -> std::io::Result<()> {
    let codex_home = codex_home.to_path_buf();
    task::spawn_blocking(move || apply_user_plugin_config_edits_blocking(&codex_home, edits))
        .await
        .map_err(|err| std::io::Error::other(format!("config persistence task panicked: {err}")))?
}

/// For callers that are already off the async runtime: the curated plugin cache
/// refresh runs on its own thread, and remote bundle installs run on a blocking
/// task, so neither can await the spawn-blocking wrapper above.
pub fn apply_user_plugin_config_edits_blocking(
    codex_home: &Path,
    edits: Vec<PluginConfigEdit>,
) -> std::io::Result<()> {
    if edits.is_empty() {
        return Ok(());
    }

    let config_path = codex_home.join(CONFIG_TOML_FILE);
    let write_paths = resolve_symlink_write_paths(&config_path)?;
    let mut doc = read_or_create_document(write_paths.read_path.as_deref())?;
    let mut mutated = false;
    for edit in edits {
        mutated |= match edit {
            PluginConfigEdit::SetEnabled {
                plugin_key,
                enabled,
            } => set_plugin_enabled(&mut doc, &plugin_key, enabled),
            PluginConfigEdit::SetMcpTrustedLaunchHash {
                plugin_key,
                server_name,
                launch_hash,
            } => set_plugin_mcp_trusted_launch_hash(
                &mut doc,
                &plugin_key,
                &server_name,
                &launch_hash,
            ),
            PluginConfigEdit::MarkMcpLaunchesPinned { plugin_key } => {
                mark_plugin_mcp_launches_pinned(&mut doc, &plugin_key)
            }
            PluginConfigEdit::SetTrustedPackageHash {
                plugin_key,
                package_hash,
            } => set_plugin_trusted_package_hash(&mut doc, &plugin_key, &package_hash),
            PluginConfigEdit::Clear { plugin_key } => clear_plugin(&mut doc, &plugin_key),
        };
    }
    if !mutated {
        return Ok(());
    }
    write_atomically(&write_paths.write_path, &doc.to_string())
}

fn read_or_create_document(config_path: Option<&Path>) -> std::io::Result<DocumentMut> {
    let Some(config_path) = config_path else {
        return Ok(DocumentMut::new());
    };
    match fs::read_to_string(config_path) {
        Ok(raw) => raw
            .parse::<DocumentMut>()
            .map_err(|err| std::io::Error::new(ErrorKind::InvalidData, err)),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(DocumentMut::new()),
        Err(err) => Err(err),
    }
}

fn set_plugin_enabled(doc: &mut DocumentMut, plugin_key: &str, enabled: bool) -> bool {
    let Some(plugins) = ensure_plugins_table(doc) else {
        return false;
    };
    let Some(plugin) = ensure_table_for_write(&mut plugins[plugin_key]) else {
        return false;
    };
    let mut replacement = value(enabled);
    if let Some(existing) = plugin.get("enabled") {
        preserve_decor(existing, &mut replacement);
    }
    plugin["enabled"] = replacement;
    true
}

fn set_plugin_mcp_trusted_launch_hash(
    doc: &mut DocumentMut,
    plugin_key: &str,
    server_name: &str,
    launch_hash: &str,
) -> bool {
    let Some(plugins) = ensure_plugins_table(doc) else {
        return false;
    };
    let Some(plugin) = ensure_table_for_write(&mut plugins[plugin_key]) else {
        return false;
    };
    let Some(servers) = ensure_table_for_write(&mut plugin["mcp_servers"]) else {
        return false;
    };
    let Some(server) = ensure_table_for_write(&mut servers[server_name]) else {
        return false;
    };
    let mut replacement = value(launch_hash);
    if let Some(existing) = server.get("trusted_launch_hash") {
        preserve_decor(existing, &mut replacement);
    }
    server["trusted_launch_hash"] = replacement;
    true
}

fn set_plugin_trusted_package_hash(
    doc: &mut DocumentMut,
    plugin_key: &str,
    package_hash: &str,
) -> bool {
    let Some(plugins) = ensure_plugins_table(doc) else {
        return false;
    };
    let Some(plugin) = ensure_table_for_write(&mut plugins[plugin_key]) else {
        return false;
    };
    let mut replacement = value(package_hash);
    if let Some(existing) = plugin.get("trusted_package_hash") {
        preserve_decor(existing, &mut replacement);
    }
    plugin["trusted_package_hash"] = replacement;
    true
}

fn mark_plugin_mcp_launches_pinned(doc: &mut DocumentMut, plugin_key: &str) -> bool {
    let Some(plugins) = ensure_plugins_table(doc) else {
        return false;
    };
    let Some(plugin) = ensure_table_for_write(&mut plugins[plugin_key]) else {
        return false;
    };
    let mut replacement = value(true);
    if let Some(existing) = plugin.get("mcp_launch_pinned") {
        preserve_decor(existing, &mut replacement);
    }
    plugin["mcp_launch_pinned"] = replacement;
    true
}

fn clear_plugin(doc: &mut DocumentMut, plugin_key: &str) -> bool {
    let root = doc.as_table_mut();
    let Some(plugins_item) = root.get_mut("plugins") else {
        return false;
    };
    let Some(plugins) = ensure_table_for_read(plugins_item) else {
        return false;
    };
    plugins.remove(plugin_key).is_some()
}

fn ensure_plugins_table(doc: &mut DocumentMut) -> Option<&mut TomlTable> {
    let root = doc.as_table_mut();
    if !root.contains_key("plugins") {
        root.insert("plugins", TomlItem::Table(new_implicit_table()));
    }
    ensure_table_for_write(root.get_mut("plugins")?)
}

fn ensure_table_for_write(item: &mut TomlItem) -> Option<&mut TomlTable> {
    match item {
        TomlItem::Table(table) => Some(table),
        TomlItem::Value(value) => {
            let table = value
                .as_inline_table()
                .map_or_else(new_implicit_table, table_from_inline);
            *item = TomlItem::Table(table);
            item.as_table_mut()
        }
        TomlItem::None => {
            *item = TomlItem::Table(new_implicit_table());
            item.as_table_mut()
        }
        _ => None,
    }
}

fn ensure_table_for_read(item: &mut TomlItem) -> Option<&mut TomlTable> {
    match item {
        TomlItem::Table(_) => {}
        TomlItem::Value(value) => {
            let inline = value.as_inline_table()?.clone();
            *item = TomlItem::Table(table_from_inline(&inline));
        }
        _ => return None,
    }
    item.as_table_mut()
}

fn table_from_inline(inline: &toml_edit::InlineTable) -> TomlTable {
    let mut table = new_implicit_table();
    for (key, value) in inline.iter() {
        let mut value = value.clone();
        value.decor_mut().set_suffix("");
        table.insert(key, TomlItem::Value(value));
    }
    table
}

fn new_implicit_table() -> TomlTable {
    let mut table = TomlTable::new();
    table.set_implicit(true);
    table
}

fn preserve_decor(existing: &TomlItem, replacement: &mut TomlItem) {
    if let (TomlItem::Value(existing_value), TomlItem::Value(replacement_value)) =
        (existing, replacement)
    {
        replacement_value
            .decor_mut()
            .clone_from(existing_value.decor());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    #[tokio::test]
    async fn set_user_plugin_enabled_writes_plugin_entry() {
        let codex_home = TempDir::new().unwrap();

        set_user_plugin_enabled(
            codex_home.path(),
            "demo@market".to_string(),
            /*enabled*/ true,
        )
        .await
        .unwrap();

        let config = read_config(codex_home.path());
        let expected: toml::Value = toml::from_str(
            r#"
[plugins."demo@market"]
enabled = true
        "#,
        )
        .unwrap();
        assert_eq!(config, expected);
    }

    #[tokio::test]
    async fn recording_a_launch_pin_keeps_the_plugin_and_its_other_servers() {
        let codex_home = TempDir::new().unwrap();
        fs::write(
            codex_home.path().join(CONFIG_TOML_FILE),
            r#"
[plugins."demo@market"]
enabled = true

[plugins."demo@market".mcp_servers.other]
enabled = false
"#,
        )
        .unwrap();

        apply_user_plugin_config_edits(
            codex_home.path(),
            vec![PluginConfigEdit::SetMcpTrustedLaunchHash {
                plugin_key: "demo@market".to_string(),
                server_name: "notes".to_string(),
                launch_hash: "sha256:abc".to_string(),
            }],
        )
        .await
        .unwrap();

        let config = read_config(codex_home.path());
        let expected: toml::Value = toml::from_str(
            r#"
[plugins."demo@market"]
enabled = true

[plugins."demo@market".mcp_servers.other]
enabled = false

[plugins."demo@market".mcp_servers.notes]
trusted_launch_hash = "sha256:abc"
        "#,
        )
        .unwrap();
        assert_eq!(config, expected);
    }

    /// A plugin can be reviewed while declaring no MCP servers at all, and that
    /// review is what makes a server appearing later a new decision.
    #[tokio::test]
    async fn a_plugin_with_no_servers_still_records_that_it_was_reviewed() {
        let codex_home = TempDir::new().unwrap();
        fs::write(
            codex_home.path().join(CONFIG_TOML_FILE),
            r#"
[plugins."demo@market"]
enabled = true
"#,
        )
        .unwrap();

        apply_user_plugin_config_edits(
            codex_home.path(),
            vec![PluginConfigEdit::MarkMcpLaunchesPinned {
                plugin_key: "demo@market".to_string(),
            }],
        )
        .await
        .unwrap();

        let config = read_config(codex_home.path());
        let expected: toml::Value = toml::from_str(
            r#"
[plugins."demo@market"]
enabled = true
mcp_launch_pinned = true
        "#,
        )
        .unwrap();
        assert_eq!(config, expected);
    }

    #[tokio::test]
    async fn recording_the_approved_files_leaves_the_rest_of_the_plugin_alone() {
        let codex_home = TempDir::new().unwrap();
        fs::write(
            codex_home.path().join(CONFIG_TOML_FILE),
            r#"
[plugins."demo@market"]
enabled = true

[plugins."demo@market".mcp_servers.notes]
trusted_launch_hash = "sha256:abc"
"#,
        )
        .unwrap();

        apply_user_plugin_config_edits(
            codex_home.path(),
            vec![PluginConfigEdit::SetTrustedPackageHash {
                plugin_key: "demo@market".to_string(),
                package_hash: "sha256:files".to_string(),
            }],
        )
        .await
        .unwrap();

        let config = read_config(codex_home.path());
        let expected: toml::Value = toml::from_str(
            r#"
[plugins."demo@market"]
enabled = true
trusted_package_hash = "sha256:files"

[plugins."demo@market".mcp_servers.notes]
trusted_launch_hash = "sha256:abc"
        "#,
        )
        .unwrap();
        assert_eq!(config, expected);
    }

    #[tokio::test]
    async fn approving_a_package_again_replaces_the_hash_it_supersedes() {
        let codex_home = TempDir::new().unwrap();
        for package_hash in ["sha256:first", "sha256:second"] {
            apply_user_plugin_config_edits(
                codex_home.path(),
                vec![PluginConfigEdit::SetTrustedPackageHash {
                    plugin_key: "demo@market".to_string(),
                    package_hash: package_hash.to_string(),
                }],
            )
            .await
            .unwrap();
        }

        let config = read_config(codex_home.path());
        let expected: toml::Value = toml::from_str(
            r#"
[plugins."demo@market"]
trusted_package_hash = "sha256:second"
        "#,
        )
        .unwrap();
        assert_eq!(config, expected);
    }

    #[tokio::test]
    async fn a_new_launch_pin_replaces_the_one_it_supersedes() {
        let codex_home = TempDir::new().unwrap();
        for launch_hash in ["sha256:first", "sha256:second"] {
            apply_user_plugin_config_edits(
                codex_home.path(),
                vec![PluginConfigEdit::SetMcpTrustedLaunchHash {
                    plugin_key: "demo@market".to_string(),
                    server_name: "notes".to_string(),
                    launch_hash: launch_hash.to_string(),
                }],
            )
            .await
            .unwrap();
        }

        let config = read_config(codex_home.path());
        let expected: toml::Value = toml::from_str(
            r#"
[plugins."demo@market".mcp_servers.notes]
trusted_launch_hash = "sha256:second"
        "#,
        )
        .unwrap();
        assert_eq!(config, expected);
    }

    #[tokio::test]
    async fn set_user_plugin_enabled_preserves_existing_plugin_fields() {
        let codex_home = TempDir::new().unwrap();
        fs::write(
            codex_home.path().join(CONFIG_TOML_FILE),
            r#"
[plugins."demo@market"]
enabled = false
source = "/tmp/plugin"
"#,
        )
        .unwrap();

        set_user_plugin_enabled(
            codex_home.path(),
            "demo@market".to_string(),
            /*enabled*/ true,
        )
        .await
        .unwrap();

        let config = read_config(codex_home.path());
        let expected: toml::Value = toml::from_str(
            r#"
[plugins."demo@market"]
enabled = true
source = "/tmp/plugin"
        "#,
        )
        .unwrap();
        assert_eq!(config, expected);
    }

    #[tokio::test]
    async fn clear_user_plugin_removes_empty_plugins_table() {
        let codex_home = TempDir::new().unwrap();
        fs::write(
            codex_home.path().join(CONFIG_TOML_FILE),
            r#"
[plugins."demo@market"]
enabled = true
"#,
        )
        .unwrap();

        clear_user_plugin(codex_home.path(), "demo@market".to_string())
            .await
            .unwrap();

        assert_eq!(
            fs::read_to_string(codex_home.path().join(CONFIG_TOML_FILE)).unwrap(),
            ""
        );
    }

    #[tokio::test]
    async fn clear_user_plugin_missing_entry_does_not_create_config() {
        let codex_home = TempDir::new().unwrap();

        clear_user_plugin(codex_home.path(), "demo@market".to_string())
            .await
            .unwrap();

        assert!(!codex_home.path().join(CONFIG_TOML_FILE).exists());
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn set_user_plugin_enabled_follows_config_symlink() {
        use std::os::unix::fs::symlink;

        let codex_home = TempDir::new().unwrap();
        let target_path = codex_home.path().join("target_config.toml");
        symlink(&target_path, codex_home.path().join(CONFIG_TOML_FILE)).unwrap();

        set_user_plugin_enabled(
            codex_home.path(),
            "demo@market".to_string(),
            /*enabled*/ true,
        )
        .await
        .unwrap();

        let config =
            toml::from_str::<toml::Value>(&fs::read_to_string(target_path).unwrap()).unwrap();
        let expected: toml::Value = toml::from_str(
            r#"
[plugins."demo@market"]
enabled = true
        "#,
        )
        .unwrap();
        assert_eq!(config, expected);
    }

    fn read_config(codex_home: &Path) -> toml::Value {
        toml::from_str(&fs::read_to_string(codex_home.join(CONFIG_TOML_FILE)).unwrap()).unwrap()
    }
}

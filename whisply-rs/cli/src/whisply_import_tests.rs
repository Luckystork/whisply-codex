//! Tests for the copy-only Codex import.
//!
//! The interesting properties here are all negative -- what the command
//! refuses to read, refuses to overwrite, and refuses to change -- so most of
//! these assert an absence.

use super::*;

use std::collections::BTreeMap;

/// Every regular file under `root`, keyed by relative path, with its bytes.
///
/// Used to prove the source is untouched: comparing the whole tree catches a
/// deletion, a truncation, and a marker file written back, which a check of
/// one known path would not.
fn snapshot_tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut found = BTreeMap::new();
    collect_tree(root, root, &mut found);
    found
}

fn collect_tree(root: &Path, current: &Path, found: &mut BTreeMap<String, Vec<u8>>) {
    let Ok(entries) = fs::read_dir(current) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        let relative = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .into_owned();
        if metadata.is_dir() {
            found.insert(format!("{relative}/"), Vec::new());
            collect_tree(root, &path, found);
        } else if let Ok(bytes) = fs::read(&path) {
            found.insert(relative, bytes);
        }
    }
}

/// A `.codex` home holding one of everything worth deciding about.
fn codex_home(root: &Path) -> PathBuf {
    let home = root.join("dot-codex");
    fs::create_dir_all(&home).expect("create source home");

    fs::write(home.join("AGENTS.md"), b"be careful\n").expect("write instructions");
    fs::write(
        home.join("config.toml"),
        concat!(
            "model = \"gpt-5.6-luna\"\n",
            "model_reasoning_effort = \"high\"\n",
            "notify = [\"/usr/local/bin/tell-me\"]\n",
            "\n[tui]\nresume_cwd = \"session\"\n",
            "\n[mcp_servers.acme]\ncommand = \"acme\"\n",
            "\n[mcp_servers.acme.env]\nACME_TOKEN = \"sk-live-should-never-move\"\n",
            "\n[projects.\"/Users/someone/secret-project\"]\ntrust_level = \"trusted\"\n",
        ),
    )
    .expect("write config");

    fs::write(home.join("auth.json"), b"{\"token\":\"sk-live-secret\"}\n").expect("write auth");
    fs::write(home.join("installation_id"), b"abc123\n").expect("write installation id");
    fs::write(home.join("history.jsonl"), b"{}\n").expect("write history");
    fs::write(home.join("hooks.json"), b"{}\n").expect("write hooks");
    fs::write(home.join("state_5.sqlite"), b"sqlite").expect("write db");
    fs::create_dir_all(home.join("sessions")).expect("create sessions");
    fs::write(home.join("sessions/rollout.jsonl"), b"{}\n").expect("write rollout");
    fs::create_dir_all(home.join("plugins/acme")).expect("create plugins");

    let skill = home.join("skills/tidy");
    fs::create_dir_all(&skill).expect("create skill");
    fs::write(
        skill.join("SKILL.md"),
        b"---\nname: tidy\n---\n\nTidy up.\n",
    )
    .expect("write skill");

    home
}

fn whisply_home(root: &Path) -> PathBuf {
    let home = root.join("dot-whisply");
    fs::create_dir_all(&home).expect("create destination home");
    home
}

fn outcome_for<'a>(entries: &'a [PlannedEntry], name: &str) -> &'a Planned {
    &entries
        .iter()
        .find(|entry| entry.name == name)
        .unwrap_or_else(|| panic!("no planned entry for {name}"))
        .planned
}

fn is_import(planned: &Planned) -> bool {
    matches!(planned, Planned::Import { .. })
}

fn skip_reason(planned: &Planned) -> Skipped {
    match planned {
        Planned::Skip { reason, .. } => *reason,
        other => panic!("expected a skip, got {other:?}"),
    }
}

#[test]
fn only_the_three_supported_shapes_are_importable() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = codex_home(temp.path());
    let destination = whisply_home(temp.path());

    let entries = plan(&source, &destination).expect("plan");
    let imported: Vec<&str> = entries
        .iter()
        .filter(|entry| is_import(&entry.planned))
        .map(|entry| entry.name.as_str())
        .collect();

    assert_eq!(imported, vec!["AGENTS.md", "config.toml", "skills/tidy"]);
}

#[test]
fn an_unrecognized_entry_is_skipped_rather_than_imported() {
    // The property that has to survive Codex adding things: the gate is an
    // allowlist, so something nobody has classified is left alone by default
    // instead of being copied because no rule forbade it.
    let temp = tempfile::tempdir().expect("tempdir");
    let source = codex_home(temp.path());
    fs::write(source.join("something-new-next-year"), b"?").expect("write new entry");
    let destination = whisply_home(temp.path());

    let entries = plan(&source, &destination).expect("plan");

    assert_eq!(
        skip_reason(outcome_for(&entries, "something-new-next-year")),
        Skipped::Unsupported
    );
}

#[test]
fn credentials_and_private_storage_are_never_imported() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = codex_home(temp.path());
    let destination = whisply_home(temp.path());

    let entries = plan(&source, &destination).expect("plan");

    assert_eq!(
        skip_reason(outcome_for(&entries, "auth.json")),
        Skipped::Credentials
    );
    assert_eq!(
        skip_reason(outcome_for(&entries, "installation_id")),
        Skipped::Credentials
    );
    assert_eq!(
        skip_reason(outcome_for(&entries, "sessions")),
        Skipped::PrivateStorage
    );
    assert_eq!(
        skip_reason(outcome_for(&entries, "history.jsonl")),
        Skipped::PrivateStorage
    );
    assert_eq!(
        skip_reason(outcome_for(&entries, "state_5.sqlite")),
        Skipped::PrivateStorage
    );
    assert_eq!(
        skip_reason(outcome_for(&entries, "hooks.json")),
        Skipped::ExecutableContent
    );
    assert_eq!(
        skip_reason(outcome_for(&entries, "plugins")),
        Skipped::ExecutableContent
    );
}

#[test]
fn configuration_import_carries_supported_preferences_and_nothing_else() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = codex_home(temp.path());
    let destination = whisply_home(temp.path());

    write_config(&source, &destination).expect("write config");
    let written = fs::read_to_string(destination.join("config.toml")).expect("read imported");

    assert!(
        written.contains("gpt-5.6-luna"),
        "kept the model: {written}"
    );
    assert!(
        written.contains("high"),
        "kept the reasoning effort: {written}"
    );
    assert!(
        written.contains("resume_cwd"),
        "kept the nested key: {written}"
    );

    // The three that must not ride along: an MCP token, a command line, and
    // trust decisions about directories on the old machine.
    assert!(
        !written.contains("sk-live-should-never-move"),
        "an MCP server token was imported: {written}"
    );
    assert!(
        !written.contains("mcp_servers"),
        "MCP servers were imported: {written}"
    );
    assert!(
        !written.contains("notify"),
        "a command line was imported: {written}"
    );
    assert!(
        !written.contains("projects"),
        "trust decisions were imported: {written}"
    );
    assert!(
        !written.contains("secret-project"),
        "a local path was imported: {written}"
    );
}

#[test]
fn an_existing_destination_is_reported_rather_than_replaced() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = codex_home(temp.path());
    let destination = whisply_home(temp.path());
    fs::write(destination.join("AGENTS.md"), b"mine\n").expect("write existing");

    let mut entries = plan(&source, &destination).expect("plan");
    assert!(matches!(
        outcome_for(&entries, "AGENTS.md"),
        Planned::Conflict { .. }
    ));

    apply(&source, &destination, &mut entries).expect("apply");

    assert_eq!(
        fs::read_to_string(destination.join("AGENTS.md")).expect("read"),
        "mine\n",
        "the existing file was replaced"
    );
}

#[test]
fn a_conflict_does_not_stop_the_rest_of_the_import() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = codex_home(temp.path());
    let destination = whisply_home(temp.path());
    fs::write(destination.join("AGENTS.md"), b"mine\n").expect("write existing");

    let mut entries = plan(&source, &destination).expect("plan");
    apply(&source, &destination, &mut entries).expect("apply");

    assert!(
        destination.join("config.toml").is_file(),
        "config was not imported"
    );
    assert!(
        destination.join("skills/tidy/SKILL.md").is_file(),
        "skills were not imported"
    );
}

#[test]
fn an_existing_skill_is_kept_rather_than_replaced() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = codex_home(temp.path());
    let destination = whisply_home(temp.path());
    let existing = destination.join("skills/tidy");
    fs::create_dir_all(&existing).expect("create existing skill");
    fs::write(existing.join("SKILL.md"), b"mine\n").expect("write existing skill");

    let mut entries = plan(&source, &destination).expect("plan");
    assert!(matches!(
        outcome_for(&entries, "skills/tidy"),
        Planned::Conflict { .. }
    ));
    apply(&source, &destination, &mut entries).expect("apply");

    assert_eq!(
        fs::read_to_string(existing.join("SKILL.md")).expect("read"),
        "mine\n",
        "an existing skill package was replaced"
    );
}

#[test]
fn a_skill_carrying_a_credential_file_is_left_behind() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = codex_home(temp.path());
    let destination = whisply_home(temp.path());
    let leaky = source.join("skills/leaky");
    fs::create_dir_all(&leaky).expect("create skill");
    fs::write(leaky.join("SKILL.md"), b"---\nname: leaky\n---\n").expect("write skill");
    fs::write(leaky.join(".env"), b"TOKEN=sk-live-secret\n").expect("write credential");

    let mut entries = plan(&source, &destination).expect("plan");

    // The preview has to say so before anything is copied. A plan that claimed
    // to import the package and then quietly dropped it would be worse than
    // refusing outright, because the user would believe they had it.
    assert_eq!(
        skip_reason(outcome_for(&entries, "skills/leaky")),
        Skipped::Credentials
    );

    apply(&source, &destination, &mut entries).expect("apply");

    assert!(
        !destination.join("skills/leaky").exists(),
        "a package holding a credential file was imported"
    );
    assert!(
        destination.join("skills/tidy/SKILL.md").is_file(),
        "the clean package should still import"
    );
}

#[test]
fn importing_does_not_change_the_codex_home() {
    // WCD-310. The whole source tree is compared, so a deletion, a truncation,
    // or a "migrated" marker written back would all fail this.
    let temp = tempfile::tempdir().expect("tempdir");
    let source = codex_home(temp.path());
    let destination = whisply_home(temp.path());
    let before = snapshot_tree(&source);

    let mut entries = plan(&source, &destination).expect("plan");
    apply(&source, &destination, &mut entries).expect("apply");

    assert_eq!(snapshot_tree(&source), before, "the Codex home changed");
}

#[test]
fn importing_twice_changes_nothing_the_second_time() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = codex_home(temp.path());
    let destination = whisply_home(temp.path());

    let mut first = plan(&source, &destination).expect("plan");
    apply(&source, &destination, &mut first).expect("apply");
    let after_first = snapshot_tree(&destination);

    let mut second = plan(&source, &destination).expect("replan");
    // Everything importable is now a conflict, which is what makes the second
    // run a no-op rather than a re-copy.
    assert!(
        !second.iter().any(|entry| is_import(&entry.planned)),
        "a second import still wanted to write something"
    );
    apply(&source, &destination, &mut second).expect("reapply");

    assert_eq!(snapshot_tree(&destination), after_first);
}

#[test]
fn a_preview_writes_nothing() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = codex_home(temp.path());
    let destination = whisply_home(temp.path());
    let before = snapshot_tree(&destination);

    let entries = plan(&source, &destination).expect("plan");

    assert!(entries.iter().any(|entry| is_import(&entry.planned)));
    assert_eq!(
        snapshot_tree(&destination),
        before,
        "planning wrote something"
    );
}

#[test]
fn a_config_with_no_supported_preference_is_not_imported() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = codex_home(temp.path());
    let destination = whisply_home(temp.path());
    fs::write(
        source.join("config.toml"),
        "[mcp_servers.acme.env]\nACME_TOKEN = \"sk-live\"\n",
    )
    .expect("rewrite config");

    let entries = plan(&source, &destination).expect("plan");

    assert!(
        !is_import(outcome_for(&entries, "config.toml")),
        "an empty import would have created a config file holding nothing"
    );
}

#[test]
fn write_new_file_refuses_an_existing_path() {
    // The check in `plan` can lose a race with anything else writing into the
    // managed home, so the guarantee has to hold at the syscall too.
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("taken");
    fs::write(&path, b"mine").expect("write");

    assert!(write_new_file(&path, b"theirs").is_err());
    assert_eq!(fs::read(&path).expect("read"), b"mine");
}

#[test]
fn the_preview_names_each_skill_package_rather_than_the_directory() {
    // Guards the reason skills are planned per package: a single `skills` row
    // reports success for packages the validator is about to refuse, so the
    // preview would promise more than the apply delivers.
    let temp = tempfile::tempdir().expect("tempdir");
    let source = codex_home(temp.path());
    let destination = whisply_home(temp.path());
    let leaky = source.join("skills/leaky");
    fs::create_dir_all(&leaky).expect("create skill");
    fs::write(leaky.join("SKILL.md"), b"---\nname: leaky\n---\n").expect("write skill");
    fs::write(leaky.join("credentials.json"), b"{}\n").expect("write credential");

    let entries = plan(&source, &destination).expect("plan");
    let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();

    assert!(
        !names.contains(&"skills"),
        "the directory was planned as one row"
    );
    assert!(names.contains(&"skills/tidy"));
    assert!(names.contains(&"skills/leaky"));
}

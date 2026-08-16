use std::collections::HashSet;

use whisply_core_skills::injection::extract_tool_mentions;
use whisply_protocol::protocol::SkillScope;
use whisply_protocol::user_input::UserInput;

use crate::catalog::SkillAuthority;
use crate::catalog::SkillCatalog;
use crate::catalog::SkillCatalogEntry;
use crate::catalog::SkillPackageId;

const SKILL_PATH_PREFIX: &str = "skill://";

#[tracing::instrument(
    level = "trace",
    skip_all,
    fields(
        input_count = inputs.len(),
        catalog_entry_count = catalog.entries.len()
    )
)]
pub(crate) fn collect_explicit_skill_mentions(
    inputs: &[UserInput],
    catalog: &SkillCatalog,
) -> ExplicitCatalogMentions {
    let mut selected = Vec::new();
    let mut seen = HashSet::new();
    let mut blocked_plain_names = HashSet::new();
    let mut unused: Vec<String> = Vec::new();

    for input in inputs {
        match input {
            UserInput::Skill { name, path } => {
                blocked_plain_names.insert(name.clone());
                select_by_path(catalog, &path.to_string_lossy(), &mut seen, &mut selected);
            }
            UserInput::Mention { name, path } if path_is_skill(path) => {
                blocked_plain_names.insert(name.clone());
                select_by_path(catalog, path, &mut seen, &mut selected);
            }
            UserInput::Text { .. }
            | UserInput::Image { .. }
            | UserInput::LocalImage { .. }
            | UserInput::Audio { .. }
            | UserInput::LocalAudio { .. } => {}
            UserInput::Mention { .. } => {}
            _ => {}
        }
    }

    for input in inputs {
        let UserInput::Text { text, .. } = input else {
            continue;
        };

        let mentions = extract_tool_mentions(text);
        for path in mentions.paths() {
            if path_is_skill(path) {
                select_by_path(
                    catalog,
                    normalize_skill_path(path),
                    &mut seen,
                    &mut selected,
                );
            }
        }
        for name in mentions.plain_names() {
            if blocked_plain_names.contains(name) {
                continue;
            }
            let answering = catalog
                .entries
                .iter()
                .filter(|entry| entry.enabled && entry.name == name)
                .collect::<Vec<_>>();
            // A catalog merges packages from sources that never agreed on
            // names, so first-in-the-list is an accident of merge order, not a
            // choice. Take the name only when it can mean one package.
            match answering.as_slice() {
                [entry] => push_selected(entry, &mut seen, &mut selected),
                [] => {}
                _ => {
                    if !unused.contains(&name.to_string()) {
                        unused.push(name.to_string());
                    }
                }
            }
        }
    }

    let selected_names = selected
        .iter()
        .map(|entry| entry.name.clone())
        .collect::<HashSet<_>>();
    unused.retain(|name| !selected_names.contains(name));
    let unused = unused
        .into_iter()
        .map(|name| {
            let candidates = catalog
                .entries
                .iter()
                .filter(|entry| entry.enabled && entry.name == name)
                .map(catalog_entry_origin)
                .collect::<Vec<_>>();
            UnusedCatalogMention { name, candidates }
        })
        .collect();

    ExplicitCatalogMentions { selected, unused }
}

/// The selection, plus the names it would have had to guess at.
pub(crate) struct ExplicitCatalogMentions {
    pub(crate) selected: Vec<SkillCatalogEntry>,
    pub(crate) unused: Vec<UnusedCatalogMention>,
}

/// A `$name` that named more than one installed package, so it ran none.
pub(crate) struct UnusedCatalogMention {
    pub(crate) name: String,
    pub(crate) candidates: Vec<String>,
}

impl UnusedCatalogMention {
    pub(crate) fn describe(&self) -> String {
        format!(
            "{count} skills are named \"{name}\", so `${name}` did not run any of them: \
             {candidates}. Pick one from the skill list to run it.",
            count = self.candidates.len(),
            name = self.name,
            candidates = self.candidates.join(", "),
        )
    }
}

/// Enough to tell two same-named packages apart in a sentence.
///
/// The displayed path is what the skill list shows, so it is the thing a person
/// can match against; a package that has none falls back to the source that
/// owns it, which is still better than two identical rows.
fn catalog_entry_origin(entry: &SkillCatalogEntry) -> String {
    let scope = match entry.prompt_scope() {
        Some(SkillScope::Repo) => Some("repository"),
        Some(SkillScope::User) => Some("home"),
        Some(SkillScope::System) => Some("system"),
        Some(SkillScope::Admin) => Some("administrator"),
        None => None,
    };
    let location = entry
        .display_path
        .clone()
        .unwrap_or_else(|| format!("{} {}", entry.authority.kind, entry.id.0));
    match scope {
        Some(scope) => format!("the {scope} one ({location})"),
        None => format!("the one at {location}"),
    }
}

fn select_by_path(
    catalog: &SkillCatalog,
    path: &str,
    seen: &mut HashSet<SkillCatalogEntryKey>,
    selected: &mut Vec<SkillCatalogEntry>,
) {
    let normalized_path = normalize_skill_path(path);
    for entry in catalog.entries.iter().filter(|entry| entry.enabled) {
        if entry_matches_path(entry, normalized_path) {
            push_selected(entry, seen, selected);
        }
    }
}

fn push_selected(
    entry: &SkillCatalogEntry,
    seen: &mut HashSet<SkillCatalogEntryKey>,
    selected: &mut Vec<SkillCatalogEntry>,
) {
    let key = SkillCatalogEntryKey::from(entry);
    if seen.insert(key) {
        selected.push(entry.clone());
    }
}

fn entry_matches_path(entry: &SkillCatalogEntry, path: &str) -> bool {
    entry.main_prompt.as_str() == path
        || entry.id.0 == path
        || entry
            .display_path
            .as_deref()
            .is_some_and(|display_path| normalize_skill_path(display_path) == path)
}

fn path_is_skill(path: &str) -> bool {
    path.starts_with(SKILL_PATH_PREFIX)
        || path
            .rsplit(['/', '\\'])
            .next()
            .is_some_and(|file_name| file_name.eq_ignore_ascii_case("SKILL.md"))
}

fn normalize_skill_path(path: &str) -> &str {
    path.strip_prefix(SKILL_PATH_PREFIX).unwrap_or(path)
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct SkillCatalogEntryKey {
    authority: SkillAuthority,
    package: SkillPackageId,
}

impl From<&SkillCatalogEntry> for SkillCatalogEntryKey {
    fn from(entry: &SkillCatalogEntry) -> Self {
        Self {
            authority: entry.authority.clone(),
            package: entry.id.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::SkillResourceId;
    use crate::catalog::SkillSourceKind;

    fn entry(name: &str, source: SkillSourceKind, path: &str) -> SkillCatalogEntry {
        SkillCatalogEntry::new(
            SkillPackageId(path.to_string()),
            SkillAuthority::new(source, "authority"),
            name,
            format!("{name} skill"),
            SkillResourceId::new(path),
        )
        .with_display_path(path)
    }

    fn catalog(entries: Vec<SkillCatalogEntry>) -> SkillCatalog {
        let mut catalog = SkillCatalog::default();
        catalog.entries = entries;
        catalog
    }

    fn text(text: &str) -> Vec<UserInput> {
        vec![UserInput::Text {
            text: text.to_string(),
            text_elements: Vec::new(),
        }]
    }

    /// The catalog merges sources that never agreed on names, so the first row
    /// is merge order. Running it would run a package the person never chose,
    /// and running it quietly would leave them no way to notice.
    #[test]
    fn a_name_two_sources_answer_to_runs_neither_and_says_so() {
        let catalog = catalog(vec![
            entry("review", SkillSourceKind::Host, "/repo/review/SKILL.md"),
            entry("review", SkillSourceKind::Executor, "/env/review/SKILL.md"),
        ]);

        let mentions = collect_explicit_skill_mentions(&text("please $review this"), &catalog);

        assert!(mentions.selected.is_empty());
        assert_eq!(mentions.unused.len(), 1);
        let message = mentions.unused[0].describe();
        assert!(
            message.contains("2 skills are named \"review\""),
            "{message}"
        );
        assert!(message.contains("/repo/review/SKILL.md"), "{message}");
        assert!(message.contains("/env/review/SKILL.md"), "{message}");
    }

    /// The disabled package is not a package the mention could have meant, so
    /// the name is not ambiguous and the remaining one runs.
    #[test]
    fn a_disabled_package_does_not_make_a_name_ambiguous() {
        let mut disabled = entry("review", SkillSourceKind::Executor, "/env/review/SKILL.md");
        disabled.enabled = false;
        let catalog = catalog(vec![
            entry("review", SkillSourceKind::Host, "/repo/review/SKILL.md"),
            disabled,
        ]);

        let mentions = collect_explicit_skill_mentions(&text("please $review this"), &catalog);

        assert_eq!(mentions.selected.len(), 1);
        assert!(mentions.unused.is_empty());
    }

    /// A mention by path chose; the name it shares with another package is not
    /// this turn's problem.
    #[test]
    fn a_mention_by_path_still_chooses_between_same_named_packages() {
        let catalog = catalog(vec![
            entry("review", SkillSourceKind::Host, "/repo/review/SKILL.md"),
            entry("review", SkillSourceKind::Executor, "/env/review/SKILL.md"),
        ]);

        let mentions = collect_explicit_skill_mentions(
            &text("please [$review](/env/review/SKILL.md) this"),
            &catalog,
        );

        assert_eq!(mentions.selected.len(), 1);
        assert_eq!(mentions.selected[0].id.0, "/env/review/SKILL.md");
        assert!(mentions.unused.is_empty());
    }

    #[test]
    fn an_unambiguous_name_still_runs_without_a_warning() {
        let catalog = catalog(vec![entry(
            "review",
            SkillSourceKind::Host,
            "/repo/review/SKILL.md",
        )]);

        let mentions = collect_explicit_skill_mentions(&text("please $review this"), &catalog);

        assert_eq!(mentions.selected.len(), 1);
        assert!(mentions.unused.is_empty());
    }
}

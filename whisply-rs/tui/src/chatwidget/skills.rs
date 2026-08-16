use std::collections::HashMap;
use std::collections::HashSet;

use super::ChatWidget;
use crate::app_event::AppEvent;
use crate::bottom_pane::SelectionItem;
use crate::bottom_pane::SelectionViewParams;
use crate::bottom_pane::SkillsToggleItem;
use crate::bottom_pane::SkillsToggleView;
use crate::bottom_pane::popup_consts::standard_popup_hint_line;
use crate::skills_helpers::skill_description;
use crate::skills_helpers::skill_display_name;
use codex_app_server_protocol::SkillMetadata;
use codex_app_server_protocol::SkillScope;
use codex_app_server_protocol::SkillsListEntry;
use codex_app_server_protocol::SkillsListResponse;
use whisply_connectors::AppInfo;
use whisply_features::Feature;
use whisply_protocol::parse_command::ParsedCommand;
use whisply_utils_absolute_path::AbsolutePathBuf;
use whisply_utils_plugins::mention_syntax::TOOL_MENTION_SIGIL;

impl ChatWidget {
    pub(crate) fn open_skills_list(&mut self) {
        if self.config.features.enabled(Feature::MentionsV2) {
            self.insert_str("@");
        } else {
            self.insert_str("$");
        }
    }

    pub(crate) fn open_skills_menu(&mut self) {
        let list_shortcut = if self.config.features.enabled(Feature::MentionsV2) {
            '@'
        } else {
            '$'
        };
        let items = vec![
            SelectionItem {
                name: "List skills".to_string(),
                description: Some(format!(
                    "Tip: press {list_shortcut} to open this list directly."
                )),
                actions: vec![Box::new(|tx| {
                    tx.send(AppEvent::OpenSkillsList);
                })],
                dismiss_on_select: true,
                ..Default::default()
            },
            SelectionItem {
                name: "Enable/Disable Skills".to_string(),
                description: Some("Enable or disable skills.".to_string()),
                actions: vec![Box::new(|tx| {
                    tx.send(AppEvent::OpenManageSkillsPopup);
                })],
                dismiss_on_select: true,
                ..Default::default()
            },
        ];

        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some("Skills".to_string()),
            subtitle: Some("Choose an action".to_string()),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            ..Default::default()
        });
    }

    pub(crate) fn open_manage_skills_popup(&mut self) {
        if self.skills_all.is_empty() {
            self.add_info_message("No skills available.".to_string(), /*hint*/ None);
            return;
        }

        let mut initial_state = HashMap::new();
        for skill in &self.skills_all {
            initial_state.insert(skill.path.clone(), skill.enabled);
        }
        self.skills_initial_state = Some(initial_state);

        // Two packages can carry one name. Rows that read identically are the
        // shadowing this list is supposed to make visible, not hide.
        let row_names = disambiguated_skill_rows(&self.skills_all);
        let items: Vec<SkillsToggleItem> = self
            .skills_all
            .iter()
            .zip(row_names)
            .map(|(skill, display_name)| {
                let description = skill_description(skill).to_string();
                SkillsToggleItem {
                    name: display_name,
                    skill_name: skill.name.clone(),
                    description,
                    enabled: skill.enabled,
                    path: skill.path.clone(),
                }
            })
            .collect();

        let view = SkillsToggleView::new(
            items,
            self.app_event_tx.clone(),
            self.bottom_pane.list_keymap(),
        );
        self.bottom_pane.show_view(Box::new(view));
    }

    pub(crate) fn update_skill_enabled(&mut self, path: AbsolutePathBuf, enabled: bool) {
        for skill in &mut self.skills_all {
            if skill.path == path {
                skill.enabled = enabled;
            }
        }
        self.set_skills(Some(enabled_skills_for_mentions(&self.skills_all)));
    }

    pub(crate) fn handle_manage_skills_closed(&mut self) {
        let Some(initial_state) = self.skills_initial_state.take() else {
            return;
        };
        let mut current_state = HashMap::new();
        for skill in &self.skills_all {
            current_state.insert(skill.path.clone(), skill.enabled);
        }

        let mut enabled_count = 0;
        let mut disabled_count = 0;
        for (path, was_enabled) in initial_state {
            let Some(is_enabled) = current_state.get(&path) else {
                continue;
            };
            if was_enabled != *is_enabled {
                if *is_enabled {
                    enabled_count += 1;
                } else {
                    disabled_count += 1;
                }
            }
        }

        if enabled_count == 0 && disabled_count == 0 {
            return;
        }
        self.add_info_message(
            format!("{enabled_count} skills enabled, {disabled_count} skills disabled"),
            /*hint*/ None,
        );
    }

    pub(crate) fn set_skills_from_response(&mut self, response: &SkillsListResponse) {
        let skills = skills_for_cwd(&self.config.cwd, &response.data);
        self.skills_all = skills;
        self.set_skills(Some(enabled_skills_for_mentions(&self.skills_all)));
    }

    pub(crate) fn annotate_skill_reads_in_parsed_cmd(
        &self,
        mut parsed_cmd: Vec<ParsedCommand>,
    ) -> Vec<ParsedCommand> {
        if self.skills_all.is_empty() {
            return parsed_cmd;
        }

        for parsed in &mut parsed_cmd {
            let ParsedCommand::Read { name, path, .. } = parsed else {
                continue;
            };
            if name != "SKILL.md" {
                continue;
            }

            // Best effort only: annotate exact SKILL.md path matches from the loaded skills list.
            if let Some(skill) = self
                .skills_all
                .iter()
                .find(|skill| skill.path.as_path() == path)
            {
                *name = format!("{name} ({} skill)", skill.name);
            }
        }

        parsed_cmd
    }
}

fn skills_for_cwd(cwd: &AbsolutePathBuf, skills_entries: &[SkillsListEntry]) -> Vec<SkillMetadata> {
    skills_entries
        .iter()
        .find(|entry| entry.cwd.as_path() == cwd.as_path())
        .map(|entry| entry.skills.clone())
        .unwrap_or_default()
}

fn enabled_skills_for_mentions(skills: &[SkillMetadata]) -> Vec<SkillMetadata> {
    skills
        .iter()
        .filter(|skill| skill.enabled)
        .cloned()
        .collect()
}

pub(crate) fn collect_tool_mentions(
    text: &str,
    mention_paths: &HashMap<String, String>,
) -> ToolMentions {
    let mut mentions = extract_tool_mentions_from_text(text);
    for (name, path) in mention_paths {
        if mentions.names.contains(name) {
            mentions.linked_paths.insert(name.clone(), path.clone());
        }
    }
    mentions
}

/// What a set of mentions resolved to, and what it could equally have meant.
///
/// A name is not a unique handle: the same skill name can be installed in a
/// repository, in the user's home, and in a plugin at once, and the loader
/// deliberately keeps all of them rather than picking one. Resolving a mention
/// by name therefore has to choose, and the choice is silent unless the
/// alternatives come back with it.
pub(crate) struct SkillMentionResolution {
    pub(crate) selected: Vec<SkillMetadata>,
    /// Same-named packages the mention passed over, in the order they were
    /// discovered.
    pub(crate) shadowed: Vec<SkillMetadata>,
}

pub(crate) fn find_skill_mentions_with_tool_mentions(
    mentions: &ToolMentions,
    skills: &[SkillMetadata],
) -> SkillMentionResolution {
    let mention_skill_paths: HashSet<&str> = mentions
        .linked_paths
        .values()
        .filter(|path| is_skill_path(path))
        .map(|path| normalize_skill_path(path))
        .collect();

    let mut seen_names = HashSet::new();
    let mut seen_paths = HashSet::new();
    let mut chosen_by_path = HashSet::new();
    let mut matches: Vec<SkillMetadata> = Vec::new();
    let mut shadowed: Vec<SkillMetadata> = Vec::new();

    for skill in skills {
        if seen_paths.contains(&skill.path) {
            continue;
        }
        let path_str = skill.path.to_string_lossy();
        if mention_skill_paths.contains(path_str.as_ref()) {
            seen_paths.insert(skill.path.clone());
            seen_names.insert(skill.name.clone());
            chosen_by_path.insert(skill.name.clone());
            matches.push(skill.clone());
        }
    }

    for skill in skills {
        if seen_paths.contains(&skill.path) {
            continue;
        }
        if !mentions.names.contains(&skill.name) {
            continue;
        }
        if seen_names.insert(skill.name.clone()) {
            seen_paths.insert(skill.path.clone());
            matches.push(skill.clone());
        } else if !chosen_by_path.contains(&skill.name) {
            // A mention that named a path already chose; only a mention by
            // name leaves a package standing that it could have meant.
            shadowed.push(skill.clone());
        }
    }

    SkillMentionResolution {
        selected: matches,
        shadowed,
    }
}

/// Says which of several same-named skills a mention used, and where the
/// others are.
///
/// Only the ones the mention actually passed over are reported: a duplicate
/// name that nobody mentioned is not this turn's problem, and a mention made
/// by path chose deliberately.
pub(crate) fn duplicate_skill_mention_notices(resolution: &SkillMentionResolution) -> Vec<String> {
    let mut notices = Vec::new();
    for used in &resolution.selected {
        let passed_over = resolution
            .shadowed
            .iter()
            .filter(|skill| skill.name == used.name)
            .collect::<Vec<_>>();
        if passed_over.is_empty() {
            continue;
        }
        let others = passed_over
            .iter()
            .map(|skill| skill_origin(skill))
            .collect::<Vec<_>>()
            .join(", ");
        notices.push(format!(
            "{} skills are named \"{}\". Used {}; did not use {}. Pick one from the skill \
             list to choose a different one.",
            passed_over.len() + 1,
            used.name,
            skill_origin(used),
            others
        ));
    }
    notices
}

/// Names for a list of skills in which no two rows read the same.
///
/// A name that appears once is left exactly as it was. A name that appears
/// more than once takes on where it came from, and if that is still not enough
/// to tell two rows apart it takes on its path, because a list that cannot
/// distinguish two packages cannot be used to enable one of them.
pub(crate) fn disambiguated_skill_rows(skills: &[SkillMetadata]) -> Vec<String> {
    let display_names = skills.iter().map(skill_display_name).collect::<Vec<_>>();
    let mut name_counts: HashMap<&str, usize> = HashMap::new();
    for name in &display_names {
        *name_counts.entry(name.as_str()).or_insert(0) += 1;
    }
    let mut scoped_counts: HashMap<(&str, &'static str), usize> = HashMap::new();
    for (skill, name) in skills.iter().zip(&display_names) {
        *scoped_counts
            .entry((name.as_str(), skill_scope_label(skill)))
            .or_insert(0) += 1;
    }

    skills
        .iter()
        .zip(&display_names)
        .map(|(skill, name)| {
            if name_counts.get(name.as_str()).copied().unwrap_or(0) <= 1 {
                return name.clone();
            }
            let scope = skill_scope_label(skill);
            if scoped_counts
                .get(&(name.as_str(), scope))
                .copied()
                .unwrap_or(0)
                <= 1
            {
                return format!("{name} ({scope})");
            }
            format!("{name} ({})", skill.path.to_string_lossy())
        })
        .collect()
}

fn skill_scope_label(skill: &SkillMetadata) -> &'static str {
    match skill.scope {
        SkillScope::Repo => "repository",
        SkillScope::User => "home",
        SkillScope::System => "system",
        SkillScope::Admin => "administrator",
    }
}

fn skill_origin(skill: &SkillMetadata) -> String {
    format!(
        "the {} one ({})",
        skill_scope_label(skill),
        skill.path.to_string_lossy()
    )
}

pub(crate) fn find_app_mentions(
    mentions: &ToolMentions,
    apps: &[AppInfo],
    skill_names_lower: &HashSet<String>,
) -> Vec<AppInfo> {
    let mut explicit_names = HashSet::new();
    let mut selected_ids = HashSet::new();
    for (name, path) in &mentions.linked_paths {
        if let Some(connector_id) = app_id_from_path(path) {
            explicit_names.insert(name.clone());
            selected_ids.insert(connector_id.to_string());
        }
    }

    let mut slug_counts: HashMap<String, usize> = HashMap::new();
    for app in apps.iter().filter(|app| is_app_mentionable(app)) {
        let slug = whisply_connectors::metadata::connector_mention_slug(app);
        *slug_counts.entry(slug).or_insert(0) += 1;
    }

    for app in apps.iter().filter(|app| is_app_mentionable(app)) {
        let slug = whisply_connectors::metadata::connector_mention_slug(app);
        let slug_count = slug_counts.get(&slug).copied().unwrap_or(0);
        if mentions.names.contains(&slug)
            && !explicit_names.contains(&slug)
            && slug_count == 1
            && !skill_names_lower.contains(&slug)
        {
            selected_ids.insert(app.id.clone());
        }
    }

    apps.iter()
        .filter(|app| is_app_mentionable(app) && selected_ids.contains(&app.id))
        .cloned()
        .collect()
}

pub(crate) fn is_app_mentionable(app: &AppInfo) -> bool {
    app.is_accessible && app.is_enabled
}

pub(crate) struct ToolMentions {
    names: HashSet<String>,
    linked_paths: HashMap<String, String>,
}

fn extract_tool_mentions_from_text(text: &str) -> ToolMentions {
    extract_tool_mentions_from_text_with_sigil(text, TOOL_MENTION_SIGIL)
}

fn extract_tool_mentions_from_text_with_sigil(text: &str, sigil: char) -> ToolMentions {
    let text_bytes = text.as_bytes();
    let mut names: HashSet<String> = HashSet::new();
    let mut linked_paths: HashMap<String, String> = HashMap::new();

    let mut index = 0;
    while index < text_bytes.len() {
        let byte = text_bytes[index];
        if byte == b'['
            && let Some((name, path, end_index)) =
                parse_linked_tool_mention(text, text_bytes, index, sigil)
        {
            if !is_common_env_var(name) {
                if is_skill_path(path) {
                    names.insert(name.to_string());
                }
                linked_paths
                    .entry(name.to_string())
                    .or_insert(path.to_string());
            }
            index = end_index;
            continue;
        }

        if byte != sigil as u8 {
            index += 1;
            continue;
        }

        let name_start = index + 1;
        let Some(first_name_byte) = text_bytes.get(name_start) else {
            index += 1;
            continue;
        };
        if !is_mention_name_char(*first_name_byte) {
            index += 1;
            continue;
        }

        let mut name_end = name_start + 1;
        while let Some(next_byte) = text_bytes.get(name_end)
            && is_mention_name_char(*next_byte)
        {
            name_end += 1;
        }

        let name = &text[name_start..name_end];
        if !is_common_env_var(name) {
            names.insert(name.to_string());
        }
        index = name_end;
    }

    ToolMentions {
        names,
        linked_paths,
    }
}

fn parse_linked_tool_mention<'a>(
    text: &'a str,
    text_bytes: &[u8],
    start: usize,
    sigil: char,
) -> Option<(&'a str, &'a str, usize)> {
    let sigil_index = start + 1;
    if text_bytes.get(sigil_index) != Some(&(sigil as u8)) {
        return None;
    }

    let name_start = sigil_index + 1;
    let first_name_byte = text_bytes.get(name_start)?;
    if !is_mention_name_char(*first_name_byte) {
        return None;
    }

    let mut name_end = name_start + 1;
    while let Some(next_byte) = text_bytes.get(name_end)
        && is_mention_name_char(*next_byte)
    {
        name_end += 1;
    }

    if text_bytes.get(name_end) != Some(&b']') {
        return None;
    }

    let mut path_start = name_end + 1;
    while let Some(next_byte) = text_bytes.get(path_start)
        && next_byte.is_ascii_whitespace()
    {
        path_start += 1;
    }
    if text_bytes.get(path_start) != Some(&b'(') {
        return None;
    }

    let mut path_end = path_start + 1;
    while let Some(next_byte) = text_bytes.get(path_end)
        && *next_byte != b')'
    {
        path_end += 1;
    }
    if text_bytes.get(path_end) != Some(&b')') {
        return None;
    }

    let path = text[path_start + 1..path_end].trim();
    if path.is_empty() {
        return None;
    }

    let name = &text[name_start..name_end];
    Some((name, path, path_end + 1))
}

fn is_common_env_var(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    matches!(
        upper.as_str(),
        "PATH"
            | "HOME"
            | "USER"
            | "SHELL"
            | "PWD"
            | "TMPDIR"
            | "TEMP"
            | "TMP"
            | "LANG"
            | "TERM"
            | "XDG_CONFIG_HOME"
    )
}

fn is_mention_name_char(byte: u8) -> bool {
    matches!(byte, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'-')
}

fn is_skill_path(path: &str) -> bool {
    !path.starts_with("app://") && !path.starts_with("mcp://") && !path.starts_with("plugin://")
}

fn normalize_skill_path(path: &str) -> &str {
    path.strip_prefix("skill://").unwrap_or(path)
}

fn app_id_from_path(path: &str) -> Option<&str> {
    path.strip_prefix("app://")
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::path::PathBuf;

    fn app(id: &str, name: &str) -> AppInfo {
        AppInfo {
            id: id.to_string(),
            name: name.to_string(),
            description: None,
            logo_url: None,
            logo_url_dark: None,
            icon_assets: None,
            icon_dark_assets: None,
            distribution_channel: None,
            branding: None,
            app_metadata: None,
            labels: None,
            install_url: None,
            is_accessible: true,
            is_enabled: true,
            plugin_display_names: Vec::new(),
        }
    }

    #[test]
    fn find_app_mentions_requires_accessible_enabled_apps_for_slugs() {
        let apps = vec![
            app("google_drive", "Google Drive"),
            AppInfo {
                is_accessible: false,
                ..app("arabica_uae", "% Arabica UAE")
            },
            AppInfo {
                is_enabled: false,
                ..app("linear", "Linear")
            },
        ];
        let mentions = collect_tool_mentions("$google-drive $arabica-uae $linear", &HashMap::new());

        assert_eq!(
            find_app_mentions(&mentions, &apps, &HashSet::new()),
            vec![apps[0].clone()]
        );
    }

    #[test]
    fn find_app_mentions_requires_accessible_enabled_apps_for_bound_paths() {
        let apps = vec![
            app("google_drive", "Google Drive"),
            AppInfo {
                is_accessible: false,
                ..app("arabica_uae", "% Arabica UAE")
            },
            AppInfo {
                is_enabled: false,
                ..app("linear", "Linear")
            },
        ];
        let mention_paths = HashMap::from([
            ("google-drive".to_string(), "app://google_drive".to_string()),
            ("arabica-uae".to_string(), "app://arabica_uae".to_string()),
            ("linear".to_string(), "app://linear".to_string()),
        ]);
        let mentions = collect_tool_mentions("$google-drive $arabica-uae $linear", &mention_paths);

        assert_eq!(
            find_app_mentions(&mentions, &apps, &HashSet::new()),
            vec![apps[0].clone()]
        );
    }

    fn skill(name: &str, path: &str, scope: SkillScope) -> SkillMetadata {
        SkillMetadata {
            name: name.to_string(),
            description: String::new(),
            short_description: None,
            version: None,
            interface: None,
            dependencies: None,
            path: AbsolutePathBuf::from_absolute_path(PathBuf::from(path))
                .expect("test skill path is absolute"),
            scope,
            enabled: true,
        }
    }

    /// A skill name is not unique: the same package name can be installed in a
    /// repository and in the user's home at once, and the loader deliberately
    /// keeps both. The mention still has to pick one, so the one it picked and
    /// the one it passed over both have to come back.
    #[test]
    fn a_mention_that_could_mean_two_skills_reports_the_one_it_did_not_use() {
        let skills = vec![
            skill(
                "review",
                "/repo/.codex/skills/review/SKILL.md",
                SkillScope::Repo,
            ),
            skill(
                "review",
                "/home/me/.codex/skills/review/SKILL.md",
                SkillScope::User,
            ),
        ];
        let mentions = collect_tool_mentions("$review please", &HashMap::new());

        let resolution = find_skill_mentions_with_tool_mentions(&mentions, &skills);

        assert_eq!(resolution.selected.len(), 1);
        assert_eq!(resolution.selected[0].scope, SkillScope::Repo);
        assert_eq!(resolution.shadowed.len(), 1);
        assert_eq!(resolution.shadowed[0].scope, SkillScope::User);

        let notices = duplicate_skill_mention_notices(&resolution);
        assert_eq!(notices.len(), 1);
        let notice = &notices[0];
        assert!(notice.contains("2 skills are named \"review\""), "{notice}");
        assert!(
            notice.contains("/repo/.codex/skills/review/SKILL.md"),
            "{notice}"
        );
        assert!(
            notice.contains("/home/me/.codex/skills/review/SKILL.md"),
            "{notice}"
        );
    }

    /// A mention made by path already chose. Reporting the other package there
    /// would be telling the person they might have meant something they
    /// explicitly did not pick.
    #[test]
    fn a_mention_made_by_path_is_not_reported_as_ambiguous() {
        let skills = vec![
            skill(
                "review",
                "/repo/.codex/skills/review/SKILL.md",
                SkillScope::Repo,
            ),
            skill(
                "review",
                "/home/me/.codex/skills/review/SKILL.md",
                SkillScope::User,
            ),
        ];
        let mention_paths = HashMap::from([(
            "review".to_string(),
            "/home/me/.codex/skills/review/SKILL.md".to_string(),
        )]);
        let mentions = collect_tool_mentions("$review please", &mention_paths);

        let resolution = find_skill_mentions_with_tool_mentions(&mentions, &skills);

        assert_eq!(resolution.selected.len(), 1);
        assert_eq!(resolution.selected[0].scope, SkillScope::User);
        assert!(resolution.shadowed.is_empty());
        assert!(duplicate_skill_mention_notices(&resolution).is_empty());
    }

    /// The enable/disable list is keyed by path, so it always toggles the
    /// right package — but two rows reading the same name give the person no
    /// way to know which one they toggled.
    #[test]
    fn a_list_never_shows_two_rows_a_person_cannot_tell_apart() {
        let skills = vec![
            skill(
                "review",
                "/repo/.codex/skills/review/SKILL.md",
                SkillScope::Repo,
            ),
            skill(
                "review",
                "/home/me/.codex/skills/review/SKILL.md",
                SkillScope::User,
            ),
            skill(
                "summarize",
                "/repo/.codex/skills/summarize/SKILL.md",
                SkillScope::Repo,
            ),
        ];

        assert_eq!(
            disambiguated_skill_rows(&skills),
            vec![
                "review (repository)".to_string(),
                "review (home)".to_string(),
                // A name that is already unique is left exactly as it was.
                "summarize".to_string(),
            ]
        );
    }

    /// Two packages of the same name from the same place are the case where
    /// the origin word runs out, and the path is the only thing left that
    /// separates them.
    #[test]
    fn two_rows_from_one_place_fall_back_to_their_paths() {
        let skills = vec![
            skill(
                "review",
                "/repo/.codex/skills/review/SKILL.md",
                SkillScope::Repo,
            ),
            skill(
                "review",
                "/repo/nested/.codex/skills/review/SKILL.md",
                SkillScope::Repo,
            ),
        ];

        assert_eq!(
            disambiguated_skill_rows(&skills),
            vec![
                "review (/repo/.codex/skills/review/SKILL.md)".to_string(),
                "review (/repo/nested/.codex/skills/review/SKILL.md)".to_string(),
            ]
        );
    }

    /// A duplicate name nobody mentioned is not this turn's problem. Saying so
    /// on every send would train the person to ignore the notice that matters.
    #[test]
    fn an_unmentioned_duplicate_says_nothing() {
        let skills = vec![
            skill(
                "review",
                "/repo/.codex/skills/review/SKILL.md",
                SkillScope::Repo,
            ),
            skill(
                "review",
                "/home/me/.codex/skills/review/SKILL.md",
                SkillScope::User,
            ),
            skill(
                "summarize",
                "/repo/.codex/skills/summarize/SKILL.md",
                SkillScope::Repo,
            ),
        ];
        let mentions = collect_tool_mentions("$summarize please", &HashMap::new());

        let resolution = find_skill_mentions_with_tool_mentions(&mentions, &skills);

        assert_eq!(resolution.selected.len(), 1);
        assert!(resolution.shadowed.is_empty());
        assert!(duplicate_skill_mention_notices(&resolution).is_empty());
    }
}

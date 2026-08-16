use std::collections::HashMap;
use std::collections::HashSet;

use whisply_protocol::user_input::UserInput;
use whisply_utils_absolute_path::AbsolutePathBuf;

use whisply_protocol::protocol::SkillScope;

use crate::SkillMetadata;
use crate::ToolMentionKind;
use crate::ToolMentions;
use crate::build_skill_name_counts;
use crate::extract_tool_mentions;
use crate::normalize_skill_path;
use crate::tool_kind_for_path;

/// Supplies ordered skills, disabled identities, and discovery paths for explicit selection.
///
/// Implementations should preserve skill discovery order and expose the logical discovery path
/// associated with each canonical skill identity when one is available.
pub trait ExplicitSkillLookup {
    fn skills(&self) -> &[SkillMetadata];

    fn disabled_paths(&self) -> &HashSet<AbsolutePathBuf>;

    fn skill_discovery_path_for_path(&self, path: &AbsolutePathBuf) -> Option<&AbsolutePathBuf>;

    fn is_skill_enabled(&self, skill: &SkillMetadata) -> bool {
        !self.disabled_paths().contains(&skill.path_to_skills_md)
    }
}

/// Why a `$name` in the message ran nothing.
///
/// A plain name is only followed when it can only mean one thing. When it
/// cannot, the mention is dropped, and dropping it quietly is the problem: the
/// person asked for a skill, the turn answers without it, and nothing in the
/// answer says so.
#[derive(Clone, Debug, PartialEq)]
pub enum UnusedSkillMentionReason {
    /// More than one installed package answers to that name.
    SeveralPackages { candidates: Vec<SkillMetadata> },
    /// A connector answers to the same name, so the mention could mean either.
    ConnectorSharesName,
}

/// A `$name` the message asked for and the turn did not run.
#[derive(Clone, Debug, PartialEq)]
pub struct UnusedSkillMention {
    pub name: String,
    pub reason: UnusedSkillMentionReason,
}

impl UnusedSkillMention {
    /// What to tell the person, in the terms they can act on: what they asked
    /// for, that it did not run, and the one thing that would make it run.
    pub fn describe(&self) -> String {
        match &self.reason {
            UnusedSkillMentionReason::SeveralPackages { candidates } => {
                let origins = candidates
                    .iter()
                    .map(skill_origin)
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(
                    "{count} skills are named \"{name}\", so `${name}` did not run any of them: \
                     {origins}. Pick one from the skill list to run it.",
                    count = candidates.len(),
                    name = self.name,
                )
            }
            UnusedSkillMentionReason::ConnectorSharesName => format!(
                "A skill and a connector are both named \"{name}\", so `${name}` did not run \
                 either. Pick the skill from the skill list to run it.",
                name = self.name,
            ),
        }
    }
}

/// Where a package came from, in the same terms the skill list uses, because a
/// bare path does not say which of two same-named packages is the repository's.
fn skill_origin(skill: &SkillMetadata) -> String {
    let scope = match skill.scope {
        SkillScope::Repo => "repository",
        SkillScope::User => "home",
        SkillScope::System => "system",
        SkillScope::Admin => "administrator",
    };
    format!(
        "the {scope} one ({path})",
        path = skill.path_to_skills_md.to_string_lossy()
    )
}

/// The selection, plus the mentions it could not honour.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExplicitSkillMentions {
    pub selected: Vec<SkillMetadata>,
    /// Reported once per name, in the order the names were first refused.
    pub unused: Vec<UnusedSkillMention>,
}

/// Collect explicitly mentioned skills from structured and text mentions.
///
/// Structured `UserInput::Skill` selections are resolved first by path against
/// enabled skills. Text inputs are then scanned to extract `$skill-name` tokens, and we
/// iterate loaded skills in their existing order to preserve prior ordering semantics.
/// Explicit paths match either a skill's canonical identity or its logical discovery
/// path, and plain names are only used when the match is unambiguous.
///
/// Complexity: `O(T + (N_s + N_t) * S)` time, `O(S + M)` space, where:
/// `S` = number of skills, `T` = total text length, `N_s` = number of structured skill inputs,
/// `N_t` = number of text inputs, `M` = max mentions parsed from a single text input.
pub fn collect_explicit_skill_mentions(
    inputs: &[UserInput],
    loaded_skills: &impl ExplicitSkillLookup,
    connector_slug_counts: &HashMap<String, usize>,
) -> Vec<SkillMetadata> {
    collect_explicit_skill_mentions_reporting_unused(inputs, loaded_skills, connector_slug_counts)
        .selected
}

/// As [`collect_explicit_skill_mentions`], and also says which plain names it
/// declined to act on.
///
/// Refusing an ambiguous name is deliberate: `$review` that names two installed
/// packages is not a choice, and guessing one runs code the person did not pick.
/// Reporting the refusal is what makes it a decision rather than a silent
/// omission, and it is reported here, once, so that the terminal, the overlay,
/// and a non-interactive run all give the same account of the same message.
pub fn collect_explicit_skill_mentions_reporting_unused(
    inputs: &[UserInput],
    loaded_skills: &impl ExplicitSkillLookup,
    connector_slug_counts: &HashMap<String, usize>,
) -> ExplicitSkillMentions {
    let skill_name_counts =
        build_skill_name_counts(loaded_skills.skills(), loaded_skills.disabled_paths()).0;

    let selection_context = SkillSelectionContext {
        loaded_skills,
        skill_name_counts: &skill_name_counts,
        connector_slug_counts,
    };
    let mut selected: Vec<SkillMetadata> = Vec::new();
    let mut seen_names: HashSet<String> = HashSet::new();
    let mut seen_paths: HashSet<AbsolutePathBuf> = HashSet::new();
    let mut blocked_plain_names: HashSet<String> = HashSet::new();
    let mut unused: Vec<UnusedSkillMention> = Vec::new();

    for input in inputs {
        if let UserInput::Skill { name, path, .. } = input {
            blocked_plain_names.insert(name.clone());
            let Ok(path) = AbsolutePathBuf::relative_to_current_dir(path) else {
                continue;
            };

            let Some(skill) = selection_context
                .loaded_skills
                .skills()
                .iter()
                .find(|skill| {
                    skill.path_to_skills_md == path
                        || selection_context
                            .loaded_skills
                            .skill_discovery_path_for_path(&skill.path_to_skills_md)
                            .is_some_and(|discovery_path| discovery_path == &path)
                })
            else {
                continue;
            };

            if !selection_context.loaded_skills.is_skill_enabled(skill)
                || seen_paths.contains(&skill.path_to_skills_md)
            {
                continue;
            }

            seen_paths.insert(skill.path_to_skills_md.clone());
            seen_names.insert(skill.name.clone());
            selected.push(skill.clone());
        }
    }

    for input in inputs {
        if let UserInput::Text { text, .. } = input {
            let mentioned_names = extract_tool_mentions(text);
            select_skills_from_mentions(
                &selection_context,
                &blocked_plain_names,
                &mentioned_names,
                &mut seen_names,
                &mut seen_paths,
                &mut selected,
                &mut unused,
            );
        }
    }

    // A name that something else in the same message already resolved was not
    // refused; the mention got what it asked for by a more specific route.
    let selected_names = selected
        .iter()
        .map(|skill| skill.name.clone())
        .collect::<HashSet<_>>();
    unused.retain(|mention| !selected_names.contains(&mention.name));

    ExplicitSkillMentions { selected, unused }
}

struct SkillSelectionContext<'a> {
    loaded_skills: &'a dyn ExplicitSkillLookup,
    skill_name_counts: &'a HashMap<String, usize>,
    connector_slug_counts: &'a HashMap<String, usize>,
}

/// Select mentioned skills while preserving the order of `skills`.
fn select_skills_from_mentions(
    selection_context: &SkillSelectionContext<'_>,
    blocked_plain_names: &HashSet<String>,
    mentions: &ToolMentions<'_>,
    seen_names: &mut HashSet<String>,
    seen_paths: &mut HashSet<AbsolutePathBuf>,
    selected: &mut Vec<SkillMetadata>,
    unused: &mut Vec<UnusedSkillMention>,
) {
    if mentions.is_empty() {
        return;
    }

    let mention_skill_paths: HashSet<String> = mentions
        .paths()
        .filter(|path| {
            !matches!(
                tool_kind_for_path(path),
                ToolMentionKind::App | ToolMentionKind::Mcp | ToolMentionKind::Plugin
            )
        })
        .map(normalize_host_skill_path)
        .collect();

    for skill in selection_context.loaded_skills.skills() {
        if !selection_context.loaded_skills.is_skill_enabled(skill)
            || seen_paths.contains(&skill.path_to_skills_md)
        {
            continue;
        }

        let canonical_path = normalize_host_skill_path(&skill.path_to_skills_md.to_string_lossy());
        let matches_discovery_path = selection_context
            .loaded_skills
            .skill_discovery_path_for_path(&skill.path_to_skills_md)
            .is_some_and(|discovery_path| {
                mention_skill_paths.contains(&normalize_host_skill_path(
                    &discovery_path.to_string_lossy(),
                ))
            });
        if mention_skill_paths.contains(&canonical_path) || matches_discovery_path {
            seen_paths.insert(skill.path_to_skills_md.clone());
            seen_names.insert(skill.name.clone());
            selected.push(skill.clone());
        }
    }

    for skill in selection_context.loaded_skills.skills() {
        if !selection_context.loaded_skills.is_skill_enabled(skill)
            || seen_paths.contains(&skill.path_to_skills_md)
        {
            continue;
        }

        if blocked_plain_names.contains(skill.name.as_str()) {
            continue;
        }
        if !mentions.contains_plain_name(skill.name.as_str()) {
            continue;
        }

        let skill_count = selection_context
            .skill_name_counts
            .get(skill.name.as_str())
            .copied()
            .unwrap_or(0);
        let connector_count = selection_context
            .connector_slug_counts
            .get(&skill.name.to_ascii_lowercase())
            .copied()
            .unwrap_or(0);
        if skill_count != 1 || connector_count != 0 {
            record_unused_mention(selection_context, skill, connector_count, unused);
            continue;
        }

        if seen_names.insert(skill.name.clone()) {
            seen_paths.insert(skill.path_to_skills_md.clone());
            selected.push(skill.clone());
        }
    }
}

/// Note a name the mention asked for and this pass would not act on.
///
/// The loop reaches this once per candidate package, so the name is recorded on
/// the first refusal and the rest are folded into it: a person who wrote one
/// `$review` should be told once what `$review` could have meant, not once per
/// package that could have answered.
fn record_unused_mention(
    selection_context: &SkillSelectionContext<'_>,
    skill: &SkillMetadata,
    connector_count: usize,
    unused: &mut Vec<UnusedSkillMention>,
) {
    if unused.iter().any(|mention| mention.name == skill.name) {
        return;
    }
    let reason = if connector_count != 0 {
        UnusedSkillMentionReason::ConnectorSharesName
    } else {
        UnusedSkillMentionReason::SeveralPackages {
            candidates: selection_context
                .loaded_skills
                .skills()
                .iter()
                .filter(|candidate| {
                    candidate.name == skill.name
                        && selection_context.loaded_skills.is_skill_enabled(candidate)
                })
                .cloned()
                .collect(),
        }
    };
    unused.push(UnusedSkillMention {
        name: skill.name.clone(),
        reason,
    });
}

fn normalize_host_skill_path(path: &str) -> String {
    normalize_skill_path(path).replace('\\', "/")
}

#[cfg(test)]
#[path = "selection_tests.rs"]
mod tests;

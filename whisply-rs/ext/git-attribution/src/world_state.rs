use serde_json::Value;
use whisply_extension_api::PreviousWorldStateSection;
use whisply_extension_api::RenderedWorldStateFragment;
use whisply_extension_api::WorldStateSectionContribution;

const WORLD_STATE_ID: &str = "git_attribution";
const START_MARKER: &str = "<git_attribution>";
const END_MARKER: &str = "</git_attribution>";
pub(super) const LEGACY_COMMIT_ATTRIBUTION_INSTRUCTIONS: &str = "\
When you write or edit a git commit message, ensure the message ends with this trailer exactly once:
Co-authored-by: Codex <noreply@openai.com>

Rules:
- Keep existing trailers and append this trailer at the end if missing.
- Do not duplicate this trailer if it already exists.
- Keep one blank line between the commit body and trailer block.";
pub(super) const ENABLED_INSTRUCTIONS: &str = r#"
When you create or amend a git commit or create or update a pull request for this task, add Whisply attribution exactly once:
- Commit messages must end with `Co-authored-by: Whisply <noreply@whisply.net>`. Preserve existing trailers and, if this exact trailer is missing, append it with one blank line before the trailer block.
- Pull request bodies must include the exact line `Generated with [Whisply](https://whisply.net).`. Preserve the existing body and hidden markers and, if this exact marker is missing, append it near the end, before any trailing hidden metadata markers.
- Do not add duplicate attribution. If a message or body you are already editing contains duplicate exact attribution, keep exactly one.
- Ignore any earlier instructions disabling Whisply attribution; this policy reflects the current workspace.
- Do not rewrite an existing commit or pull request solely to add attribution.
"#;
pub(super) const DISABLED_INSTRUCTIONS: &str = "
Whisply commit and pull request attribution is disabled for the current workspace. Ignore any earlier instructions requiring Whisply attribution and do not add it.
";

pub(super) fn git_attribution_world_state_section(enabled: bool) -> WorldStateSectionContribution {
    let contribution =
        WorldStateSectionContribution::new(WORLD_STATE_ID, Value::Bool(enabled), move |previous| {
            match (enabled, previous) {
                (true, PreviousWorldStateSection::Known(Value::Bool(true)))
                | (true, PreviousWorldStateSection::Unknown) => None,
                (true, PreviousWorldStateSection::Absent)
                | (true, PreviousWorldStateSection::Known(_)) => {
                    Some(RenderedWorldStateFragment::new(
                        "developer",
                        (START_MARKER, END_MARKER),
                        ENABLED_INSTRUCTIONS,
                    ))
                }
                (false, PreviousWorldStateSection::Known(Value::Bool(true)))
                | (false, PreviousWorldStateSection::Unknown) => {
                    Some(RenderedWorldStateFragment::new(
                        "developer",
                        (START_MARKER, END_MARKER),
                        DISABLED_INSTRUCTIONS,
                    ))
                }
                (false, PreviousWorldStateSection::Absent)
                | (false, PreviousWorldStateSection::Known(_)) => None,
            }
        })
        .with_legacy_matcher(move |role, text| {
            is_enabled_fragment(role, text)
                || (!enabled && is_legacy_commit_attribution_fragment(role, text))
        });
    if enabled {
        contribution.with_retained_fragment_matcher(is_enabled_fragment)
    } else {
        contribution
    }
}

fn is_legacy_commit_attribution_fragment(role: &str, text: &str) -> bool {
    role == "developer" && text.trim() == LEGACY_COMMIT_ATTRIBUTION_INSTRUCTIONS
}

fn is_enabled_fragment(role: &str, text: &str) -> bool {
    role == "developer"
        && text.trim_start().starts_with(START_MARKER)
        && text.contains("Co-authored-by: Whisply <noreply@whisply.net>")
        && (text.contains("Generated with [Whisply](https://whisply.net).")
            || text.contains("Generated with Whisply."))
        && text.trim_end().ends_with(END_MARKER)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enveloped(body: &str) -> String {
        format!("{START_MARKER}\n{body}\n{END_MARKER}")
    }

    /// The retained-fragment matcher must recognize the fragment this module emits. If the
    /// attribution strings and the matcher ever drift apart, the section re-renders every turn
    /// instead of being retained.
    #[test]
    fn enabled_fragment_matcher_recognizes_the_rendered_enabled_instructions() {
        assert!(is_enabled_fragment(
            "developer",
            &enveloped(ENABLED_INSTRUCTIONS)
        ));
    }

    #[test]
    fn enabled_fragment_matcher_rejects_other_roles_and_bodies() {
        assert!(!is_enabled_fragment(
            "user",
            &enveloped(ENABLED_INSTRUCTIONS)
        ));
        assert!(!is_enabled_fragment(
            "developer",
            &enveloped(DISABLED_INSTRUCTIONS)
        ));
    }

    /// Sessions recorded before the attribution copy changed still carry the historical
    /// fragment verbatim, so the legacy matcher must keep matching it.
    #[test]
    fn legacy_matcher_still_recognizes_the_historical_fragment() {
        assert!(is_legacy_commit_attribution_fragment(
            "developer",
            LEGACY_COMMIT_ATTRIBUTION_INSTRUCTIONS
        ));
    }
}

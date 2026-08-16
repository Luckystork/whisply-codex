//! Naming Whisply's three public permission modes in the terminal.
//!
//! The terminal stores a permission mode as an approval policy plus a reviewer
//! rather than as one value, so every surface that wants to name the current
//! mode has to derive it. Deriving it in each place is how the terminal came to
//! call the same mode `Approve for me` in one view and `Auto` in another, and
//! how it drifted from what the app calls it. This is the one derivation.

use codex_app_server_protocol::AskForApproval;
use whisply_config::types::ApprovalsReviewer;

pub(crate) use codex_whisply::PermissionMode;

/// The public mode the terminal is currently in, if it is in one of them.
///
/// Returns `None` for a read-only or custom permission profile: those are not
/// among the three public modes and naming them as one would misreport what
/// the person selected.
pub(crate) fn active_permission_mode(
    approval_policy: AskForApproval,
    approvals_reviewer: ApprovalsReviewer,
) -> Option<PermissionMode> {
    match (approval_policy, approvals_reviewer) {
        (AskForApproval::OnRequest, ApprovalsReviewer::AutoReview) => Some(PermissionMode::Auto),
        (AskForApproval::OnRequest, ApprovalsReviewer::User) => Some(PermissionMode::Ask),
        (AskForApproval::Never, _) => Some(PermissionMode::FullAccess),
        (AskForApproval::UnlessTrusted | AskForApproval::Granular { .. }, _) => None,
    }
}

/// The mode a new terminal thread starts in.
pub(crate) const NEW_THREAD_MODE: PermissionMode = PermissionMode::Auto;

/// The reviewer the terminal asks for when nothing else has decided.
///
/// This is a default, not an override: a person who has configured a reviewer,
/// or passed one on the command line, keeps it. It is supplied by the terminal
/// rather than built into configuration loading because a non-interactive
/// surface has no one to hand an `ask` back to, and so must not start in a mode
/// whose whole purpose is to sometimes ask.
pub(crate) const fn new_thread_default_reviewer() -> Option<ApprovalsReviewer> {
    Some(match NEW_THREAD_MODE {
        PermissionMode::Auto => ApprovalsReviewer::AutoReview,
        PermissionMode::Ask | PermissionMode::FullAccess => ApprovalsReviewer::User,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    /// WCD-714: the app and the terminal offer the same three modes, so they
    /// have to call them the same three things.
    #[test]
    fn the_three_modes_are_named_the_way_the_app_names_them() {
        assert_eq!(PermissionMode::Ask.display_name(), "Ask for approval");
        assert_eq!(PermissionMode::Auto.display_name(), "Auto");
        assert_eq!(PermissionMode::FullAccess.display_name(), "Full access");
    }

    #[test]
    fn who_answers_an_approval_is_what_separates_ask_from_auto() {
        assert_eq!(
            active_permission_mode(AskForApproval::OnRequest, ApprovalsReviewer::User),
            Some(PermissionMode::Ask)
        );
        assert_eq!(
            active_permission_mode(AskForApproval::OnRequest, ApprovalsReviewer::AutoReview),
            Some(PermissionMode::Auto)
        );
    }

    #[test]
    fn not_asking_at_all_is_full_access_whoever_the_reviewer_is() {
        for reviewer in [ApprovalsReviewer::User, ApprovalsReviewer::AutoReview] {
            assert_eq!(
                active_permission_mode(AskForApproval::Never, reviewer),
                Some(PermissionMode::FullAccess)
            );
        }
    }

    /// A read-only or custom profile is not one of the three, and reporting it
    /// as one would tell the person they had chosen something they had not.
    #[test]
    fn a_policy_outside_the_three_modes_is_not_named_as_one_of_them() {
        assert_eq!(
            active_permission_mode(AskForApproval::UnlessTrusted, ApprovalsReviewer::User),
            None
        );
    }

    /// WCD-714: a new thread starts in `Auto`, and the reviewer the terminal
    /// asks for is the one that actually produces `Auto` rather than a value
    /// that merely looks like it should.
    #[test]
    fn the_default_the_terminal_asks_for_is_the_mode_a_new_thread_starts_in() {
        assert_eq!(NEW_THREAD_MODE, PermissionMode::Auto);
        let reviewer = new_thread_default_reviewer().expect("the terminal supplies a default");
        assert_eq!(
            active_permission_mode(AskForApproval::OnRequest, reviewer),
            Some(NEW_THREAD_MODE)
        );
    }

    /// The default only counts if the terminal hands it to configuration
    /// loading. Dropping the field is silent — every mode still works, new
    /// threads just quietly start in `Ask` — so the wiring is asserted here.
    #[test]
    fn the_terminal_hands_that_default_to_configuration_loading() {
        let entry_point = include_str!("lib.rs");
        assert!(
            entry_point.contains(
                "default_approvals_reviewer: permission_mode::new_thread_default_reviewer(),"
            ),
            "the terminal no longer supplies its new-thread permission default"
        );
    }
}

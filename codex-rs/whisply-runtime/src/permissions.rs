//! Whisply's public Ask/Auto/Full access permission model.

use serde::Deserialize;
use serde::Serialize;

/// The only public local-approval modes exposed by Whisply.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    /// Surface every approval that the effective local policy requires.
    Ask,
    /// Use one fresh, stateless review for each eligible local approval.
    #[default]
    Auto,
    /// Remove routine local sandbox confirmations only.
    FullAccess,
}

impl PermissionMode {
    /// The precise public copy used by terminal and native selectors.
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Ask => "Ask",
            Self::Auto => "Auto",
            Self::FullAccess => "Full access",
        }
    }
}

/// The bounded, tool-free input packet supplied to one automatic reviewer.
///
/// This intentionally has no conversation transcript, model output, secret,
/// trusted execution envelope, or mutable authority field.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalPacket {
    pub proposed_action: String,
    pub canonical_cwd: String,
    pub targets: Vec<String>,
    pub sandbox_facts: Vec<String>,
    pub policy_facts: Vec<String>,
    pub safe_reason: String,
    pub policy_revision: String,
}

impl ApprovalPacket {
    /// Checks that a packet remains bounded before it can reach a reviewer.
    pub fn validate(&self) -> Result<(), AutoReviewReason> {
        validate_text(&self.proposed_action, 4_096)?;
        validate_text(&self.canonical_cwd, 4_096)?;
        validate_text(&self.safe_reason, 1_024)?;
        validate_text(&self.policy_revision, 256)?;
        validate_values(&self.targets, 32, 1_024)?;
        validate_values(&self.sandbox_facts, 32, 1_024)?;
        validate_values(&self.policy_facts, 32, 1_024)?;
        Ok(())
    }
}

/// A machine-readable result of one fresh automatic review invocation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoReviewDecision {
    Allow { reason_code: String },
    AskUser { reason: AutoReviewReason },
    Deny { reason_code: String },
}

/// A safe reason for a non-automatic outcome.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoReviewReason {
    InvalidPacket,
    ReviewerUnavailable,
    ReviewerTimedOut,
    ReviewerMalformed,
    ReviewerUncertain,
    UnsupportedAction,
}

/// Runs one isolated reviewer invocation and fails closed to `AskUser`.
///
/// The `FnOnce` requirement prevents this API from reusing an invocation or
/// carrying history between approvals. The gateway implementation later owns
/// the actual stateless Luna request and usage receipt.
pub fn resolve_auto_review<F>(
    packet: ApprovalPacket,
    invoke_fresh_reviewer: F,
) -> AutoReviewDecision
where
    F: FnOnce(ApprovalPacket) -> Result<AutoReviewDecision, AutoReviewReason>,
{
    if let Err(reason) = packet.validate() {
        return AutoReviewDecision::AskUser { reason };
    }
    match invoke_fresh_reviewer(packet) {
        Ok(AutoReviewDecision::Allow { reason_code }) if !reason_code.trim().is_empty() => {
            AutoReviewDecision::Allow { reason_code }
        }
        Ok(AutoReviewDecision::Deny { reason_code }) if !reason_code.trim().is_empty() => {
            AutoReviewDecision::Deny { reason_code }
        }
        Ok(AutoReviewDecision::AskUser { reason }) => AutoReviewDecision::AskUser { reason },
        Ok(_) | Err(AutoReviewReason::InvalidPacket) => AutoReviewDecision::AskUser {
            reason: AutoReviewReason::ReviewerMalformed,
        },
        Err(reason) => AutoReviewDecision::AskUser { reason },
    }
}

fn validate_values(
    values: &[String],
    max_count: usize,
    max_bytes: usize,
) -> Result<(), AutoReviewReason> {
    if values.len() > max_count {
        return Err(AutoReviewReason::InvalidPacket);
    }
    values
        .iter()
        .try_for_each(|value| validate_text(value, max_bytes))
}

fn validate_text(value: &str, max_bytes: usize) -> Result<(), AutoReviewReason> {
    if value.trim().is_empty() || value.len() > max_bytes {
        Err(AutoReviewReason::InvalidPacket)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet() -> ApprovalPacket {
        ApprovalPacket {
            proposed_action: "write one file inside the selected workspace".to_string(),
            canonical_cwd: "/workspace".to_string(),
            targets: vec!["/workspace/notes.md".to_string()],
            sandbox_facts: vec!["workspace-write".to_string()],
            policy_facts: vec!["local approval delegated".to_string()],
            safe_reason: "user requested the update".to_string(),
            policy_revision: "policy-1".to_string(),
        }
    }

    #[test]
    fn auto_is_the_default_public_permission_mode() {
        assert_eq!(PermissionMode::default(), PermissionMode::Auto);
        assert_eq!(PermissionMode::FullAccess.display_name(), "Full access");
    }

    #[test]
    fn malformed_reviewer_output_falls_back_to_user_approval() {
        let decision = resolve_auto_review(packet(), |_| {
            Ok(AutoReviewDecision::Allow {
                reason_code: String::new(),
            })
        });

        assert_eq!(
            decision,
            AutoReviewDecision::AskUser {
                reason: AutoReviewReason::ReviewerMalformed,
            }
        );
    }

    #[test]
    fn reviewer_failure_never_becomes_an_automatic_allow() {
        let decision = resolve_auto_review(packet(), |_| Err(AutoReviewReason::ReviewerTimedOut));

        assert_eq!(
            decision,
            AutoReviewDecision::AskUser {
                reason: AutoReviewReason::ReviewerTimedOut,
            }
        );
    }
}

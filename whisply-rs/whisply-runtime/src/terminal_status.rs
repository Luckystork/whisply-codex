//! Stable machine-readable terminal statuses and process exit codes.
//!
//! Automation driving `whisply exec` could previously only tell success from
//! failure: every startup and runtime failure exited `1`, so a script could not
//! distinguish "your config is malformed" from "sign in first" from "this
//! action needs a confirmation channel you did not provide". This module is the
//! single definition of the terminal outcomes and the integers they map to.
//!
//! Two compatibility rules are deliberate:
//!
//! * `Failed` keeps exit code `1`. Existing automation, and the checked-in exec
//!   suites, treat a generic terminal failure as `1`; reassigning it would break
//!   callers for no benefit. `1` now means "generic terminal failure" by
//!   definition rather than by accident.
//! * Assigned codes stay below `126` so they never collide with the shell's
//!   reserved range (`126` not executable, `127` not found, `128 + signal`).

use serde::Deserialize;
use serde::Serialize;

/// A terminal outcome a Whisply terminal surface can report.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WhisplyTerminalStatus {
    /// The requested work completed.
    Succeeded,
    /// A generic terminal failure with no more specific classification.
    Failed,
    /// Supplied configuration, overrides, or policy rules could not be used.
    InvalidConfiguration,
    /// The local runtime environment could not be resolved or prepared.
    EnvironmentUnavailable,
    /// The account is not signed in, or its session is no longer usable.
    AuthenticationRequired,
    /// The account is signed in but its plan or entitlement denies the work.
    EntitlementDenied,
    /// Funded admission was refused because Usage or funding is exhausted.
    UsageExhausted,
    /// The action requires a human confirmation and no confirmation channel
    /// is available, so it fails closed rather than proceeding unattended.
    ConfirmationUnavailable,
    /// A required precondition was not met before any work started.
    PreconditionUnmet,
    /// The work was stopped by the user or an explicit cancellation.
    Cancelled,
}

impl WhisplyTerminalStatus {
    /// Every status, so a projection cannot silently omit one.
    pub const ALL: [Self; 10] = [
        Self::Succeeded,
        Self::Failed,
        Self::InvalidConfiguration,
        Self::EnvironmentUnavailable,
        Self::AuthenticationRequired,
        Self::EntitlementDenied,
        Self::UsageExhausted,
        Self::ConfirmationUnavailable,
        Self::PreconditionUnmet,
        Self::Cancelled,
    ];

    /// The stable wire identifier used by JSONL output and diagnostics.
    pub const fn status_id(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::InvalidConfiguration => "invalid_configuration",
            Self::EnvironmentUnavailable => "environment_unavailable",
            Self::AuthenticationRequired => "authentication_required",
            Self::EntitlementDenied => "entitlement_denied",
            Self::UsageExhausted => "usage_exhausted",
            Self::ConfirmationUnavailable => "confirmation_unavailable",
            Self::PreconditionUnmet => "precondition_unmet",
            Self::Cancelled => "cancelled",
        }
    }

    /// The stable process exit code for this outcome.
    pub const fn exit_code(self) -> i32 {
        match self {
            Self::Succeeded => 0,
            Self::Failed => 1,
            Self::InvalidConfiguration => 10,
            Self::EnvironmentUnavailable => 11,
            Self::AuthenticationRequired => 12,
            Self::EntitlementDenied => 13,
            Self::UsageExhausted => 14,
            Self::ConfirmationUnavailable => 15,
            Self::PreconditionUnmet => 16,
            Self::Cancelled => 17,
        }
    }

    /// Whether the outcome represents completed work.
    pub const fn is_success(self) -> bool {
        matches!(self, Self::Succeeded)
    }
}

#[cfg(test)]
#[path = "terminal_status_tests.rs"]
mod tests;

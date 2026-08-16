//! Reports the managed Whisply Runtime update contract.
//!
//! The runtime must never probe upstream package registries, release feeds, or
//! installers. The owning Whisply app replaces the whole Runtime tree
//! atomically, including the CLI, app-server, and catalog-key resource.

use whisply_core::config::Config;

use super::CheckStatus;
use super::DoctorCheck;

pub(super) fn updates_check(_config: &Config) -> DoctorCheck {
    DoctorCheck::new(
        "updates.status",
        "updates",
        CheckStatus::Ok,
        "updates are managed by the installed Whisply app",
    )
    .details(vec![
        "runtime update source: managed Whisply app bundle".to_string(),
        "network update probes: disabled in the runtime".to_string(),
        "replacement scope: atomic Runtime support tree".to_string(),
    ])
}

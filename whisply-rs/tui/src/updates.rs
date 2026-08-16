#![cfg(not(debug_assertions))]

//! Managed-runtime update policy.
//!
//! Whisply packages the CLI/TUI/app-server as one verified Runtime support
//! tree. Checking upstream package registries or release feeds would advertise
//! an incompatible update path and could split the binary from its signed
//! catalog key resource, so runtime update discovery is intentionally disabled.

use crate::legacy_core::config::Config;

pub(crate) use crate::updates_cache::dismiss_version;

/// Runtime updates are announced and installed only by the owning Whisply app.
pub fn get_upgrade_version(_config: &Config) -> Option<String> {
    None
}

/// Runtime updates are announced and installed only by the owning Whisply app.
pub fn get_upgrade_version_for_popup(_config: &Config) -> Option<String> {
    None
}

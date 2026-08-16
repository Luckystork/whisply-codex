//! Which MCP servers a session starts, and how many at once.
//!
//! Every enabled stdio server is a child process this machine has to carry, and
//! the set is not curated: it is whatever the config, the installed plugins,
//! and any local marketplace add up to. Starting all of them, all at the same
//! moment, is fine at five and is a login-time stampede at two hundred. Neither
//! number was bounded, so both are bounded here: a ceiling on how many servers
//! a session starts at all, and a ceiling on how many are starting at any one
//! moment.
//!
//! A correctly configured custom MCP server keeps working. The point of the
//! bound is the pathological end -- and when it does bite, the servers it left
//! out are named rather than silently missing.

use std::collections::HashMap;

use crate::server::EffectiveMcpServer;

/// How many MCP servers one session will start.
///
/// Far above any hand-written config; the configurations that reach it are
/// generated or accumulated.
pub(crate) const MAX_STARTED_MCP_SERVERS: usize = 64;

/// How many may be starting at the same moment.
///
/// Startup is where a stdio server costs the most -- a process spawn, an
/// interpreter, and whatever it does before it answers `initialize` -- and it
/// is the moment every server does it together. Staggering does not slow a
/// normal configuration down, because a normal configuration does not have
/// more than this many servers.
pub(crate) const MAX_CONCURRENT_MCP_STARTUPS: usize = 8;

pub(crate) struct StartupAdmission {
    /// In the order they will be started.
    pub(crate) started: Vec<(String, EffectiveMcpServer)>,
    /// Names left out by the ceiling, sorted.
    pub(crate) refused: Vec<String>,
}

/// Sorts the enabled servers into the ones this session starts and the ones it
/// does not.
///
/// Required servers come first: a session that is meant to fail without one
/// should fail because that server did not start, not because an alphabetically
/// luckier optional server took its place. The rest are taken in name order --
/// the servers arrive in a `HashMap`, and without a deterministic order the
/// same configuration would start a different subset on each launch.
pub(crate) fn admit_servers(
    mcp_servers: HashMap<String, EffectiveMcpServer>,
    limit: usize,
) -> StartupAdmission {
    let mut enabled = mcp_servers
        .into_iter()
        .filter(|(_, server)| server.enabled())
        .collect::<Vec<_>>();
    enabled.sort_by(|(left_name, left), (right_name, right)| {
        right
            .required()
            .cmp(&left.required())
            .then_with(|| left_name.cmp(right_name))
    });

    let refused_names = enabled
        .split_off(limit.min(enabled.len()))
        .into_iter()
        .map(|(server_name, _)| server_name)
        .collect::<Vec<_>>();
    let mut refused = refused_names;
    refused.sort();

    StartupAdmission {
        started: enabled,
        refused,
    }
}

/// What is said about a server the ceiling left out.
pub(crate) fn refused_startup_message(enabled: usize, limit: usize) -> String {
    format!(
        "not started: {enabled} MCP servers are enabled and this session starts at most \
         {limit}. Disable the servers you do not need, or remove the plugins that added them, \
         and the rest will start."
    )
}

#[cfg(test)]
#[path = "startup_admission_tests.rs"]
mod tests;

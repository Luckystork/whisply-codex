//! What a child process Whisply starts is allowed to inherit.
//!
//! Whisply runs code it did not write: MCP servers a person configured, the
//! `git` and `npm` that fetch and update plugins, hook commands. None of that
//! is hostile by assumption, but none of it is the product either, and a child
//! inherits its parent's environment wholesale unless something says otherwise.
//!
//! Two levels, because the answer differs by who owns the child.
//!
//! The authority handles say where this runtime's own credentials can be
//! reached: the descriptor numbers carrying the gateway token and endpoint, the
//! native broker's capability descriptor, and the broker socket. Those are
//! withheld from every child. Holding a descriptor number is useless once the
//! descriptor is close-on-exec, which it is by the time any child starts, so
//! passing the names on communicates nothing and can actively mislead -- a
//! nested Whisply that believes it inherited a launcher contract will read the
//! numbers, find them closed or reused, and fail in a way that has nothing to
//! do with what the person asked for.
//!
//! The whole `WHISPLY_` namespace, which also includes the account's storage
//! root, is withheld from third-party code specifically: a plugin's install
//! helper has no business being told where this account keeps its data. A hook
//! is deliberately not treated that way. Hooks are commands the person wrote,
//! and one that shells out to `whisply` should reach the same account they were
//! configured in rather than silently a different one.

use std::ffi::OsStr;
use std::ffi::OsString;

use crate::brand::GATEWAY_AUTH_FD_ENV;
use crate::brand::GATEWAY_ENDPOINT_FD_ENV;
use crate::brand::HOME_ENV;
use crate::brand::NATIVE_BROKER_CAPABILITY_FD_ENV;
use crate::brand::NATIVE_BROKER_SOCKET_ENV;
use crate::brand::RUNTIME_BROKER_CONTROL_ONLY_ENV;

/// The environment namespace that carries Whisply's own handles.
///
/// A namespace rather than a list of known-sensitive names, because a list is
/// a promise to remember every future variable. Nothing outside the product
/// has a reason to read one, so refusing the whole namespace costs a
/// legitimate program nothing.
pub const RESERVED_ENV_PREFIX: &str = "WHISPLY_";

/// The handles that say where this runtime's authority can be reached.
///
/// Withheld by name whether or not the parent has them set, so a child's
/// environment is the same shape in a test as it is on a person's machine.
pub const RUNTIME_AUTHORITY_ENV: [&str; 5] = [
    GATEWAY_AUTH_FD_ENV,
    GATEWAY_ENDPOINT_FD_ENV,
    NATIVE_BROKER_CAPABILITY_FD_ENV,
    NATIVE_BROKER_SOCKET_ENV,
    RUNTIME_BROKER_CONTROL_ONLY_ENV,
];

/// Whether `name` belongs to Whisply rather than to the person's environment.
pub fn env_var_is_reserved(name: &str) -> bool {
    name.starts_with(RESERVED_ENV_PREFIX)
}

/// Withholds the authority handles from a child process.
///
/// `remove` is called with each name to drop; both `std::process::Command` and
/// its Tokio counterpart take `env_remove`, and neither is named here so this
/// stays usable from crates that have only one of them.
pub fn withhold_runtime_authority<F>(mut remove: F)
where
    F: FnMut(&OsStr),
{
    for name in RUNTIME_AUTHORITY_ENV {
        remove(OsStr::new(name));
    }
}

/// Withholds everything of Whisply's from a child process it does not own.
///
/// For code the product runs on someone else's behalf: a plugin's `git` or
/// `npm`, an installer, any future helper of that kind. The authority handles
/// plus the account's storage root plus whatever else the namespace grows.
pub fn withhold_reserved_environment<F>(mut remove: F)
where
    F: FnMut(&OsStr),
{
    withhold_runtime_authority(&mut remove);
    remove(OsStr::new(HOME_ENV));
    for name in reserved_names_in(std::env::vars_os().map(|(name, _)| name)) {
        remove(&name);
    }
}

/// The reserved names actually present in `names`.
///
/// Separated from the process environment so the rule can be tested without a
/// test having to mutate the environment every other test is reading.
pub fn reserved_names_in<I>(names: I) -> Vec<OsString>
where
    I: IntoIterator<Item = OsString>,
{
    names
        .into_iter()
        .filter(|name| {
            name.to_str()
                .is_some_and(|name| name.starts_with(RESERVED_ENV_PREFIX))
        })
        .collect()
}

#[cfg(test)]
#[path = "child_env_tests.rs"]
mod tests;

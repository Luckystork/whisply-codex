use std::collections::HashMap;
use whisply_protocol::ThreadId;
#[cfg(test)]
use whisply_protocol::config_types::EnvironmentVariablePattern;
use whisply_protocol::config_types::ShellEnvironmentPolicy;
use whisply_protocol::models::ActivePermissionProfile;
use whisply_protocol::shell_environment;

pub use whisply_protocol::shell_environment::CODEX_THREAD_ID_ENV_VAR;

/// Informational name of the active permission profile. Child processes can
/// overwrite this value, so it must not be treated as proof of enforcement.
pub const CODEX_PERMISSION_PROFILE_ENV_VAR: &str = "CODEX_PERMISSION_PROFILE";

/// Construct an environment map based on the rules in the specified policy. The
/// resulting map can be passed directly to `Command::envs()` after calling
/// `env_clear()` to ensure no unintended variables are leaked to the spawned
/// process.
///
/// The derivation follows the algorithm documented in the struct-level comment
/// for [`ShellEnvironmentPolicy`].
///
/// `CODEX_THREAD_ID` is injected when a thread id is provided, even when
/// `include_only` is set.
///
/// Whisply's own authority handles are withheld regardless of the policy. The
/// product already withholds them from every plugin helper, hook, MCP server
/// and custom executor it starts; the one child that was still inheriting them
/// is the one running a command a model wrote, which is the child with the
/// least claim to them. They are dropped last so that no `inherit`,
/// `include_only` or `set` can put them back: they name descriptors and a
/// socket belonging to this process, and a person who writes one into their
/// config is describing something that will not be there.
pub fn create_env(
    policy: &ShellEnvironmentPolicy,
    thread_id: Option<ThreadId>,
) -> HashMap<String, String> {
    create_env_from_vars(std::env::vars(), policy, thread_id)
}

/// [`create_env`] against a supplied environment rather than this process's.
///
/// This is the whole derivation. [`create_env`] differs only in where the
/// starting variables come from, so a test of this function is a test of what
/// a child actually receives.
fn create_env_from_vars<I>(
    vars: I,
    policy: &ShellEnvironmentPolicy,
    thread_id: Option<ThreadId>,
) -> HashMap<String, String>
where
    I: IntoIterator<Item = (String, String)>,
{
    let thread_id = thread_id.map(|thread_id| thread_id.to_string());
    let mut env = shell_environment::create_env_from_vars(vars, policy, thread_id.as_deref());
    withhold_runtime_authority_from(&mut env);
    env
}

/// Drops the handles naming this runtime's own authority.
///
/// Applied after the policy rather than as part of it, so that no `inherit`,
/// `include_only` or `set` can put one back.
fn withhold_runtime_authority_from(env: &mut HashMap<String, String>) {
    codex_whisply::withhold_runtime_authority(|name| {
        let name = name.to_string_lossy().into_owned();
        if cfg!(windows) {
            env.retain(|key, _| !key.eq_ignore_ascii_case(&name));
        } else {
            env.remove(&name);
        }
    });
}

/// Injects the selected named permission profile into a shell tool's environment.
///
/// This is applied after the shell environment policy so the runtime-selected
/// profile wins over inherited or configured values.
pub(crate) fn inject_permission_profile_env(
    env: &mut HashMap<String, String>,
    active_permission_profile: Option<&ActivePermissionProfile>,
) {
    if cfg!(windows) {
        env.retain(|key, _| !key.eq_ignore_ascii_case(CODEX_PERMISSION_PROFILE_ENV_VAR));
    } else {
        env.remove(CODEX_PERMISSION_PROFILE_ENV_VAR);
    }
    if let Some(active_permission_profile) = active_permission_profile {
        env.insert(
            CODEX_PERMISSION_PROFILE_ENV_VAR.to_string(),
            active_permission_profile.id.clone(),
        );
    }
}

#[cfg(test)]
#[path = "exec_env_tests.rs"]
mod tests;

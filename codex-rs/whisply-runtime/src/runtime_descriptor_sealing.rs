//! Process-entry sealing for inherited Whisply runtime descriptors.
//!
//! The signed launcher intentionally maps the three one-shot descriptors to
//! fixed child descriptor numbers. The runtime keeps those descriptors open
//! for its own later reads, but must make them close-on-exec before any CLI
//! helper can launch an unrelated child process.

use std::ffi::OsString;

use thiserror::Error;

use crate::brand::GATEWAY_AUTH_FD;
use crate::brand::GATEWAY_AUTH_FD_ENV;
use crate::brand::GATEWAY_ENDPOINT_FD;
use crate::brand::GATEWAY_ENDPOINT_FD_ENV;
use crate::brand::NATIVE_BROKER_CAPABILITY_FD;
use crate::brand::NATIVE_BROKER_CAPABILITY_FD_ENV;

const INHERITED_RUNTIME_DESCRIPTORS: [(&str, i32); 3] = [
    (GATEWAY_AUTH_FD_ENV, GATEWAY_AUTH_FD),
    (GATEWAY_ENDPOINT_FD_ENV, GATEWAY_ENDPOINT_FD),
    (NATIVE_BROKER_CAPABILITY_FD_ENV, NATIVE_BROKER_CAPABILITY_FD),
];

/// Failure to validate or seal the fixed launcher descriptor contract.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum RuntimeDescriptorSealingError {
    #[error("Whisply runtime descriptors are incomplete")]
    Incomplete,
    #[error("Whisply runtime descriptors are invalid")]
    Invalid,
    #[error("Whisply runtime descriptor is unavailable")]
    Unavailable,
    #[error("Whisply runtime descriptors are unsupported on this platform")]
    UnsupportedPlatform,
}

/// Validates and seals launcher-owned descriptor targets before CLI dispatch.
///
/// No descriptor is read, duplicated, or closed here. Marking an already-open
/// descriptor `FD_CLOEXEC` preserves the current runtime's one-shot reads
/// while preventing every later `exec` child from inheriting it.
pub fn seal_inherited_runtime_descriptors() -> Result<(), RuntimeDescriptorSealingError> {
    let Some(descriptors) = inherited_runtime_descriptor_fds(|name| std::env::var_os(name))? else {
        return Ok(());
    };
    seal_runtime_descriptor_fds(descriptors)
}

fn inherited_runtime_descriptor_fds(
    mut environment: impl FnMut(&str) -> Option<OsString>,
) -> Result<Option<[i32; 3]>, RuntimeDescriptorSealingError> {
    let descriptors = INHERITED_RUNTIME_DESCRIPTORS.map(|(name, fd)| (name, fd, environment(name)));
    if descriptors.iter().all(|(_, _, value)| value.is_none()) {
        return Ok(None);
    }
    if descriptors.iter().any(|(_, _, value)| value.is_none()) {
        return Err(RuntimeDescriptorSealingError::Incomplete);
    }
    if descriptors
        .iter()
        .any(|(_, fd, value)| value.as_ref() != Some(&OsString::from(fd.to_string())))
    {
        return Err(RuntimeDescriptorSealingError::Invalid);
    }
    Ok(Some(INHERITED_RUNTIME_DESCRIPTORS.map(|(_, fd)| fd)))
}

#[cfg(unix)]
fn seal_runtime_descriptor_fds(descriptors: [i32; 3]) -> Result<(), RuntimeDescriptorSealingError> {
    let flags = descriptors.map(|fd| {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        if flags < 0 {
            Err(RuntimeDescriptorSealingError::Unavailable)
        } else {
            Ok(flags)
        }
    });
    let flags: [i32; 3] = flags
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .and_then(|flags| {
            flags
                .try_into()
                .map_err(|_| RuntimeDescriptorSealingError::Unavailable)
        })?;

    for (fd, flags) in descriptors.into_iter().zip(flags) {
        if flags & libc::FD_CLOEXEC == 0 {
            let result = unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) };
            if result < 0 {
                return Err(RuntimeDescriptorSealingError::Unavailable);
            }
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn seal_runtime_descriptor_fds(_: [i32; 3]) -> Result<(), RuntimeDescriptorSealingError> {
    Err(RuntimeDescriptorSealingError::UnsupportedPlatform)
}

#[cfg(test)]
#[path = "runtime_descriptor_sealing_tests.rs"]
mod tests;

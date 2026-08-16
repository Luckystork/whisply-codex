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
use crate::brand::RUNTIME_BROKER_CONTROL_ONLY_ENV;

const INHERITED_RUNTIME_DESCRIPTORS: [(&str, i32); 3] = [
    (GATEWAY_AUTH_FD_ENV, GATEWAY_AUTH_FD),
    (GATEWAY_ENDPOINT_FD_ENV, GATEWAY_ENDPOINT_FD),
    (NATIVE_BROKER_CAPABILITY_FD_ENV, NATIVE_BROKER_CAPABILITY_FD),
];

const BROKER_CONTROL_ONLY_VALUE: &str = "1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InheritedRuntimeDescriptorSet {
    Full([i32; 3]),
    BrokerControlOnly(i32),
}

impl InheritedRuntimeDescriptorSet {
    fn as_slice(&self) -> &[i32] {
        match self {
            Self::Full(descriptors) => descriptors,
            Self::BrokerControlOnly(descriptor) => std::slice::from_ref(descriptor),
        }
    }
}

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
    let broker_control_only =
        broker_control_only_requested(std::env::var_os(RUNTIME_BROKER_CONTROL_ONLY_ENV))?;
    let Some(descriptors) =
        inherited_runtime_descriptor_fds(broker_control_only, |name| std::env::var_os(name))?
    else {
        return Ok(());
    };
    seal_runtime_descriptor_fds(descriptors.as_slice())
}

fn broker_control_only_requested(
    value: Option<OsString>,
) -> Result<bool, RuntimeDescriptorSealingError> {
    match value {
        None => Ok(false),
        Some(value) if value == OsString::from(BROKER_CONTROL_ONLY_VALUE) => Ok(true),
        Some(_) => Err(RuntimeDescriptorSealingError::Invalid),
    }
}

fn inherited_runtime_descriptor_fds(
    broker_control_only: bool,
    mut environment: impl FnMut(&str) -> Option<OsString>,
) -> Result<Option<InheritedRuntimeDescriptorSet>, RuntimeDescriptorSealingError> {
    let descriptors = INHERITED_RUNTIME_DESCRIPTORS.map(|(name, fd)| (name, fd, environment(name)));
    if broker_control_only {
        let (_, _, auth) = &descriptors[0];
        let (_, _, endpoint) = &descriptors[1];
        let (_, capability_fd, capability) = &descriptors[2];
        if auth.is_some() || endpoint.is_some() {
            return Err(RuntimeDescriptorSealingError::Invalid);
        }
        let Some(capability) = capability else {
            return Err(RuntimeDescriptorSealingError::Incomplete);
        };
        if capability != &OsString::from(capability_fd.to_string()) {
            return Err(RuntimeDescriptorSealingError::Invalid);
        }
        return Ok(Some(InheritedRuntimeDescriptorSet::BrokerControlOnly(
            *capability_fd,
        )));
    }
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
    Ok(Some(InheritedRuntimeDescriptorSet::Full(
        INHERITED_RUNTIME_DESCRIPTORS.map(|(_, fd)| fd),
    )))
}

#[cfg(unix)]
fn seal_runtime_descriptor_fds(descriptors: &[i32]) -> Result<(), RuntimeDescriptorSealingError> {
    let flags = descriptors.iter().copied().map(|fd| {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        if flags < 0 {
            Err(RuntimeDescriptorSealingError::Unavailable)
        } else {
            Ok(flags)
        }
    });
    let flags = flags.collect::<Result<Vec<_>, _>>()?;

    for (fd, flags) in descriptors.iter().copied().zip(flags) {
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
fn seal_runtime_descriptor_fds(_: &[i32]) -> Result<(), RuntimeDescriptorSealingError> {
    Err(RuntimeDescriptorSealingError::UnsupportedPlatform)
}

#[cfg(test)]
#[path = "runtime_descriptor_sealing_tests.rs"]
mod tests;

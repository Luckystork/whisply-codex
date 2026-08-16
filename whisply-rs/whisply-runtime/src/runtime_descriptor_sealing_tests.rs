use pretty_assertions::assert_eq;
use std::ffi::OsString;

use crate::brand::GATEWAY_AUTH_FD;
use crate::brand::GATEWAY_AUTH_FD_ENV;
use crate::brand::GATEWAY_ENDPOINT_FD;
use crate::brand::GATEWAY_ENDPOINT_FD_ENV;
use crate::brand::NATIVE_BROKER_CAPABILITY_FD;
use crate::brand::NATIVE_BROKER_CAPABILITY_FD_ENV;

use super::InheritedRuntimeDescriptorSet;
use super::RuntimeDescriptorSealingError;
use super::broker_control_only_requested;
use super::inherited_runtime_descriptor_fds;
#[cfg(unix)]
use super::seal_runtime_descriptor_fds;

#[test]
fn absent_descriptor_contract_needs_no_sealing() {
    assert_eq!(inherited_runtime_descriptor_fds(false, |_| None), Ok(None),);
}

#[test]
fn partial_descriptor_contract_is_rejected() {
    assert_eq!(
        inherited_runtime_descriptor_fds(false, |name| {
            (name == GATEWAY_AUTH_FD_ENV).then(|| OsString::from(GATEWAY_AUTH_FD.to_string()))
        }),
        Err(RuntimeDescriptorSealingError::Incomplete),
    );
}

#[test]
fn descriptor_contract_accepts_its_exact_fixed_targets() {
    assert_eq!(
        inherited_runtime_descriptor_fds(false, |name| {
            let fd = match name {
                GATEWAY_AUTH_FD_ENV => GATEWAY_AUTH_FD,
                GATEWAY_ENDPOINT_FD_ENV => GATEWAY_ENDPOINT_FD,
                NATIVE_BROKER_CAPABILITY_FD_ENV => NATIVE_BROKER_CAPABILITY_FD,
                _ => unreachable!(),
            };
            Some(OsString::from(fd.to_string()))
        }),
        Ok(Some(InheritedRuntimeDescriptorSet::Full([
            GATEWAY_AUTH_FD,
            GATEWAY_ENDPOINT_FD,
            NATIVE_BROKER_CAPABILITY_FD,
        ]))),
    );
}

#[test]
fn descriptor_contract_requires_its_fixed_targets() {
    assert_eq!(
        inherited_runtime_descriptor_fds(false, |name| {
            let fd = match name {
                GATEWAY_AUTH_FD_ENV => GATEWAY_AUTH_FD,
                GATEWAY_ENDPOINT_FD_ENV => GATEWAY_ENDPOINT_FD,
                NATIVE_BROKER_CAPABILITY_FD_ENV => 99,
                _ => unreachable!(),
            };
            Some(OsString::from(fd.to_string()))
        }),
        Err(RuntimeDescriptorSealingError::Invalid),
    );
}

#[test]
fn control_only_contract_accepts_only_the_fixed_capability_target() {
    assert_eq!(
        inherited_runtime_descriptor_fds(true, |name| {
            (name == NATIVE_BROKER_CAPABILITY_FD_ENV)
                .then(|| OsString::from(NATIVE_BROKER_CAPABILITY_FD.to_string()))
        }),
        Ok(Some(InheritedRuntimeDescriptorSet::BrokerControlOnly(
            NATIVE_BROKER_CAPABILITY_FD,
        ))),
    );
}

#[test]
fn control_only_contract_rejects_gateway_descriptors_or_a_wrong_capability_target() {
    assert_eq!(
        inherited_runtime_descriptor_fds(true, |name| {
            let value = match name {
                GATEWAY_AUTH_FD_ENV => Some(OsString::from(GATEWAY_AUTH_FD.to_string())),
                NATIVE_BROKER_CAPABILITY_FD_ENV => {
                    Some(OsString::from(NATIVE_BROKER_CAPABILITY_FD.to_string()))
                }
                _ => None,
            };
            value
        }),
        Err(RuntimeDescriptorSealingError::Invalid),
    );
    assert_eq!(
        inherited_runtime_descriptor_fds(true, |name| {
            (name == NATIVE_BROKER_CAPABILITY_FD_ENV).then(|| OsString::from("99"))
        }),
        Err(RuntimeDescriptorSealingError::Invalid),
    );
}

#[test]
fn control_only_marker_requires_its_exact_value() {
    assert_eq!(broker_control_only_requested(None), Ok(false));
    assert_eq!(
        broker_control_only_requested(Some(OsString::from("1"))),
        Ok(true),
    );
    assert_eq!(
        broker_control_only_requested(Some(OsString::from("true"))),
        Err(RuntimeDescriptorSealingError::Invalid),
    );
}

#[cfg(unix)]
#[test]
fn sealing_prevents_a_generic_child_from_observing_runtime_descriptor_targets() {
    let descriptors = PrivateDescriptorTriple::new();
    let descriptor_fds = descriptors.fds();
    seal_runtime_descriptor_fds(&descriptor_fds).expect("seal private descriptors");

    // Descriptor numbers can be reused while the shell starts, so merely
    // observing an open `/dev/fd/<number>` would not prove inheritance. Each
    // private reader has a distinct marker buffered before `exec`; only an
    // inherited original descriptor can yield that marker in the child.
    let probe = descriptor_fds
        .into_iter()
        .map(|fd| {
            format!(
                "if IFS= read -r marker <&{fd} 2>/dev/null && [ \"$marker\" = \"{PRIVATE_DESCRIPTOR_MARKER}\" ]; then exit 1; fi"
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let status = std::process::Command::new("/bin/sh")
        .args(["-c", &probe])
        .status()
        .expect("launch generic child");
    assert!(
        status.success(),
        "generic child inherited a sealed descriptor"
    );
}

#[cfg(unix)]
struct PrivateDescriptorTriple {
    descriptors: [std::os::fd::OwnedFd; 3],
}

#[cfg(unix)]
const PRIVATE_DESCRIPTOR_MARKER: &str = "whisply-runtime-descriptor-marker";
#[cfg(unix)]
const PRIVATE_DESCRIPTOR_MARKER_LINE: &[u8] = b"whisply-runtime-descriptor-marker\n";

#[cfg(unix)]
impl PrivateDescriptorTriple {
    fn new() -> Self {
        use std::os::fd::AsRawFd;
        use std::os::fd::FromRawFd;
        use std::os::fd::OwnedFd;

        let private_pipe = || {
            let mut pipe = [-1; 2];
            assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
            let read = unsafe { OwnedFd::from_raw_fd(pipe[0]) };
            let writer = unsafe { OwnedFd::from_raw_fd(pipe[1]) };
            assert_eq!(
                unsafe {
                    libc::write(
                        writer.as_raw_fd(),
                        PRIVATE_DESCRIPTOR_MARKER_LINE.as_ptr().cast(),
                        PRIVATE_DESCRIPTOR_MARKER_LINE.len(),
                    )
                },
                PRIVATE_DESCRIPTOR_MARKER_LINE.len() as isize,
                "write private descriptor marker",
            );
            let descriptor = duplicate_for_test_staging(read.as_raw_fd());
            drop(read);
            drop(writer);
            clear_close_on_exec_for_test(descriptor.as_raw_fd());
            descriptor
        };

        let first_descriptor = private_pipe();
        let second_descriptor = private_pipe();
        let third_descriptor = private_pipe();
        Self {
            descriptors: [first_descriptor, second_descriptor, third_descriptor],
        }
    }

    fn fds(&self) -> [i32; 3] {
        use std::os::fd::AsRawFd;

        [
            self.descriptors[0].as_raw_fd(),
            self.descriptors[1].as_raw_fd(),
            self.descriptors[2].as_raw_fd(),
        ]
    }
}

#[cfg(unix)]
fn duplicate_for_test_staging(fd: i32) -> std::os::fd::OwnedFd {
    use std::os::fd::FromRawFd;

    let duplicate = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 64) };
    assert!(duplicate >= 64, "stage test descriptor");
    unsafe { std::os::fd::OwnedFd::from_raw_fd(duplicate) }
}

#[cfg(unix)]
fn clear_close_on_exec_for_test(fd: i32) {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    assert!(flags >= 0, "read descriptor flags");
    assert_eq!(
        unsafe { libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) },
        0,
    );
}

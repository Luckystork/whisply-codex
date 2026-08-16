//! Public Whisply identity constants.

/// Product name shown on ordinary user-facing runtime surfaces.
pub const PRODUCT_NAME: &str = "Whisply";
/// The only public runtime executable name.
pub const EXECUTABLE_NAME: &str = "whisply";
/// Environment variable that selects the logical Whisply storage root.
pub const HOME_ENV: &str = "WHISPLY_HOME";
/// Default product-home directory beneath the operating-system user home.
pub const DEFAULT_HOME_DIR: &str = ".whisply";
/// Direct, first-party hosted provider ID.
pub const WHISPLY_PROVIDER_ID: &str = "whisply";
/// Inherited read-once descriptor containing a release-owned gateway endpoint
/// JSON document. The environment value is an FD number, never an endpoint
/// URL. This prevents a shell environment override from redirecting a
/// brokered bearer session.
pub const GATEWAY_ENDPOINT_FD_ENV: &str = "WHISPLY_GATEWAY_ENDPOINT_FD";
/// Fixed child descriptor number for the inherited gateway endpoint document.
pub const GATEWAY_ENDPOINT_FD: i32 = 41;
/// Inherited read-once descriptor containing a short-lived brokered gateway
/// token. The environment value is an FD number, never the token itself.
pub const GATEWAY_AUTH_FD_ENV: &str = "WHISPLY_GATEWAY_AUTH_FD";
/// Fixed child descriptor number for the inherited gateway authorization
/// document.
pub const GATEWAY_AUTH_FD: i32 = 40;
/// Inherited descriptor containing the per-runtime broker capability. The
/// environment value is an FD number only; the capability never appears in an
/// environment value, argument, file, or log.
pub const NATIVE_BROKER_CAPABILITY_FD_ENV: &str = "WHISPLY_NATIVE_BROKER_CAPABILITY_FD";
/// Fixed child descriptor number for the inherited native-broker capability.
pub const NATIVE_BROKER_CAPABILITY_FD: i32 = 42;
/// Marks the one fixed broker-control-only launch shape. In this mode the
/// verified launcher supplies descriptor 42 but deliberately withholds both
/// gateway descriptors until browser sign-in has created an account home.
pub const RUNTIME_BROKER_CONTROL_ONLY_ENV: &str = "WHISPLY_RUNTIME_BROKER_CONTROL_ONLY";
/// The non-secret Unix-domain socket path owned by the signed native bundle.
pub const NATIVE_BROKER_SOCKET_ENV: &str = "WHISPLY_NATIVE_BROKER_SOCKET";
/// Non-secret release-manifest hash injected only by the verified launcher into
/// the strict runtime child environment. It is never read from user config or
/// command-line input.
pub const RELEASE_MANIFEST_SHA256_ENV: &str = "WHISPLY_RELEASE_MANIFEST_SHA256";

/// Returns the verified launch manifest hash when its strict child environment
/// value has the required canonical format.
pub fn release_manifest_sha256_from_environment() -> Option<String> {
    std::env::var(RELEASE_MANIFEST_SHA256_ENV)
        .ok()
        .filter(|value| {
            value.len() == 64
                && value.bytes().all(|byte| {
                    byte.is_ascii_digit() || (byte.is_ascii_lowercase() && byte <= b'f')
                })
        })
}

/// Produces a public Whisply user agent without upstream product branding.
pub fn user_agent(runtime_version: &str) -> String {
    format!("Whisply/{runtime_version}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_agent_uses_public_whisply_identity() {
        assert_eq!(user_agent("0.147.0-wsply.1"), "Whisply/0.147.0-wsply.1");
    }
}

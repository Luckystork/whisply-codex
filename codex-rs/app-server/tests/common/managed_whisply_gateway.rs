//! Test-only native-broker fixture for the managed Whisply provider.
//!
//! The app under test receives the same FD + capability + UDS transport shape
//! as production. The loopback endpoint is accepted only by codex-whisply's
//! non-default `test-support` feature and its typed inherited-descriptor marker.

use std::fs;
use std::fs::File;
use std::io::Read;
use std::io::Seek;
use std::io::SeekFrom;
use std::io::Write;
use std::mem;
use std::os::fd::AsRawFd;
use std::os::fd::RawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::thread;
use std::thread::JoinHandle;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use anyhow::ensure;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use codex_whisply::GATEWAY_AUTH_FD_ENV;
use codex_whisply::GATEWAY_ENDPOINT_FD_ENV;
use codex_whisply::ManagedGatewayClient;
use codex_whisply::ManagedGatewayTestLaunch;
use codex_whisply::NATIVE_BROKER_CAPABILITY_FD_ENV;
use codex_whisply::NATIVE_BROKER_SOCKET_ENV;
use codex_whisply::RELEASE_MANIFEST_SHA256_ENV;
use hmac::Hmac;
use hmac::Mac as _;
use serde::Deserialize;
use serde::Serialize;
use sha2::Sha256;
use tempfile::TempDir;
use url::Url;

const TEST_TRANSPORT_MARKER: &str = "inherited-fd-localhost-v1";
const TEST_INITIAL_BEARER_DESCRIPTOR: &[u8] = b"test-initial-broker-bearer";
const TEST_BEARER_DESCRIPTOR: &[u8] = b"test-managed-broker-bearer";
const TEST_ACCOUNT_EPOCH: &str = "5ce47c6b-387a-4f92-9dd9-0bdb1082bc07";
const TEST_RELEASE_MANIFEST_SHA256: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";
const TEST_CAPABILITY: [u8; 32] = [0xA5; 32];
const BROKER_SCHEMA_VERSION: u16 = 1;
const DESCRIPTOR_TTL_MS: i64 = 60 * 60 * 1_000;
// The signed launcher maps its three one-shot authority descriptors to these
// exact child targets before the Whisply CLI begins dispatch. Keep the test
// fixture faithful to that fixed contract rather than exposing its ambient
// parent descriptor numbers.
const FIXED_CHILD_DESCRIPTOR_TARGETS: [RawFd; 3] = [40, 41, 42];

type HmacSha256 = Hmac<Sha256>;

/// Owns a synthetic but protocol-faithful managed gateway launch transport.
///
/// Values are test-private and only delivered to the child as inherited file
/// descriptors or a local UDS path; no model-provider config carries an
/// endpoint, bearer, or capability.
pub struct ManagedWhisplyGatewayFixture {
    _root: TempDir,
    socket_path: PathBuf,
    initial_auth: File,
    initial_endpoint: File,
    capability: File,
    stop: Arc<AtomicBool>,
    operations: Arc<Mutex<Vec<String>>>,
    error: Arc<Mutex<Option<String>>>,
    broker_thread: Option<JoinHandle<()>>,
}

/// Retains the broker fixture while an embedded app-server uses its typed,
/// test-scoped client. Unlike child-process setup, this never writes launch
/// authority into the environment or a process-global cache.
pub struct InProcessManagedWhisplyGatewayFixture {
    fixture: ManagedWhisplyGatewayFixture,
    client: Arc<ManagedGatewayClient>,
}

impl ManagedWhisplyGatewayFixture {
    pub fn new(gateway_base_url: &str) -> Result<Self> {
        let endpoint_descriptor = endpoint_descriptor(gateway_base_url)?;
        let root = TempDir::new().context("create managed Whisply broker fixture root")?;
        let broker_dir = root.path().join(".native-broker");
        fs::create_dir(&broker_dir).context("create managed Whisply broker fixture directory")?;
        fs::set_permissions(&broker_dir, fs::Permissions::from_mode(0o700))
            .context("restrict managed Whisply broker fixture directory")?;
        let socket_path = broker_dir.join("v1.sock");
        let listener = UnixListener::bind(&socket_path)
            .context("bind managed Whisply broker fixture socket")?;
        fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600))
            .context("restrict managed Whisply broker fixture socket")?;
        let runtime_home = root.path().join("runtime");
        fs::create_dir(&runtime_home).context("create managed Whisply fixture runtime home")?;
        let descriptor_payload = serde_json::to_vec(&serde_json::json!({
            "runtimeHome": runtime_home
                .to_str()
                .context("managed Whisply fixture runtime home must be UTF-8")?,
            "brokerSocketPath": socket_path
                .to_str()
                .context("managed Whisply fixture socket path must be UTF-8")?,
        }))
        .context("serialize managed Whisply broker descriptor payload")?;

        let initial_auth = descriptor_file(TEST_INITIAL_BEARER_DESCRIPTOR)?;
        let initial_endpoint = descriptor_file(&endpoint_descriptor)?;
        let capability = descriptor_file(&TEST_CAPABILITY)?;
        let stop = Arc::new(AtomicBool::new(false));
        let operations = Arc::new(Mutex::new(Vec::new()));
        let error = Arc::new(Mutex::new(None));
        let broker_thread = Some(spawn_broker(
            listener,
            Arc::clone(&stop),
            Arc::clone(&operations),
            Arc::clone(&error),
            endpoint_descriptor,
            descriptor_payload,
        ));

        Ok(Self {
            _root: root,
            socket_path,
            initial_auth,
            initial_endpoint,
            capability,
            stop,
            operations,
            error,
            broker_thread,
        })
    }

    /// Returns the inherited launch-descriptor environment for a direct child
    /// process test. Pair this with [`Self::configure_child_command`] before
    /// spawning: the parent keeps every fixture FD close-on-exec, then the
    /// child-only pre-exec hook clears it for these exact three descriptors.
    /// The fixture must outlive that child so its descriptor handles and
    /// native broker transport remain available.
    pub fn environment_overrides(&self) -> Vec<(String, Option<String>)> {
        vec![
            (
                GATEWAY_AUTH_FD_ENV.to_string(),
                Some(FIXED_CHILD_DESCRIPTOR_TARGETS[0].to_string()),
            ),
            (
                GATEWAY_ENDPOINT_FD_ENV.to_string(),
                Some(FIXED_CHILD_DESCRIPTOR_TARGETS[1].to_string()),
            ),
            (
                NATIVE_BROKER_CAPABILITY_FD_ENV.to_string(),
                Some(FIXED_CHILD_DESCRIPTOR_TARGETS[2].to_string()),
            ),
            (
                NATIVE_BROKER_SOCKET_ENV.to_string(),
                Some(self.socket_path.to_string_lossy().into_owned()),
            ),
            (
                RELEASE_MANIFEST_SHA256_ENV.to_string(),
                Some(TEST_RELEASE_MANIFEST_SHA256.to_string()),
            ),
            // The fixture's endpoint is an owned loopback server. Clear every
            // inherited outbound proxy route and force loopback exclusions so
            // the assertion observes the direct managed provider request.
            ("HTTP_PROXY".to_string(), None),
            ("http_proxy".to_string(), None),
            ("HTTPS_PROXY".to_string(), None),
            ("https_proxy".to_string(), None),
            ("ALL_PROXY".to_string(), None),
            ("all_proxy".to_string(), None),
            (
                "NO_PROXY".to_string(),
                Some("127.0.0.1,localhost".to_string()),
            ),
            (
                "no_proxy".to_string(),
                Some("127.0.0.1,localhost".to_string()),
            ),
        ]
    }

    /// The only descriptors this fixture may expose to one app-server child.
    pub fn inherited_descriptor_fds(&self) -> [RawFd; 3] {
        [
            self.initial_auth.as_raw_fd(),
            self.initial_endpoint.as_raw_fd(),
            self.capability.as_raw_fd(),
        ]
    }

    /// Stages exactly `descriptors` at the signed launcher's fixed child
    /// targets before the next child `exec`.
    ///
    /// Every fixture descriptor remains `FD_CLOEXEC` in the test process, so
    /// parallel fixtures cannot leak their launch authority into this child.
    /// `pre_exec` executes after fork, making the staging child-local. Each
    /// source is first copied above the fixed target range, so the mapping is
    /// safe even if a source already occupies a target or the sources cross.
    pub fn configure_child_command(
        command: &mut tokio::process::Command,
        descriptors: [RawFd; 3],
    ) -> Result<()> {
        unsafe {
            command.pre_exec(move || stage_fixed_child_descriptors(descriptors));
        }
        Ok(())
    }

    pub(crate) fn operations(&self) -> Vec<String> {
        self.operations
            .lock()
            .map_or_else(|_| Vec::new(), |operations| operations.clone())
    }

    /// Fails a consuming integration test when the fixture's broker task observed a
    /// protocol error. This is public because integration-test crates consume the
    /// fixture through `app_test_support` rather than as an internal module.
    pub fn assert_healthy(&self) -> Result<()> {
        let error = self
            .error
            .lock()
            .map_err(|_| anyhow::anyhow!("managed Whisply broker fixture state was poisoned"))?;
        if let Some(error) = error.as_deref() {
            bail!("managed Whisply broker fixture failed: {error}");
        }
        Ok(())
    }

    /// Converts this child-process fixture into a single embedded-runtime
    /// fixture. Consuming `self` prevents descriptor reuse across launch
    /// mechanisms while retaining the broker for the client's full lifetime.
    pub fn into_in_process_gateway(self) -> Result<InProcessManagedWhisplyGatewayFixture> {
        let client = ManagedGatewayClient::from_test_launch(ManagedGatewayTestLaunch::new(
            self.initial_auth
                .try_clone()
                .context("clone managed Whisply auth descriptor")?
                .into(),
            self.initial_endpoint
                .try_clone()
                .context("clone managed Whisply endpoint descriptor")?
                .into(),
            self.capability
                .try_clone()
                .context("clone managed Whisply capability descriptor")?
                .into(),
            self.socket_path.clone(),
            TEST_RELEASE_MANIFEST_SHA256.to_string(),
        ))?;
        Ok(InProcessManagedWhisplyGatewayFixture {
            fixture: self,
            client,
        })
    }
}

impl InProcessManagedWhisplyGatewayFixture {
    pub fn client(&self) -> Arc<ManagedGatewayClient> {
        Arc::clone(&self.client)
    }

    pub fn assert_healthy(&self) -> Result<()> {
        self.fixture.assert_healthy()
    }
}

impl Drop for ManagedWhisplyGatewayFixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = UnixStream::connect(&self.socket_path);
        if let Some(broker_thread) = self.broker_thread.take() {
            let _ = broker_thread.join();
        }
    }
}

fn endpoint_descriptor(gateway_base_url: &str) -> Result<Vec<u8>> {
    let endpoint =
        Url::parse(gateway_base_url).context("parse managed Whisply fixture gateway endpoint")?;
    ensure!(
        endpoint.scheme() == "http"
            && endpoint.host_str() == Some("127.0.0.1")
            && endpoint.port().is_some()
            && endpoint.username().is_empty()
            && endpoint.password().is_none()
            && endpoint.query().is_none()
            && endpoint.fragment().is_none()
            && endpoint.path() == "/",
        "managed Whisply fixture endpoint must be a root 127.0.0.1 HTTP URL"
    );
    serde_json::to_vec(&serde_json::json!({
        "schemaVersion": 1,
        "gatewayBaseUrl": gateway_base_url,
        "testTransport": TEST_TRANSPORT_MARKER,
        "testCatalogKeySet": codex_whisply::test_catalog_key_set(),
    }))
    .context("serialize managed Whisply fixture endpoint descriptor")
}

fn descriptor_file(bytes: &[u8]) -> Result<File> {
    let mut file = tempfile::tempfile().context("create managed Whisply descriptor")?;
    file.write_all(bytes)
        .context("write managed Whisply descriptor")?;
    file.seek(SeekFrom::Start(0))
        .context("rewind managed Whisply descriptor")?;
    set_close_on_exec(file.as_raw_fd())?;
    Ok(file)
}

fn set_close_on_exec(fd: RawFd) -> Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error()).context("read descriptor FD flags");
    }
    if flags & libc::FD_CLOEXEC == 0 {
        let result = unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) };
        if result < 0 {
            return Err(std::io::Error::last_os_error()).context("set descriptor close-on-exec");
        }
    }
    Ok(())
}

fn stage_fixed_child_descriptors(sources: [RawFd; 3]) -> std::io::Result<()> {
    // Duplicate every source before modifying a target: this remains correct
    // for source==target and cross-target permutations. The duplicates retain
    // `FD_CLOEXEC`; `dup2` intentionally clears it only on the fixed runtime
    // targets that the CLI seals at process entry.
    let mut staged = [-1; FIXED_CHILD_DESCRIPTOR_TARGETS.len()];
    for (index, source) in sources.into_iter().enumerate() {
        let duplicate = unsafe {
            libc::fcntl(
                source,
                libc::F_DUPFD_CLOEXEC,
                FIXED_CHILD_DESCRIPTOR_TARGETS[2] + 1,
            )
        };
        if duplicate < 0 {
            let error = std::io::Error::last_os_error();
            for descriptor in staged.into_iter().filter(|descriptor| *descriptor >= 0) {
                unsafe {
                    libc::close(descriptor);
                }
            }
            return Err(error);
        }
        staged[index] = duplicate;
    }

    for (source, target) in staged.into_iter().zip(FIXED_CHILD_DESCRIPTOR_TARGETS) {
        if unsafe { libc::dup2(source, target) } < 0 {
            let error = std::io::Error::last_os_error();
            for descriptor in staged {
                unsafe {
                    libc::close(descriptor);
                }
            }
            return Err(error);
        }
    }

    for target in FIXED_CHILD_DESCRIPTOR_TARGETS {
        clear_close_on_exec_in_child(target)?;
    }

    for descriptor in staged {
        unsafe {
            libc::close(descriptor);
        }
    }
    Ok(())
}

fn clear_close_on_exec_in_child(fd: RawFd) -> std::io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error());
    }
    if flags & libc::FD_CLOEXEC != 0 {
        let result = unsafe { libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) };
        if result < 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

fn spawn_broker(
    listener: UnixListener,
    stop: Arc<AtomicBool>,
    operations: Arc<Mutex<Vec<String>>>,
    error: Arc<Mutex<Option<String>>>,
    endpoint_descriptor: Vec<u8>,
    descriptor_payload: Vec<u8>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        while !stop.load(Ordering::Acquire) {
            let accepted = listener.accept();
            let (stream, _) = match accepted {
                Ok(connection) => connection,
                Err(source) => {
                    record_error(
                        &error,
                        format!("accept native broker fixture connection: {source}"),
                    );
                    return;
                }
            };
            if stop.load(Ordering::Acquire) {
                return;
            }
            if let Err(source) = handle_connection(
                stream,
                &operations,
                &endpoint_descriptor,
                &descriptor_payload,
            ) {
                record_error(&error, source.to_string());
                return;
            }
        }
    })
}

fn record_error(error: &Mutex<Option<String>>, message: String) {
    if let Ok(mut error) = error.lock()
        && error.is_none()
    {
        *error = Some(message);
    }
}

fn handle_connection(
    mut stream: UnixStream,
    operations: &Mutex<Vec<String>>,
    endpoint_descriptor: &[u8],
    descriptor_payload: &[u8],
) -> Result<()> {
    let request = read_request(&mut stream)?;
    verify_request(&request)?;
    operations
        .lock()
        .map_err(|_| anyhow::anyhow!("managed Whisply broker fixture operations were poisoned"))?
        .push(request.operation.clone());

    match request.operation.as_str() {
        "broker.hello" => {
            validate_hello_payload(&request)?;
            write_response(&mut stream, &request, b"{}", None, None)?;
        }
        "auth.status" => write_response(
            &mut stream,
            &request,
            br#"{"authenticated":false,"opaqueAccountKey":null,"accountEpoch":null}"#,
            None,
            None,
        )?,
        "runtime.descriptors" | "auth.refresh" => {
            let generation = next_descriptor_generation(operations)?;
            let auth = descriptor_file(TEST_BEARER_DESCRIPTOR)?;
            let endpoint = descriptor_file(endpoint_descriptor)?;
            write_response(
                &mut stream,
                &request,
                descriptor_payload,
                Some(&generation),
                Some((&auth, &endpoint)),
            )?;
        }
        operation => bail!("unexpected managed Whisply broker operation: {operation}"),
    }
    Ok(())
}

fn read_request(stream: &mut UnixStream) -> Result<BrokerRequest> {
    let mut header = [0_u8; 4];
    stream
        .read_exact(&mut header)
        .context("read managed Whisply broker request header")?;
    let length = usize::try_from(u32::from_be_bytes(header)).context("broker request length")?;
    ensure!(
        length <= 16 * 1024,
        "managed Whisply broker request is too large"
    );
    let mut body = vec![0_u8; length];
    stream
        .read_exact(&mut body)
        .context("read managed Whisply broker request")?;
    serde_json::from_slice(&body).context("decode managed Whisply broker request")
}

fn verify_request(request: &BrokerRequest) -> Result<()> {
    ensure!(
        request.schema_version == BROKER_SCHEMA_VERSION,
        "managed Whisply broker schema version is invalid"
    );
    let input = format!(
        "{BROKER_SCHEMA_VERSION}\n{}\n{}\n{}\n{}\n{}\n{}",
        request.request_id,
        request.operation,
        request.account_epoch.as_deref().unwrap_or_default(),
        request.nonce,
        request.issued_at_ms,
        request.payload_base64_url,
    );
    let proof = URL_SAFE_NO_PAD
        .decode(&request.capability_proof_base64_url)
        .context("decode managed Whisply broker capability proof")?;
    let mut mac = HmacSha256::new_from_slice(&TEST_CAPABILITY)
        .map_err(|_| anyhow::anyhow!("construct managed Whisply broker capability proof"))?;
    mac.update(input.as_bytes());
    mac.verify_slice(&proof)
        .map_err(|_| anyhow::anyhow!("managed Whisply broker capability proof did not verify"))
}

fn validate_hello_payload(request: &BrokerRequest) -> Result<()> {
    let payload = URL_SAFE_NO_PAD
        .decode(&request.payload_base64_url)
        .context("decode managed Whisply broker hello payload")?;
    let value: serde_json::Value =
        serde_json::from_slice(&payload).context("decode managed Whisply broker hello JSON")?;
    let object = value
        .as_object()
        .context("managed Whisply broker hello payload must be an object")?;
    let expected_keys = [
        "clientKind",
        "releaseManifestSha256",
        "runtimeCapabilityBase64Url",
    ];
    ensure!(
        object.len() == expected_keys.len()
            && expected_keys.iter().all(|key| object.contains_key(*key)),
        "managed Whisply broker hello payload has unexpected keys"
    );
    let payload: HelloPayload =
        serde_json::from_value(value).context("decode managed Whisply broker hello shape")?;
    ensure!(
        payload.client_kind == "runtime"
            && payload.release_manifest_sha256 == TEST_RELEASE_MANIFEST_SHA256
            && payload.runtime_capability_base64_url.is_none(),
        "managed Whisply broker hello payload is not the canonical runtime hello"
    );
    Ok(())
}

fn next_descriptor_generation(operations: &Mutex<Vec<String>>) -> Result<String> {
    let operations = operations
        .lock()
        .map_err(|_| anyhow::anyhow!("managed Whisply broker fixture operations were poisoned"))?;
    let refresh_count = u64::try_from(
        operations
            .iter()
            .filter(|operation| {
                operation.as_str() == "runtime.descriptors" || operation.as_str() == "auth.refresh"
            })
            .count(),
    )
    .context("managed Whisply broker descriptor generation")?;
    ensure!(
        refresh_count <= 0xffff_ffff_ffff,
        "managed Whisply broker fixture exhausted descriptor generations"
    );
    Ok(format!("00000000-0000-4000-8000-{refresh_count:012x}"))
}

fn write_response(
    stream: &mut UnixStream,
    request: &BrokerRequest,
    payload: &[u8],
    descriptor_generation: Option<&str>,
    descriptors: Option<(&File, &File)>,
) -> Result<()> {
    let expires_at_ms = if descriptor_generation.is_some() {
        Some(unix_ms()?.saturating_add(DESCRIPTOR_TTL_MS))
    } else {
        None
    };
    let response = BrokerResponse {
        schema_version: BROKER_SCHEMA_VERSION,
        request_id: request.request_id.clone(),
        status: "ok",
        account_epoch: descriptor_generation.map(|_| TEST_ACCOUNT_EPOCH),
        descriptor_generation,
        expires_at_ms,
        payload_base64_url: URL_SAFE_NO_PAD.encode(payload),
    };
    let body = serde_json::to_vec(&response).context("encode managed Whisply broker response")?;
    let header = u32::try_from(body.len())
        .context("managed Whisply broker response length")?
        .to_be_bytes();
    if let Some((auth, endpoint)) = descriptors {
        send_header_with_descriptors(stream, &header, &[auth.as_raw_fd(), endpoint.as_raw_fd()])?;
    } else {
        stream
            .write_all(&header)
            .context("write managed Whisply broker response header")?;
    }
    stream
        .write_all(&body)
        .context("write managed Whisply broker response")
}

fn send_header_with_descriptors(
    stream: &UnixStream,
    header: &[u8; 4],
    fds: &[RawFd],
) -> Result<()> {
    let mut header = *header;
    let mut iov = libc::iovec {
        iov_base: header.as_mut_ptr().cast(),
        iov_len: header.len(),
    };
    let control_len =
        unsafe { libc::CMSG_SPACE((fds.len() * mem::size_of::<RawFd>()) as _) as usize };
    let mut control = vec![0_u8; control_len];
    let mut message: libc::msghdr = unsafe { mem::zeroed() };
    message.msg_iov = &mut iov;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen =
        u32::try_from(control.len()).context("broker control message length")?;
    unsafe {
        let control_header = libc::CMSG_FIRSTHDR(&mut message);
        ensure!(
            !control_header.is_null(),
            "managed Whisply broker control header is missing"
        );
        (*control_header).cmsg_level = libc::SOL_SOCKET;
        (*control_header).cmsg_type = libc::SCM_RIGHTS;
        (*control_header).cmsg_len =
            libc::CMSG_LEN((fds.len() * mem::size_of::<RawFd>()) as _) as _;
        std::ptr::copy_nonoverlapping(
            fds.as_ptr().cast::<u8>(),
            libc::CMSG_DATA(control_header),
            fds.len() * mem::size_of::<RawFd>(),
        );
    }
    let written = unsafe { libc::sendmsg(stream.as_raw_fd(), &message, 0) };
    if written < 0 {
        return Err(std::io::Error::last_os_error())
            .context("send managed Whisply broker descriptors");
    }
    let written =
        usize::try_from(written).context("managed Whisply broker descriptor header length")?;
    ensure!(
        written <= header.len(),
        "managed Whisply broker wrote an oversized descriptor header"
    );
    if written < header.len() {
        let mut stream = stream;
        stream
            .write_all(&header[written..])
            .context("finish managed Whisply broker descriptor header")?;
    }
    Ok(())
}

fn unix_ms() -> Result<i64> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("read managed Whisply broker fixture clock")?
        .as_millis();
    i64::try_from(millis).context("managed Whisply broker fixture clock exceeds i64")
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BrokerRequest {
    #[serde(rename = "schemaVersion")]
    schema_version: u16,
    #[serde(rename = "requestID")]
    request_id: String,
    #[serde(rename = "operation")]
    operation: String,
    #[serde(rename = "accountEpoch")]
    account_epoch: Option<String>,
    #[serde(rename = "nonce")]
    nonce: String,
    #[serde(rename = "issuedAtMS")]
    issued_at_ms: i64,
    #[serde(rename = "payloadBase64URL")]
    payload_base64_url: String,
    #[serde(rename = "capabilityProofBase64URL")]
    capability_proof_base64_url: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HelloPayload {
    #[serde(rename = "clientKind")]
    client_kind: String,
    #[serde(rename = "releaseManifestSha256")]
    release_manifest_sha256: String,
    #[serde(rename = "runtimeCapabilityBase64Url")]
    runtime_capability_base64_url: Option<String>,
}

#[derive(Serialize)]
struct BrokerResponse<'a> {
    #[serde(rename = "schemaVersion")]
    schema_version: u16,
    #[serde(rename = "requestID")]
    request_id: String,
    #[serde(rename = "status")]
    status: &'a str,
    #[serde(rename = "accountEpoch", skip_serializing_if = "Option::is_none")]
    account_epoch: Option<&'a str>,
    #[serde(
        rename = "descriptorGeneration",
        skip_serializing_if = "Option::is_none"
    )]
    descriptor_generation: Option<&'a str>,
    #[serde(rename = "expiresAtMS", skip_serializing_if = "Option::is_none")]
    expires_at_ms: Option<i64>,
    #[serde(rename = "payloadBase64URL")]
    payload_base64_url: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_uses_the_native_broker_envelope_and_runtime_hello_shape() {
        let request: BrokerRequest = serde_json::from_slice(
            br#"{"schemaVersion":1,"requestID":"request-0001","operation":"broker.hello","accountEpoch":null,"nonce":"nonce-0001","issuedAtMS":1700000000123,"payloadBase64URL":"eyJjbGllbnRLaW5kIjoicnVudGltZSIsInJlbGVhc2VNYW5pZmVzdFNoYTI1NiI6IjAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAiLCJydW50aW1lQ2FwYWJpbGl0eUJhc2U2NFVybCI6bnVsbH0","capabilityProofBase64URL":"proof"}"#,
        )
        .expect("decode canonical native request vector");
        validate_hello_payload(&request).expect("canonical runtime hello payload");
        assert!(serde_json::from_slice::<BrokerRequest>(
            br#"{"schemaVersion":1,"requestId":"request-0001","operation":"broker.hello","accountEpoch":null,"nonce":"nonce-0001","issuedAtMs":1700000000123,"payloadBase64Url":"e30","capabilityProofBase64Url":"proof"}"#
        )
        .is_err());

        let response = BrokerResponse {
            schema_version: 1,
            request_id: "request-0001".to_string(),
            status: "ok",
            account_epoch: Some(TEST_ACCOUNT_EPOCH),
            descriptor_generation: Some("00000000-0000-4000-8000-000000000001"),
            expires_at_ms: Some(4_102_444_800_000),
            payload_base64_url: "e30".to_string(),
        };
        assert_eq!(
            serde_json::to_value(&response).expect("encode canonical native response"),
            serde_json::json!({
                "schemaVersion": 1,
                "requestID": "request-0001",
                "status": "ok",
                "accountEpoch": TEST_ACCOUNT_EPOCH,
                "descriptorGeneration": "00000000-0000-4000-8000-000000000001",
                "expiresAtMS": 4_102_444_800_000_i64,
                "payloadBase64URL": "e30",
            })
        );

        let operations = Mutex::new(vec!["runtime.descriptors".to_string()]);
        assert_eq!(
            next_descriptor_generation(&operations).expect("fixture generation"),
            "00000000-0000-4000-8000-000000000001"
        );
    }

    #[test]
    fn fixture_refreshes_descriptors_over_a_real_unix_socket() -> Result<()> {
        let fixture = ManagedWhisplyGatewayFixture::new("http://127.0.0.1:43123/")?
            .into_in_process_gateway()?;

        // Check the broker's own failure before the client maps it to its
        // intentionally redacted public error. This exercises the real
        // SCM_RIGHTS handoff used by managed response fixtures.
        let refreshed = fixture.client().ensure_fresh();
        fixture.assert_healthy()?;
        refreshed?;
        assert_eq!(
            fixture.fixture.operations(),
            vec![
                "broker.hello".to_string(),
                "runtime.descriptors".to_string()
            ]
        );
        Ok(())
    }

    #[tokio::test]
    async fn fixture_stages_the_fixed_runtime_descriptor_contract_for_a_child() -> Result<()> {
        let fixture = ManagedWhisplyGatewayFixture::new("http://127.0.0.1:43123/")?;
        let mut command = tokio::process::Command::new("/bin/sh");
        command.arg("-c").arg(
            "test \"$WHISPLY_GATEWAY_AUTH_FD\" = 40 \\
                 && test \"$WHISPLY_GATEWAY_ENDPOINT_FD\" = 41 \\
                 && test \"$WHISPLY_NATIVE_BROKER_CAPABILITY_FD\" = 42 \\
                 && test -r /dev/fd/40 \\
                 && test -r /dev/fd/41 \\
                 && test -r /dev/fd/42",
        );
        for (key, value) in fixture.environment_overrides() {
            match value {
                Some(value) => {
                    command.env(key, value);
                }
                None => {
                    command.env_remove(key);
                }
            }
        }
        ManagedWhisplyGatewayFixture::configure_child_command(
            &mut command,
            fixture.inherited_descriptor_fds(),
        )?;

        assert!(
            command.status().await?.success(),
            "fixture child should receive exactly the fixed runtime descriptors"
        );
        Ok(())
    }
}

use super::*;

fn descriptor() -> serde_json::Value {
    serde_json::json!({"schemaVersion":1,"sessionToken":format!("wus_{}", "a".repeat(43)),
        "installId":"70000000-0000-4000-8000-000000000003","expiresAtMS":now_unix_ms().unwrap()+30_000})
}

#[test]
fn exact_exam_descriptor_is_short_lived_and_redacted() {
    let value = descriptor();
    let proof = parse_exam_proof_descriptor(&serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(proof.is_current());
    assert_eq!(
        proof.installation_id(),
        "70000000-0000-4000-8000-000000000003"
    );
    assert_eq!(proof.token(), value["sessionToken"].as_str().unwrap());
    assert_eq!(format!("{proof:?}"), "RuntimeExamProof([REDACTED])");
}

#[test]
fn malformed_expired_or_unbounded_exam_proofs_are_rejected() {
    for (key, invalid) in [
        ("schemaVersion", serde_json::json!(2)),
        ("sessionToken", serde_json::json!("not-a-proof")),
        (
            "sessionToken",
            serde_json::json!(format!("wus_{}!", "a".repeat(42))),
        ),
        ("installId", serde_json::json!("not-an-installation")),
        ("expiresAtMS", serde_json::json!(0)),
        ("expiresAtMS", serde_json::json!(i64::MAX)),
        ("unexpectedAuthority", serde_json::json!(true)),
    ] {
        let mut value = descriptor();
        value[key] = invalid;
        assert!(
            parse_exam_proof_descriptor(&serde_json::to_vec(&value).unwrap()).is_err(),
            "{key}"
        );
    }
}

#[test]
fn invalid_explicit_reference_never_becomes_absent() {
    for value in [
        "",
        "bad",
        "70000000000040008000000000000001",
        "70000000-0000-1000-8000-000000000001",
    ] {
        assert_eq!(
            RuntimeExamTurnReference::parse(value),
            RuntimeExamTurnReference::Invalid
        );
    }
    let valid = "70000000-0000-4000-8000-000000000001";
    assert_eq!(
        RuntimeExamTurnReference::parse(valid).metadata_value(),
        valid
    );
}

#[test]
fn proof_descriptor_cannot_cross_an_ordinary_broker_operation() {
    let response = BrokerResponse {
        schema_version: BROKER_SCHEMA_VERSION,
        request_id: "request".into(),
        status: "ok".into(),
        account_epoch: Some("70000000-0000-4000-8000-000000000003".into()),
        descriptor_generation: None,
        expires_at_ms: None,
        payload_base64_url: String::new(),
        code: None,
    };
    let fds = vec![std::fs::File::open("/dev/null").unwrap().into()];
    assert!(
        validate_response_envelope(&response, BrokerOperation::ExamTurnProof, &[], &fds).is_ok()
    );
    assert!(validate_response_envelope(&response, BrokerOperation::AuthStatus, &[], &fds).is_err());
    assert!(
        validate_response_envelope(&response, BrokerOperation::ExamTurnProof, &[], &[]).is_err()
    );
    assert!(
        validate_response_envelope(
            &response,
            BrokerOperation::ExamTurnProof,
            b"secret-in-wire-json",
            &fds
        )
        .is_err()
    );
}

#[cfg(unix)]
fn pipe_with_writer() -> (std::os::fd::OwnedFd, std::fs::File) {
    use std::os::fd::FromRawFd;
    let mut pair = [-1; 2];
    assert_eq!(unsafe { libc::pipe(pair.as_mut_ptr()) }, 0);
    unsafe {
        (
            std::os::fd::OwnedFd::from_raw_fd(pair[0]),
            std::fs::File::from_raw_fd(pair[1]),
        )
    }
}

#[cfg(unix)]
#[test]
fn proof_pipe_is_bounded_even_when_writer_never_closes() {
    let (reader, _writer) = pipe_with_writer();
    let began = std::time::Instant::now();
    assert!(read_exam_proof_pipe(reader).is_err());
    assert!(began.elapsed() < std::time::Duration::from_secs(2));
    let regular = std::fs::File::open("/dev/null").unwrap().into();
    assert!(read_exam_proof_pipe(regular).is_err());
}

#[cfg(unix)]
#[test]
fn proof_pipe_accepts_exact_bytes_and_rejects_oversize() {
    use std::io::Write;
    for size in [100, 1_025] {
        let (reader, mut writer) = pipe_with_writer();
        writer.write_all(&vec![b'a'; size]).unwrap();
        drop(writer);
        let result = read_exam_proof_pipe(reader);
        if size <= 1_024 {
            assert_eq!(result.unwrap().len(), size);
        } else {
            assert!(result.is_err());
        }
    }
}

#[cfg(unix)]
fn send_proof_frame(
    stream: &std::os::unix::net::UnixStream,
    value: serde_json::Value,
    descriptor: std::os::fd::OwnedFd,
) {
    use std::os::fd::AsRawFd;
    let body = serde_json::to_vec(&value).unwrap();
    let mut packet = (body.len() as u32).to_be_bytes().to_vec();
    packet.extend(body);
    let mut control = vec![0_u8; control_space_for_fds(1)];
    let mut iov = libc::iovec {
        iov_base: packet.as_mut_ptr().cast(),
        iov_len: packet.len(),
    };
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut iov;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = control.len() as _;
    unsafe {
        let header = libc::CMSG_FIRSTHDR(&message);
        (*header).cmsg_level = libc::SOL_SOCKET;
        (*header).cmsg_type = libc::SCM_RIGHTS;
        (*header).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<i32>() as _) as _;
        std::ptr::write_unaligned(
            libc::CMSG_DATA(header).cast::<i32>(),
            descriptor.as_raw_fd(),
        );
        assert_eq!(
            libc::sendmsg(stream.as_raw_fd(), &message, 0),
            packet.len() as isize
        );
    }
}

#[cfg(unix)]
#[test]
fn one_authenticated_broker_request_waits_for_confirmation_and_receives_only_a_protected_pipe() {
    use std::io::{Read, Write};
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;
    use std::sync::mpsc;
    let folder = tempfile::Builder::new()
        .prefix("exam-wire-")
        .tempdir()
        .unwrap();
    let socket = folder.path().join("proof.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
    let capability = [9_u8; 32];
    let epoch = "5ce47c6b-387a-4f92-9dd9-0bdb1082bc07";
    let reference = Uuid::new_v4();
    let (observed_tx, observed_rx) = mpsc::channel();
    let (confirm_tx, confirm_rx) = mpsc::channel();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        let mut size = [0; 4];
        stream.read_exact(&mut size).unwrap();
        let mut bytes = vec![0; u32::from_be_bytes(size) as usize];
        stream.read_exact(&mut bytes).unwrap();
        let request: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(request["operation"], "exam.turn.proof");
        assert_eq!(request["accountEpoch"], epoch);
        let payload = request["payloadBase64URL"].as_str().unwrap();
        let input = broker_capability_proof_input(
            request["requestID"].as_str().unwrap(),
            BrokerOperation::ExamTurnProof,
            Some(epoch),
            request["nonce"].as_str().unwrap(),
            request["issuedAtMS"].as_i64().unwrap(),
            payload,
        );
        let mut mac = HmacSha256::new_from_slice(&capability).unwrap();
        mac.update(input.as_bytes());
        mac.verify_slice(
            &URL_SAFE_NO_PAD
                .decode(request["capabilityProofBase64URL"].as_str().unwrap())
                .unwrap(),
        )
        .unwrap();
        let binding: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).unwrap()).unwrap();
        assert_eq!(
            binding,
            serde_json::json!({"reference":reference,"threadID":"thread","turnID":"turn",
            "parentThreadID":null,"parentTurnID":null,"automaticReview":false})
        );
        assert!(!String::from_utf8_lossy(&bytes).contains("wus_"));
        observed_tx.send(()).unwrap();
        confirm_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        let (proof_fd, mut writer) = pipe_with_writer();
        writer
            .write_all(&serde_json::to_vec(&descriptor()).unwrap())
            .unwrap();
        drop(writer);
        send_proof_frame(
            &stream,
            serde_json::json!({"schemaVersion":1,"requestID":request["requestID"],
            "status":"ok","accountEpoch":epoch,"payloadBase64URL":""}),
            proof_fd,
        );
    });
    let client = ManagedGatewayClient {
        broker: Arc::new(NativeBrokerClient {
            socket_path: socket,
            capability: Zeroizing::new(capability),
            release_manifest_sha256: "a".repeat(64),
        }),
        state: Mutex::new(GatewayDescriptorSnapshot::for_test_with(
            "synthetic",
            "https://proxy.whisply.net",
            Some("70000000-0000-4000-8000-000000000003"),
            Some(now_unix_ms().unwrap() + 120_000),
        )),
        refresh_gate: Mutex::new(()),
        #[cfg(feature = "test-support")]
        test_catalog_key_set: None,
    };
    let caller = std::thread::spawn(move || {
        client.exam_turn_proof(
            RuntimeExamTurnReference::Valid(reference),
            "thread",
            "turn",
            None,
        )
    });
    observed_rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    confirm_tx.send(()).unwrap();
    let proof = caller.join().unwrap().unwrap();
    assert!(proof.is_current());
    assert_eq!(proof.token(), format!("wus_{}", "a".repeat(43)));
    server.join().unwrap();
}

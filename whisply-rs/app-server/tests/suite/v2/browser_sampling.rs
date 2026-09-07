//! Real stable JSON-RPC + authenticated Unix/FD broker + loopback provider.
//! No production provider, native browser, or customer data is involved.
#![cfg(target_os = "macos")]

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use app_test_support::{
    BrowserStepFixtureControl, ManagedWhisplyConfig, ManagedWhisplyGatewayFixture, TestAppServer,
};
use codex_app_server_protocol::*;
use core_test_support::responses;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::time::timeout;
use uuid::Uuid;
use wiremock::{MockServer, ResponseTemplate};

const WAIT: Duration = Duration::from_secs(10);
const MODEL: &str = "gpt-5.6-terra";
const INSTALL: &str = "70000000-0000-4000-8000-000000000001";
const LEASE: &str = "synthetic-browser-task-lease-no-production-authority";
const RECEIPT_ID: &str = "80000000-0000-4000-8000-000000000008";
const OUTPUT: &str = r#"{"verified":true}"#;

async fn start(server: &MockServer) -> Result<(TestAppServer, BrowserStepFixtureControl, TempDir)> {
    start_with_capabilities(server, false).await
}

async fn start_with_capabilities(
    server: &MockServer,
    experimental_api: bool,
) -> Result<(TestAppServer, BrowserStepFixtureControl, TempDir)> {
    let home = TempDir::new()?;
    ManagedWhisplyConfig::new()
        .with_model(MODEL)
        .write(home.path())?;
    let gateway = ManagedWhisplyGatewayFixture::new(&server.uri())?;
    let control = gateway.browser_steps();
    let mut app = TestAppServer::builder()
        .with_codex_home(home.path())
        .with_managed_whisply_gateway(gateway)
        .build()
        .await?;
    let initialized = app
        .initialize_with_capabilities(
            ClientInfo {
                name: "whisply-browser-stable-test".to_string(),
                title: None,
                version: "1".to_string(),
            },
            Some(InitializeCapabilities {
                experimental_api,
                ..Default::default()
            }),
        )
        .await?;
    anyhow::ensure!(
        matches!(initialized, JSONRPCMessage::Response(_)),
        "stable initialization failed"
    );
    Ok((app, control, home))
}

async fn prepared(
    app: &mut TestAppServer,
    control: &BrowserStepFixtureControl,
    model_id: &str,
) -> Result<(WhisplyBrowserSampleParams, Value)> {
    let models: ModelListResponse = app
        .request(|request_id| ClientRequest::ModelList {
            request_id,
            params: ModelListParams {
                limit: Some(100),
                cursor: None,
                include_hidden: None,
            },
        })
        .await?;
    let model = models
        .data
        .iter()
        .find(|model| model.id == model_id)
        .context("signed selected model")?;
    let effort = if model.supported_reasoning_efforts.is_empty() {
        None
    } else {
        Some(
            serde_json::to_value(&model.default_reasoning_effort)?
                .as_str()
                .context("effort")?
                .to_string(),
        )
    };
    let catalog = codex_whisply::test_signed_catalog_envelope()?;
    let signed = catalog
        .catalog
        .models
        .iter()
        .find(|model| model.id.as_str() == model_id)
        .context("catalog model")?;
    let payload = WhisplyBrowserSamplingPayload {
        purpose: WhisplyBrowserSamplingPurpose::Planning,
        model_id: model_id.to_string(),
        maximum_output_tokens: u32::try_from(signed.capabilities.output_limit)?,
        reasoning_effort: effort,
        system_prompt: "Return the exact JSON decision. No actions are available.".to_string(),
        input_json: r#"{"objective":"Verify the synthetic page"}"#.to_string(),
        output_schema_json: codex_whisply::canonical_browser_schema_json(&json!({
            "type":"object", "properties":{"verified":{"type":"boolean"}},
            "required":["verified"], "additionalProperties":false,
        }))?,
        visual_jpeg_data_url: None,
    };
    let reference = Uuid::new_v4().to_string();
    let request_id = Uuid::new_v4().to_string();
    let task_id = Uuid::new_v4().to_string();
    let proof = json!({
        "schemaVersion":1, "accountID":Uuid::new_v4().to_string(), "installationID":INSTALL,
        "taskID":task_id, "requestID":request_id, "componentID":Uuid::new_v4().to_string(),
        "executionLeaseToken":LEASE,
        "payloadDigest":codex_whisply::browser_sampling_payload_digest(&payload, &request_id, &task_id)?,
        "expiresAtMS":i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis())? + 120_000,
    });
    control.register(&reference, proof.clone())?;
    Ok((WhisplyBrowserSampleParams { reference, payload }, proof))
}

fn completed() -> String {
    responses::sse(vec![
        responses::ev_response_created("browser-sample"),
        responses::ev_assistant_message("browser-decision", OUTPUT),
        responses::ev_completed_with_tokens("browser-sample", 60),
    ])
}

fn receipt_event(proof: &Value) -> Value {
    let mut event = responses::ev_completed_with_tokens("browser-sample", 60);
    event["response"]["whisply_browser_receipt"] = json!({"status":"settled",
        "requestId":proof["requestID"], "componentId":proof["componentID"], "receiptId":RECEIPT_ID});
    event
}

fn completed_with_receipt(proof: &Value) -> String {
    responses::sse(vec![
        responses::ev_response_created("browser-sample"),
        responses::ev_assistant_message("browser-decision", OUTPUT),
        receipt_event(proof),
    ])
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn browser_requires_matching_durable_receipt_and_accepts_shared_wire_fixture() -> Result<()> {
    let server = responses::start_mock_server().await;
    let (mut app, control, _home) = start(&server).await?;
    for case in [
        "absent",
        "wrong_request",
        "wrong_component",
        "unknown",
        "blank_receipt",
        "model_text_spoof",
    ] {
        let (params, proof) = prepared(&mut app, &control, MODEL).await?;
        let mut event = receipt_event(&proof);
        match case {
            "absent" | "model_text_spoof" => {
                event["response"]
                    .as_object_mut()
                    .unwrap()
                    .remove("whisply_browser_receipt");
            }
            "wrong_request" => {
                event["response"]["whisply_browser_receipt"]["requestId"] =
                    Uuid::new_v4().to_string().into()
            }
            "wrong_component" => {
                event["response"]["whisply_browser_receipt"]["componentId"] =
                    Uuid::new_v4().to_string().into()
            }
            "unknown" => {
                event["response"]["whisply_browser_receipt"]["status"] =
                    "reconciliation_required".into()
            }
            "blank_receipt" => {
                event["response"]["whisply_browser_receipt"]["receiptId"] = "".into()
            }
            _ => unreachable!(),
        }
        let output = if case == "model_text_spoof" {
            receipt_event(&proof)["response"].to_string()
        } else {
            OUTPUT.to_string()
        };
        responses::mount_sse_once(
            &server,
            responses::sse(vec![
                responses::ev_assistant_message("decision", &output),
                event,
            ]),
        )
        .await;
        let request = app
            .send_raw_request(
                "whisply/browser/sample",
                Some(serde_json::to_value(params)?),
            )
            .await?;
        let error = timeout(
            WAIT,
            app.read_stream_until_error_message(RequestId::Integer(request)),
        )
        .await??;
        assert!(
            error.error.message.contains("Usage receipt"),
            "{case}: {}",
            error.error.message
        );
    }
    let wire = include_str!(
        "../../../../whisply-runtime/fixtures/browser-runtime-settled-response-v1.sse"
    );
    let completed: Value = serde_json::from_str(
        wire.lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .last()
            .unwrap(),
    )?;
    let receipt = &completed["response"]["whisply_browser_receipt"];
    let (params, mut proof) = prepared(&mut app, &control, MODEL).await?;
    proof["requestID"] = receipt["requestId"].clone();
    proof["componentID"] = receipt["componentId"].clone();
    proof["payloadDigest"] = codex_whisply::browser_sampling_payload_digest(
        &params.payload,
        proof["requestID"].as_str().unwrap(),
        proof["taskID"].as_str().unwrap(),
    )?
    .into();
    control.register(&params.reference, proof)?;
    responses::mount_sse_once(&server, wire.to_string()).await;
    let result: WhisplyBrowserSampleResponse = app
        .request(|request_id| ClientRequest::WhisplyBrowserSample {
            request_id,
            params: params.clone(),
        })
        .await?;
    assert_eq!(result.output_json, OUTPUT);
    assert_eq!(result.receipt.receipt_id, receipt["receiptId"]);
    assert_eq!(result.receipt.status, WhisplyBrowserReceiptStatus::Settled);
    let count = server.received_requests().await.unwrap().len();
    let replay = app
        .send_raw_request(
            "whisply/browser/sample",
            Some(serde_json::to_value(params)?),
        )
        .await?;
    timeout(
        WAIT,
        app.read_stream_until_error_message(RequestId::Integer(replay)),
    )
    .await??;
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        count,
        "completed reference replay never dispatches again"
    );
    app.assert_managed_whisply_gateway_healthy()?;
    Ok(())
}

fn assert_exact_request(
    request: &responses::ResponsesRequest,
    params: &WhisplyBrowserSampleParams,
    proof: &Value,
) {
    let body = request.body_json();
    assert_eq!(
        request.header("x-whisply-protocol-version").as_deref(),
        Some("1-browser-sampling")
    );
    assert_eq!(
        request.header("x-whisply-request-id").as_deref(),
        proof["requestID"].as_str()
    );
    assert_eq!(
        request.header("x-whisply-contextual-task-id").as_deref(),
        proof["taskID"].as_str()
    );
    assert_eq!(
        request.header("x-whisply-browser-component-id").as_deref(),
        proof["componentID"].as_str()
    );
    assert_eq!(
        request
            .header("x-whisply-contextual-execution-lease")
            .as_deref(),
        Some(LEASE)
    );
    assert_eq!(
        request
            .header("x-whisply-browser-payload-digest")
            .as_deref(),
        proof["payloadDigest"].as_str()
    );
    assert_eq!(body["model"], params.payload.model_id);
    assert_eq!(body["instructions"], params.payload.system_prompt);
    assert_eq!(
        body["max_output_tokens"],
        params.payload.maximum_output_tokens
    );
    assert_eq!(body["tools"], json!([]));
    assert_eq!(body["parallel_tool_calls"], false);
    assert_eq!(body["store"], false);
    let mut content = vec![json!({"type":"input_text", "text":params.payload.input_json})];
    if let Some(image) = &params.payload.visual_jpeg_data_url {
        content.push(json!({"type":"input_image", "image_url":image}));
    }
    assert_eq!(
        body["input"],
        json!([{"type":"message", "role":"user", "content":content}])
    );
    assert_eq!(
        body["text"]["format"]["schema"],
        serde_json::from_str::<Value>(&params.payload.output_schema_json).unwrap()
    );
    let rendered = body.to_string();
    for secret in [
        LEASE,
        &params.reference,
        proof["accountID"].as_str().unwrap(),
        proof["componentID"].as_str().unwrap(),
    ] {
        assert!(
            !rendered.contains(secret),
            "private authority reached model material"
        );
    }
    // Optional test-only export of the actual serializer output. This fixture
    // contains only synthetic material and never calls a real provider.
    if params.payload.model_id == MODEL
        && let Some(directory) = std::env::var_os("WHISPLY_TEST_BROWSER_WIRE_DIR")
    {
        let purpose = if params.payload.visual_jpeg_data_url.is_some() {
            "visual"
        } else {
            "planning"
        };
        let headers = [
            "x-whisply-protocol-version",
            "x-whisply-request-id",
            "x-whisply-contextual-task-id",
            "x-whisply-contextual-execution-lease",
            "x-whisply-browser-component-id",
            "x-whisply-browser-payload-digest",
            "x-whisply-browser-purpose",
            "x-whisply-install-id",
        ]
        .into_iter()
        .map(|name| (name.to_string(), json!(request.header(name))))
        .collect::<serde_json::Map<_, _>>();
        let destination =
            std::path::PathBuf::from(directory).join(format!("outbound-{purpose}.json"));
        std::fs::write(
            destination,
            serde_json::to_vec_pretty(&json!({
                "syntheticOnly":true, "payload":params.payload, "body":body, "headers":headers,
            }))
            .unwrap(),
        )
        .unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn browser_and_chrome_nested_samples_finish_while_outer_native_tool_is_pending() -> Result<()>
{
    let server = responses::start_mock_server().await;
    responses::mount_sse_once(
        &server,
        responses::sse(vec![
            responses::ev_function_call_with_namespace(
                "spawned-call",
                "whisply",
                "browser",
                r#"{"operation":"Inspect the synthetic page"}"#,
            ),
            responses::ev_completed("outer1"),
        ]),
    )
    .await;
    // Outer production-style thread uses the existing environment capability;
    // independent sample/receipt/cancel tests disable experimental APIs.
    let (mut app, control, _home) = start_with_capabilities(&server, true).await?;
    let ThreadStartResponse { thread, .. } = app.start_thread(ThreadStartParams::default()).await?;
    let _: WhisplyToolAdmissionRegisterResponse = app
        .request(|request_id| ClientRequest::WhisplyToolAdmissionRegister {
            request_id,
            params: WhisplyToolAdmissionRegisterParams {
                admission_id: "browser-parent-admission".to_string(),
                thread_id: thread.id.clone(),
                client_user_message_id: "browser-parent-message".to_string(),
                tool_ids: vec!["whisply.browser".to_string(), "whisply.chrome".to_string()],
            },
        })
        .await?;
    let TurnStartResponse { turn } = app
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                thread_id: thread.id.clone(),
                client_user_message_id: Some("browser-parent-message".to_string()),
                input: vec![UserInput::Text {
                    text: "Inspect the synthetic browser pages.".to_string(),
                    text_elements: vec![],
                }],
                ..Default::default()
            },
        })
        .await?;
    for (index, call_id) in ["spawned-call", "chrome-call"].into_iter().enumerate() {
        let outer = timeout(WAIT, app.read_stream_until_request_message()).await??;
        let ServerRequest::WhisplyToolAdmittedExecute {
            request_id: outer_id,
            params: outer,
        } = outer
        else {
            panic!("expected the pending outer native Browser tool");
        };
        assert_eq!(outer.turn_id, turn.id);
        assert_eq!(outer.call.execution_id, call_id);
        // The outer server request remains unanswered while a second stable
        // request on the same process/connection claims its native FD proof.
        let (params, proof) = prepared(&mut app, &control, MODEL).await?;
        let sample_mock = responses::mount_sse_once(&server, completed_with_receipt(&proof)).await;
        let result: WhisplyBrowserSampleResponse = timeout(
            WAIT,
            app.request(|request_id| ClientRequest::WhisplyBrowserSample {
                request_id,
                params: params.clone(),
            }),
        )
        .await??;
        assert_eq!(result.output_json, OUTPUT);
        assert_eq!(result.request_id, proof["requestID"]);
        assert_eq!(result.component_id, proof["componentID"]);
        assert_exact_request(&sample_mock.single_request(), &params, &proof);
        assert_eq!(result.receipt.receipt_id, RECEIPT_ID);
        let next_outer = if index == 0 {
            responses::sse(vec![
                responses::ev_function_call_with_namespace(
                    "chrome-call",
                    "whisply",
                    "chrome",
                    r#"{"operation":"Inspect the synthetic page"}"#,
                ),
                responses::ev_completed("outer2"),
            ])
        } else {
            responses::sse(vec![
                responses::ev_assistant_message("final", "The native browser tasks completed."),
                responses::ev_completed("outer3"),
            ])
        };
        responses::mount_sse_once(&server, next_outer).await;
        app.send_response(
            outer_id,
            serde_json::to_value(WhisplyToolResult {
                execution_id: call_id.to_string(),
                status: WhisplyToolTerminalStatus::Succeeded,
                content: Some(json!({"summary":"Synthetic native Browser task completed."})),
                safe_summary: "Synthetic native Browser task completed.".to_string(),
                setup_route: None,
                receipt_id: None,
            })?,
        )
        .await?;
    }
    let done: TurnCompletedNotification =
        timeout(WAIT, app.read_notification("turn/completed")).await??;
    assert_eq!(done.turn.id, turn.id);
    assert_eq!(done.turn.status, TurnStatus::Completed);
    assert_eq!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|request| request.method.as_str() == "POST")
            .count(),
        5
    );
    assert_eq!(
        app.managed_whisply_broker_operations()
            .unwrap()
            .iter()
            .filter(|op| *op == "browser.step.proof")
            .count(),
        2
    );
    app.assert_managed_whisply_gateway_healthy()?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn browser_uses_each_signed_models_exact_output_and_reasoning_limits_without_tools()
-> Result<()> {
    let server = responses::start_mock_server().await;
    let catalog = codex_whisply::test_signed_catalog_envelope()?;
    let (mut app, control, _home) = start(&server).await?;
    for model in &catalog.catalog.models {
        for visual in [false, true] {
            let (mut params, mut proof) = prepared(&mut app, &control, model.id.as_str()).await?;
            if visual {
                params.payload.purpose = WhisplyBrowserSamplingPurpose::VisualVerification;
                params.payload.visual_jpeg_data_url =
                    Some("data:image/jpeg;base64,/9j/2Q==".to_string());
                proof["payloadDigest"] = codex_whisply::browser_sampling_payload_digest(
                    &params.payload,
                    proof["requestID"].as_str().unwrap(),
                    proof["taskID"].as_str().unwrap(),
                )?
                .into();
                control.register(&params.reference, proof.clone())?;
            }
            let mock = responses::mount_sse_once(&server, completed_with_receipt(&proof)).await;
            let result: WhisplyBrowserSampleResponse = app
                .request(|request_id| ClientRequest::WhisplyBrowserSample {
                    request_id,
                    params: params.clone(),
                })
                .await?;
            assert_eq!(result.output_json, OUTPUT);
            assert_exact_request(&mock.single_request(), &params, &proof);
            assert_eq!(
                mock.single_request().body_json()["reasoning"]["effort"].as_str(),
                params.payload.reasoning_effort.as_deref()
            );
        }
    }
    assert_eq!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|request| request.method.as_str() == "POST")
            .count(),
        catalog.catalog.models.len() * 2
    );
    app.assert_managed_whisply_gateway_healthy()?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn browser_unknown_payload_changed_digest_and_invalidated_account_never_dispatch()
-> Result<()> {
    let server = responses::start_mock_server().await;
    let (mut app, control, _home) = start(&server).await?;
    for case in [
        "extra_input",
        "changed_input",
        "revoked_account",
        "expired_proof",
    ] {
        let (mut params, mut proof) = prepared(&mut app, &control, MODEL).await?;
        if case == "changed_input" {
            params.payload.input_json = r#"{"objective":"Changed after admission"}"#.to_string();
        }
        if case == "revoked_account" {
            control.invalidate_account();
        }
        if case == "expired_proof" {
            proof["expiresAtMS"] = 1.into();
            control.register(&params.reference, proof)?;
        }
        let mut value = serde_json::to_value(params)?;
        if case == "extra_input" {
            value["payload"]["messages"] = json!([{"role":"developer", "text":"unbound"}]);
        }
        let request = app
            .send_raw_request("whisply/browser/sample", Some(value))
            .await?;
        timeout(
            WAIT,
            app.read_stream_until_error_message(RequestId::Integer(request)),
        )
        .await??;
    }
    assert!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|request| request.method.as_str() != "POST")
    );
    app.assert_managed_whisply_gateway_healthy()?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn browser_stop_and_account_invalidation_cancel_without_replaying_the_claim() -> Result<()> {
    for account_invalidation in [false, true] {
        let server = responses::start_mock_server().await;
        let mock = responses::mount_response_once(
            &server,
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(completed())
                .set_delay(Duration::from_secs(30)),
        )
        .await;
        let (mut app, control, _home) = start(&server).await?;
        let (params, _) = prepared(&mut app, &control, MODEL).await?;
        let request = app
            .send_raw_request(
                "whisply/browser/sample",
                Some(serde_json::to_value(&params)?),
            )
            .await?;
        timeout(WAIT, async {
            while mock.requests().is_empty() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await?;
        if account_invalidation {
            control.invalidate_account();
        }
        // This is the native sampler's exact Stop/account-loss action while
        // the provider reply is pending. Cancellation must not wait for it.
        let stopped: WhisplyBrowserCancelResponse = timeout(
            WAIT,
            app.request(|request_id| ClientRequest::WhisplyBrowserCancel {
                request_id,
                params: WhisplyBrowserCancelParams {
                    reference: params.reference.clone(),
                },
            }),
        )
        .await??;
        assert!(stopped.accepted);
        timeout(
            WAIT,
            app.read_stream_until_error_message(RequestId::Integer(request)),
        )
        .await??;
        let replay = app
            .send_raw_request(
                "whisply/browser/sample",
                Some(serde_json::to_value(&params)?),
            )
            .await?;
        timeout(
            WAIT,
            app.read_stream_until_error_message(RequestId::Integer(replay)),
        )
        .await??;
        assert_eq!(
            mock.requests().len(),
            1,
            "a claimed step cannot start another provider request"
        );
        app.assert_managed_whisply_gateway_healthy()?;
    }
    Ok(())
}

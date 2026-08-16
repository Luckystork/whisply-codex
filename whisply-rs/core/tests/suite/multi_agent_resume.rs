#![cfg(target_os = "macos")]

use anyhow::Result;
use app_test_support::ManagedWhisplyGatewayFixture;
use core_test_support::responses::ResponseMock;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call_with_namespace;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_once_match;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::Instant;
use tokio::time::sleep;
use whisply_core::CodexThread;
use whisply_core::ThreadManager;
use whisply_core::config::AgentRoleConfig;
use whisply_features::Feature;
use whisply_model_provider::whisply_provider_info;
use whisply_protocol::ThreadId;
use whisply_protocol::models::PermissionProfile;
use whisply_protocol::openai_models::ReasoningEffort;
use whisply_protocol::protocol::AgentStatus;
use whisply_protocol::protocol::EventMsg;
use whisply_protocol::protocol::Op;
use whisply_protocol::protocol::Submission;
use whisply_protocol::user_input::UserInput;

const COLLABORATION_NAMESPACE: &str = "collaboration";
const SPAWN_CALL_ID: &str = "spawn-worker";
const NESTED_CALL_ID: &str = "spawn-grandchild";
const QUEUE_CALL_ID: &str = "queue-worker-message";
const FOLLOWUP_CALL_ID: &str = "followup-worker";
const SIBLING_SPAWN_CALL_ID: &str = "spawn-survivor";
const SIBLING_FOLLOWUP_CALL_ID: &str = "followup-survivor";
const INTERRUPT_CALL_ID: &str = "interrupt-worker";
const INITIAL_PROMPT: &str = "spawn a durable worker";
const INITIAL_TASK: &str = "inspect the repository";
const NESTED_TASK: &str = "inspect the nested repository";
const QUEUE_PROMPT: &str = "queue context for the durable worker";
const QUEUED_MESSAGE: &str = "queue-only context from an earlier parent turn";
const FOLLOWUP_PROMPT: &str = "continue the durable worker";
const FOLLOWUP_TASK: &str = "inspect the tests too";
const SIBLING_PROMPT: &str = "spawn a second durable worker";
const SIBLING_TASK: &str = "inspect the release lifecycle";
const SIBLING_FOLLOWUP_PROMPT: &str = "continue the surviving worker";
const SIBLING_FOLLOWUP_TASK: &str = "verify the surviving worker";
const INTERRUPT_PROMPT: &str = "release the interrupted worker";
const SIBLING_NAME: &str = "survivor";
const ROLE_NAME: &str = "durable_worker";
const ROLE_MODEL: &str = "gpt-5.6-sol";
const ROLE_MODEL_PROVIDER_ID: &str = "whisply";
const ROLE_DEVELOPER_INSTRUCTIONS: &str = "Keep the durable worker role configuration.";
const SUBAGENT_DEVELOPER_INSTRUCTIONS: &str = "Use the default durable worker instructions.";
const CHILD_LIFECYCLE_TIMEOUT: Duration = Duration::from_secs(2);

fn decoded_body(request: &wiremock::Request) -> Option<Vec<u8>> {
    let is_zstd = request
        .headers
        .get("content-encoding")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(',')
                .any(|entry| entry.trim().eq_ignore_ascii_case("zstd"))
        });
    if is_zstd {
        zstd::stream::decode_all(std::io::Cursor::new(&request.body)).ok()
    } else {
        Some(request.body.clone())
    }
}

fn body_contains(request: &wiremock::Request, text: &str) -> bool {
    decoded_body(request)
        .and_then(|body| String::from_utf8(body).ok())
        .is_some_and(|body| body.contains(text))
}

fn request_has_model(request: &wiremock::Request, model: &str) -> bool {
    decoded_body(request)
        .and_then(|body| serde_json::from_slice::<Value>(&body).ok())
        .is_some_and(|body| body.get("model").and_then(Value::as_str) == Some(model))
}

fn request_has_input_type(request: &wiremock::Request, input_type: &str) -> bool {
    decoded_body(request)
        .and_then(|body| serde_json::from_slice::<Value>(&body).ok())
        .and_then(|body| body.get("input").and_then(Value::as_array).cloned())
        .is_some_and(|items| {
            items
                .iter()
                .any(|item| item.get("type").and_then(Value::as_str) == Some(input_type))
        })
}

/// The direct Whisply route intentionally sends only its bounded metadata
/// envelope, rather than the upstream flat client-metadata projection.
fn whisply_turn_metadata(body: &Value) -> Option<Value> {
    body["client_metadata"]["x-whisply-turn-metadata"]
        .as_str()
        .and_then(|metadata| serde_json::from_str(metadata).ok())
}

async fn wait_for_child_thread_id(
    request_mock: &ResponseMock,
    parent_thread_id: ThreadId,
    description: &str,
) -> Result<ThreadId> {
    let deadline = Instant::now() + CHILD_LIFECYCLE_TIMEOUT;
    loop {
        if let Some(thread_id) = request_mock.requests().into_iter().find_map(|request| {
            let body = request.body_json();
            let metadata = whisply_turn_metadata(&body)?;
            if metadata["parent_thread_id"] != json!(parent_thread_id) {
                return None;
            }
            metadata["thread_id"]
                .as_str()
                .and_then(|thread_id| ThreadId::from_string(thread_id).ok())
        }) {
            return Ok(thread_id);
        }
        if Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for {description} request");
        }
        sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_for_spawned_child_turn_metadata(
    server: &wiremock::MockServer,
    parent_thread_id: ThreadId,
    task: &str,
    description: &str,
) -> Result<Value> {
    let deadline = Instant::now() + CHILD_LIFECYCLE_TIMEOUT;
    loop {
        let requests = server
            .received_requests()
            .await
            .expect("captured response requests");
        if let Some(metadata) = requests.into_iter().find_map(|request| {
            let body = decoded_body(&request)?;
            let parsed_body = serde_json::from_slice::<Value>(&body).ok()?;
            let metadata = whisply_turn_metadata(&parsed_body)?;
            (body_contains(&request, task)
                && request_has_input_type(&request, "agent_message")
                && metadata["parent_thread_id"] == json!(parent_thread_id)
                && metadata["thread_id"] != json!(parent_thread_id)
                && metadata["subagent_kind"] == "thread_spawn")
                .then_some(metadata)
        }) {
            return Ok(metadata);
        }
        if Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for {description} request");
        }
        sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_for_registered_thread(
    thread_manager: &ThreadManager,
    thread_id: ThreadId,
    description: &str,
) -> Result<Arc<CodexThread>> {
    let deadline = Instant::now() + CHILD_LIFECYCLE_TIMEOUT;
    loop {
        if let Ok(thread) = thread_manager.get_thread(thread_id).await {
            return Ok(thread);
        }
        if Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for {description} registration");
        }
        sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_for_thread_completion(thread: &CodexThread, description: &str) -> Result<()> {
    let deadline = Instant::now() + CHILD_LIFECYCLE_TIMEOUT;
    loop {
        if matches!(thread.agent_status().await, AgentStatus::Completed(_)) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for {description} completion");
        }
        sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_for_thread_removal(
    thread_manager: &ThreadManager,
    thread_id: ThreadId,
    description: &str,
) -> Result<()> {
    let deadline = Instant::now() + CHILD_LIFECYCLE_TIMEOUT;
    loop {
        if thread_manager.get_thread(thread_id).await.is_err() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for {description} removal");
        }
        sleep(Duration::from_millis(10)).await;
    }
}

async fn mount_root_collaboration_call(
    server: &wiremock::MockServer,
    prompt: &'static str,
    call_id: &'static str,
    tool_name: &'static str,
    arguments: &str,
) {
    let first_response_id = format!("resp-{call_id}-1");
    mount_sse_once_match(
        server,
        move |request: &wiremock::Request| {
            body_contains(request, prompt) && !request_has_model(request, ROLE_MODEL)
        },
        sse(vec![
            ev_response_created(&first_response_id),
            ev_function_call_with_namespace(call_id, COLLABORATION_NAMESPACE, tool_name, arguments),
            ev_completed(&first_response_id),
        ]),
    )
    .await;

    let second_response_id = format!("resp-{call_id}-2");
    let message_id = format!("msg-{call_id}-2");
    mount_sse_once_match(
        server,
        move |request: &wiremock::Request| {
            body_contains(request, call_id) && !request_has_model(request, ROLE_MODEL)
        },
        sse(vec![
            ev_response_created(&second_response_id),
            ev_assistant_message(&message_id, "collaboration completed"),
            ev_completed(&second_response_id),
        ]),
    )
    .await;
}

fn configure_multi_agent_v2_with_role(config: &mut whisply_core::config::Config) {
    config.model = Some("gpt-5.6-terra".to_string());
    config.model_provider = whisply_provider_info();
    config
        .features
        .enable(Feature::Collab)
        .expect("test config should allow feature update");
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    config.multi_agent_v2.subagent_developer_instructions =
        Some(SUBAGENT_DEVELOPER_INSTRUCTIONS.to_string());
    config.multi_agent_v2.max_concurrent_threads_per_session = 3;
    let role_path = config.codex_home.join("durable-worker-role.toml");
    std::fs::write(
        &role_path,
        format!(
            "model = \"{ROLE_MODEL}\"\nmodel_reasoning_effort = \"high\"\ndeveloper_instructions = \"{ROLE_DEVELOPER_INSTRUCTIONS}\"\nsandbox_mode = \"read-only\"\n"
        ),
    )
    .expect("write durable worker role config");
    config.agent_roles.insert(
        ROLE_NAME.to_string(),
        AgentRoleConfig {
            description: Some("Durable worker role".to_string()),
            config_file: Some(role_path.to_path_buf()),
            nickname_candidates: None,
        },
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cold_root_resume_restores_agent_identity_and_role_on_followup() -> Result<()> {
    let server = start_mock_server().await;
    let managed_gateway =
        ManagedWhisplyGatewayFixture::new(&server.uri())?.into_in_process_gateway()?;
    let spawn_args = serde_json::to_string(&json!({
        "message": INITIAL_TASK,
        "task_name": "worker",
        "agent_type": ROLE_NAME,
        "fork_turns": "none",
    }))?;
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| body_contains(request, INITIAL_PROMPT),
        sse(vec![
            ev_response_created("resp-spawn-1"),
            ev_function_call_with_namespace(
                SPAWN_CALL_ID,
                COLLABORATION_NAMESPACE,
                "spawn_agent",
                &spawn_args,
            ),
            ev_completed("resp-spawn-1"),
        ]),
    )
    .await;
    let initial_child_request = mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            request_has_model(request, ROLE_MODEL)
                && request_has_input_type(request, "agent_message")
                && body_contains(request, INITIAL_TASK)
        },
        sse(vec![
            ev_response_created("resp-worker-1"),
            ev_function_call_with_namespace(
                NESTED_CALL_ID,
                COLLABORATION_NAMESPACE,
                "spawn_agent",
                r#"{"message":"inspect the nested repository","task_name":"grandchild","fork_turns":"none"}"#,
            ),
            ev_completed("resp-worker-1"),
        ]),
    )
    .await;
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            body_contains(request, NESTED_TASK)
                && request_has_input_type(request, "agent_message")
                && !body_contains(request, NESTED_CALL_ID)
        },
        sse(vec![ev_completed("resp-parent-turn-assistant")]),
    )
    .await;
    for (text, is_subagent) in [(NESTED_CALL_ID, true), (QUEUE_CALL_ID, false)] {
        mount_sse_once_match(
            &server,
            move |request: &wiremock::Request| {
                body_contains(request, text)
                    && request_has_input_type(request, "agent_message") == is_subagent
            },
            sse(vec![ev_completed("resp-parent-turn-assistant")]),
        )
        .await;
    }
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            body_contains(request, SPAWN_CALL_ID) && !request_has_model(request, ROLE_MODEL)
        },
        sse(vec![
            ev_response_created("resp-spawn-2"),
            ev_assistant_message("msg-spawn-2", "worker spawned"),
            ev_completed("resp-spawn-2"),
        ]),
    )
    .await;

    let mut initial_builder = test_codex()
        .with_managed_gateway_client(managed_gateway.client())
        .with_config(move |config| {
            configure_multi_agent_v2_with_role(config);
        });
    let initial = initial_builder.build_with_auto_env(&server).await?;
    let root_thread_id = initial.session_configured.thread_id;
    let mut op = vec![UserInput::Text {
        text: INITIAL_PROMPT.to_string(),
        text_elements: Vec::new(),
    }]
    .into();
    if let Op::UserInput {
        thread_settings, ..
    } = &mut op
    {
        thread_settings.permission_profile = Some(PermissionProfile::Disabled);
    }
    initial
        .codex
        .submit_with_id(Submission {
            id: "spoofed-root-turn".to_string(),
            op,
            client_user_message_id: None,
            trace: None,
            parent_turn_id: Some("spoofed-parent-turn".to_string()),
        })
        .await?;
    wait_for_event(&initial.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;

    let worker_thread_id =
        wait_for_child_thread_id(&initial_child_request, root_thread_id, "spawned worker").await?;
    let worker_thread =
        wait_for_registered_thread(&initial.thread_manager, worker_thread_id, "spawned worker")
            .await?;
    wait_for_thread_completion(worker_thread.as_ref(), "spawned worker").await?;
    let nested_metadata = wait_for_spawned_child_turn_metadata(
        &server,
        worker_thread_id,
        NESTED_TASK,
        "nested grandchild",
    )
    .await?;
    assert_eq!(nested_metadata["parent_thread_id"], json!(worker_thread_id));
    assert_eq!(nested_metadata["subagent_kind"], "thread_spawn");
    assert!(initial_child_request.requests().iter().any(|request| {
        request.body_contains_text(INITIAL_TASK)
            && request.body_contains_text(ROLE_DEVELOPER_INSTRUCTIONS)
            && request.body_contains_text("<permission_profile type=\"disabled\">")
            && !request.body_contains_text(SUBAGENT_DEVELOPER_INSTRUCTIONS)
    }));
    let initial_worker_config = worker_thread.config_snapshot().await;
    let initial_worker_role_config = (
        initial_worker_config.model,
        initial_worker_config.model_provider_id,
        initial_worker_config.reasoning_effort,
        initial_worker_config.permission_profile,
    );
    assert_eq!(
        initial_worker_role_config,
        (
            ROLE_MODEL.to_string(),
            ROLE_MODEL_PROVIDER_ID.to_string(),
            Some(ReasoningEffort::High),
            PermissionProfile::Disabled,
        )
    );

    let sibling_spawn_args = serde_json::to_string(&json!({
        "message": SIBLING_TASK,
        "task_name": SIBLING_NAME,
        "agent_type": ROLE_NAME,
        "fork_turns": "none",
    }))?;
    mount_root_collaboration_call(
        &server,
        SIBLING_PROMPT,
        SIBLING_SPAWN_CALL_ID,
        "spawn_agent",
        &sibling_spawn_args,
    )
    .await;
    let sibling_initial_request = mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            request_has_model(request, ROLE_MODEL)
                && request_has_input_type(request, "agent_message")
                && body_contains(request, SIBLING_TASK)
        },
        sse(vec![
            ev_response_created("resp-survivor-1"),
            ev_assistant_message("msg-survivor-1", "initial survivor task complete"),
            ev_completed("resp-survivor-1"),
        ]),
    )
    .await;
    initial.submit_turn(SIBLING_PROMPT).await?;

    let sibling_thread_id =
        wait_for_child_thread_id(&sibling_initial_request, root_thread_id, "spawned sibling")
            .await?;
    let sibling_thread = wait_for_registered_thread(
        &initial.thread_manager,
        sibling_thread_id,
        "spawned sibling",
    )
    .await?;
    wait_for_thread_completion(sibling_thread.as_ref(), "spawned sibling").await?;
    sibling_thread.flush_rollout().await?;
    worker_thread.flush_rollout().await?;
    initial.codex.flush_rollout().await?;
    let persisted_worker = worker_thread
        .read_thread(
            /*include_archived*/ true, /*include_history*/ false,
        )
        .await?;
    assert_eq!(persisted_worker.parent_thread_id, Some(root_thread_id));
    assert_eq!(
        persisted_worker.source.parent_thread_id(),
        Some(root_thread_id)
    );
    sibling_thread.shutdown_and_wait().await?;
    worker_thread.shutdown_and_wait().await?;
    drop(sibling_thread);
    drop(worker_thread);

    let followup_args = serde_json::to_string(&json!({
        "target": "worker",
        "message": FOLLOWUP_TASK,
    }))?;
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| body_contains(request, FOLLOWUP_PROMPT),
        sse(vec![
            ev_response_created("resp-followup-1"),
            ev_function_call_with_namespace(
                FOLLOWUP_CALL_ID,
                COLLABORATION_NAMESPACE,
                "followup_task",
                &followup_args,
            ),
            ev_completed("resp-followup-1"),
        ]),
    )
    .await;
    let followup_child_request = mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            request_has_model(request, ROLE_MODEL)
                && request_has_input_type(request, "agent_message")
                && body_contains(request, FOLLOWUP_TASK)
                && body_contains(request, QUEUED_MESSAGE)
        },
        sse(vec![
            ev_response_created("resp-worker-2"),
            ev_assistant_message("msg-worker-2", "follow-up complete"),
            ev_completed("resp-worker-2"),
        ]),
    )
    .await;
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            body_contains(request, FOLLOWUP_CALL_ID) && !request_has_model(request, ROLE_MODEL)
        },
        sse(vec![
            ev_response_created("resp-followup-2"),
            ev_assistant_message("msg-followup-2", "follow-up sent"),
            ev_completed("resp-followup-2"),
        ]),
    )
    .await;

    let mut resume_builder = test_codex()
        .with_managed_gateway_client(managed_gateway.client())
        .with_config(move |config| {
            configure_multi_agent_v2_with_role(config);
        });
    let resumed = resume_builder.restart(&server, &initial).await?;
    drop(initial);
    assert_eq!(
        resumed.thread_manager.list_thread_ids().await,
        vec![root_thread_id]
    );
    assert!(
        resumed
            .thread_manager
            .get_thread(worker_thread_id)
            .await
            .is_err()
    );
    assert!(
        resumed
            .thread_manager
            .get_thread(sibling_thread_id)
            .await
            .is_err()
    );

    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| body_contains(request, QUEUE_PROMPT),
        sse(vec![
            ev_response_created("resp-queue"),
            ev_function_call_with_namespace(
                QUEUE_CALL_ID,
                COLLABORATION_NAMESPACE,
                "send_message",
                r#"{"target":"worker","message":"queue-only context from an earlier parent turn"}"#,
            ),
            ev_completed("resp-queue"),
        ]),
    )
    .await;
    resumed.submit_turn(QUEUE_PROMPT).await?;
    let reloaded_worker =
        wait_for_registered_thread(&resumed.thread_manager, worker_thread_id, "reloaded worker")
            .await?;
    let reloaded_worker_config = reloaded_worker.config_snapshot().await;
    assert_eq!(
        reloaded_worker_config.parent_thread_id,
        Some(root_thread_id),
        "cold-reloaded worker must retain the persisted root parent before handling a follow-up",
    );
    assert_eq!(
        reloaded_worker_config.session_source.parent_thread_id(),
        Some(root_thread_id),
    );

    resumed.submit_turn(FOLLOWUP_PROMPT).await?;
    wait_for_thread_completion(reloaded_worker.as_ref(), "reloaded worker").await?;
    assert!(followup_child_request.requests().iter().any(|request| {
        request.body_contains_text(FOLLOWUP_TASK)
            && request.body_contains_text(ROLE_DEVELOPER_INSTRUCTIONS)
            && request.body_contains_text("<permission_profile type=\"disabled\">")
            && !request.body_contains_text(SUBAGENT_DEVELOPER_INSTRUCTIONS)
    }));
    let requests = server
        .received_requests()
        .await
        .expect("captured response requests");
    assert!(!followup_child_request.requests().iter().any(|request| {
        whisply_turn_metadata(&request.body_json()).is_some_and(|metadata| {
            metadata["thread_id"] == json!(worker_thread_id)
                && request.body_contains_text(QUEUED_MESSAGE)
                && !request.body_contains_text(FOLLOWUP_TASK)
        })
    }));
    let body_for = |text: &str, thread: whisply_protocol::ThreadId| {
        requests
            .iter()
            .find_map(|request| {
                let body: Value = serde_json::from_slice(&decoded_body(request)?).ok()?;
                let metadata = whisply_turn_metadata(&body)?;
                (body_contains(request, text) && metadata["thread_id"] == json!(thread))
                    .then_some((body, metadata))
            })
            .expect("matching model request for expected thread")
    };
    let (_initial_root, initial_root_metadata) = body_for(INITIAL_PROMPT, root_thread_id);
    let (_queue_root, queue_root_metadata) = body_for(QUEUE_PROMPT, root_thread_id);
    let (_followup_root, followup_root_metadata) = body_for(FOLLOWUP_PROMPT, root_thread_id);
    let (_initial_child, initial_child_metadata) = body_for(INITIAL_TASK, worker_thread_id);
    let (_followup_child, followup_child_metadata) = body_for(FOLLOWUP_TASK, worker_thread_id);
    let initial_parent = initial_root_metadata["turn_id"]
        .as_str()
        .expect("initial parent turn");
    let queue_parent = queue_root_metadata["turn_id"]
        .as_str()
        .expect("queue-only parent turn");
    let followup_parent = followup_root_metadata["turn_id"]
        .as_str()
        .expect("follow-up parent turn");
    let nested_parent = initial_child_metadata["turn_id"]
        .as_str()
        .expect("nested worker parent turn");
    assert_ne!(followup_parent, initial_parent);
    assert_ne!(followup_parent, queue_parent);
    for (metadata, parent_thread, parent_turn) in [
        (&initial_root_metadata, None, None),
        (&queue_root_metadata, None, None),
        (&followup_root_metadata, None, None),
        (
            &initial_child_metadata,
            Some(root_thread_id),
            Some(initial_parent),
        ),
        (
            &followup_child_metadata,
            Some(root_thread_id),
            Some(followup_parent),
        ),
        (
            &nested_metadata,
            Some(worker_thread_id),
            Some(nested_parent),
        ),
    ] {
        if let Some(parent_thread) = parent_thread {
            assert_eq!(metadata["parent_thread_id"], json!(parent_thread));
        }
        assert_eq!(metadata["parent_turn_id"].as_str(), parent_turn);
    }
    let reloaded_worker_role_config = (
        reloaded_worker_config.model,
        reloaded_worker_config.model_provider_id,
        reloaded_worker_config.reasoning_effort,
        reloaded_worker_config.permission_profile,
    );
    assert_eq!(reloaded_worker_role_config, initial_worker_role_config);

    reloaded_worker.shutdown_and_wait().await?;
    assert!(
        resumed
            .thread_manager
            .get_thread(worker_thread_id)
            .await
            .is_ok()
    );

    let interrupt_args = serde_json::to_string(&json!({
        "target": "worker",
    }))?;
    mount_root_collaboration_call(
        &server,
        INTERRUPT_PROMPT,
        INTERRUPT_CALL_ID,
        "interrupt_agent",
        &interrupt_args,
    )
    .await;
    resumed.submit_turn(INTERRUPT_PROMPT).await?;
    wait_for_thread_removal(
        &resumed.thread_manager,
        worker_thread_id,
        "interrupted worker",
    )
    .await?;

    let sibling_followup_args = serde_json::to_string(&json!({
        "target": SIBLING_NAME,
        "message": SIBLING_FOLLOWUP_TASK,
    }))?;
    mount_root_collaboration_call(
        &server,
        SIBLING_FOLLOWUP_PROMPT,
        SIBLING_FOLLOWUP_CALL_ID,
        "followup_task",
        &sibling_followup_args,
    )
    .await;
    let sibling_followup_request = mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            request_has_model(request, ROLE_MODEL)
                && request_has_input_type(request, "agent_message")
                && body_contains(request, SIBLING_FOLLOWUP_TASK)
        },
        sse(vec![
            ev_response_created("resp-survivor-2"),
            ev_assistant_message("msg-survivor-2", "survivor follow-up complete"),
            ev_completed("resp-survivor-2"),
        ]),
    )
    .await;
    resumed.submit_turn(SIBLING_FOLLOWUP_PROMPT).await?;

    let surviving_sibling = wait_for_registered_thread(
        &resumed.thread_manager,
        sibling_thread_id,
        "surviving sibling",
    )
    .await?;
    wait_for_thread_completion(surviving_sibling.as_ref(), "surviving sibling").await?;
    assert!(sibling_followup_request.requests().iter().any(|request| {
        request.body_contains_text(SIBLING_FOLLOWUP_TASK)
            && request.body_contains_text(ROLE_DEVELOPER_INSTRUCTIONS)
    }));
    managed_gateway.assert_healthy()?;

    Ok(())
}

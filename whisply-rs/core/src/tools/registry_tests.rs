use super::*;
use crate::session::session::Session;
use crate::session::step_context::StepContext;
use futures::future::BoxFuture;
use pretty_assertions::assert_eq;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use whisply_protocol::DEFAULT_FUNCTION_NAMESPACE;

struct TestHandler {
    tool_name: whisply_tools::ToolName,
}

impl ToolExecutor<ToolInvocation> for TestHandler {
    fn tool_name(&self) -> whisply_tools::ToolName {
        self.tool_name.clone()
    }

    fn spec(&self) -> whisply_tools::ToolSpec {
        test_spec(&self.tool_name)
    }

    fn handle(&self, _invocation: ToolInvocation) -> whisply_tools::ToolExecutorFuture<'_> {
        Box::pin(async {
            Ok(
                Box::new(crate::tools::context::FunctionToolOutput::from_text(
                    "ok".to_string(),
                    Some(true),
                )) as Box<dyn crate::tools::context::ToolOutput>,
            )
        })
    }
}

impl CoreToolRuntime for TestHandler {}

struct ReadinessTestHandler {
    handler: TestHandler,
    readiness_waits: Arc<AtomicUsize>,
}

impl ToolExecutor<ToolInvocation> for ReadinessTestHandler {
    fn tool_name(&self) -> whisply_tools::ToolName {
        self.handler.tool_name()
    }

    fn spec(&self) -> whisply_tools::ToolSpec {
        self.handler.spec()
    }

    fn handle(&self, invocation: ToolInvocation) -> whisply_tools::ToolExecutorFuture<'_> {
        self.handler.handle(invocation)
    }
}

impl CoreToolRuntime for ReadinessTestHandler {
    fn wait_until_ready<'a>(&'a self, _session: &'a Arc<Session>) -> Option<BoxFuture<'a, ()>> {
        Some(Box::pin(async {
            self.readiness_waits.fetch_add(1, Ordering::Relaxed);
        }))
    }
}

#[derive(Clone)]
enum LifecycleTestResult {
    Ok { success: bool },
    Err,
}

struct LifecycleTestHandler {
    tool_name: whisply_tools::ToolName,
    result: LifecycleTestResult,
}

impl ToolExecutor<ToolInvocation> for LifecycleTestHandler {
    fn tool_name(&self) -> whisply_tools::ToolName {
        self.tool_name.clone()
    }

    fn spec(&self) -> whisply_tools::ToolSpec {
        test_spec(&self.tool_name)
    }

    fn handle(&self, invocation: ToolInvocation) -> whisply_tools::ToolExecutorFuture<'_> {
        assert_eq!(
            invocation.tool_name,
            self.tool_name.clone().with_default_namespace()
        );
        Box::pin(self.handle_call())
    }
}

impl LifecycleTestHandler {
    async fn handle_call(
        &self,
    ) -> Result<Box<dyn crate::tools::context::ToolOutput>, FunctionCallError> {
        match self.result.clone() {
            LifecycleTestResult::Ok { success } => Ok(Box::new(
                crate::tools::context::FunctionToolOutput::from_text(
                    "ok".to_string(),
                    Some(success),
                ),
            )
                as Box<dyn crate::tools::context::ToolOutput>),
            LifecycleTestResult::Err => Err(FunctionCallError::RespondToModel(
                "handler failed".to_string(),
            )),
        }
    }
}

impl CoreToolRuntime for LifecycleTestHandler {}

fn test_spec(tool_name: &whisply_tools::ToolName) -> whisply_tools::ToolSpec {
    whisply_tools::ToolSpec::Function(whisply_tools::ResponsesApiTool {
        name: tool_name.name.clone(),
        description: "Test tool.".to_string(),
        strict: false,
        defer_loading: None,
        parameters: whisply_tools::JsonSchema::default(),
        output_schema: None,
    })
}

#[derive(Debug, PartialEq, Eq)]
enum RecordedToolLifecycle {
    Start {
        call_id: String,
        tool_name: whisply_tools::ToolName,
    },
    Finish {
        call_id: String,
        tool_name: whisply_tools::ToolName,
        outcome: whisply_extension_api::ToolCallOutcome,
    },
}

struct ToolLifecycleRecorder {
    records: Arc<std::sync::Mutex<Vec<RecordedToolLifecycle>>>,
}

impl whisply_extension_api::ToolLifecycleContributor for ToolLifecycleRecorder {
    fn on_tool_start<'a>(
        &'a self,
        input: whisply_extension_api::ToolStartInput<'a>,
    ) -> whisply_extension_api::ToolLifecycleFuture<'a> {
        let records = Arc::clone(&self.records);
        let record = RecordedToolLifecycle::Start {
            call_id: input.call_id.to_string(),
            tool_name: input.tool_name.clone(),
        };
        Box::pin(async move {
            records
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(record);
        })
    }

    fn on_tool_finish<'a>(
        &'a self,
        input: whisply_extension_api::ToolFinishInput<'a>,
    ) -> whisply_extension_api::ToolLifecycleFuture<'a> {
        let records = Arc::clone(&self.records);
        let record = RecordedToolLifecycle::Finish {
            call_id: input.call_id.to_string(),
            tool_name: input.tool_name.clone(),
            outcome: input.outcome,
        };
        Box::pin(async move {
            records
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(record);
        })
    }
}

#[test]
fn handler_normalizes_only_the_default_namespace() {
    let namespace = "mcp__codex_apps__gmail";
    let tool_name = "gmail_get_recent_emails";
    let plain_name = whisply_tools::ToolName::plain(tool_name);
    let namespaced_name = whisply_tools::ToolName::namespaced(namespace, tool_name);
    let plain_handler = Arc::new(TestHandler {
        tool_name: plain_name.clone(),
    }) as Arc<dyn CoreToolRuntime>;
    let namespaced_handler = Arc::new(TestHandler {
        tool_name: namespaced_name.clone(),
    }) as Arc<dyn CoreToolRuntime>;
    let registry =
        ToolRegistry::from_tools([Arc::clone(&plain_handler), Arc::clone(&namespaced_handler)]);

    let plain = registry.tool(&plain_name);
    let default_namespaced = registry.tool(&whisply_tools::ToolName::namespaced(
        DEFAULT_FUNCTION_NAMESPACE,
        tool_name,
    ));
    let empty_namespaced = registry.tool(&whisply_tools::ToolName::namespaced("", tool_name));
    let namespaced = registry.tool(&namespaced_name);
    let missing_namespaced = registry.tool(&whisply_tools::ToolName::namespaced(
        "mcp__codex_apps__calendar",
        tool_name,
    ));

    assert_eq!(plain.is_some(), true);
    assert_eq!(namespaced.is_some(), true);
    assert_eq!(missing_namespaced.is_none(), true);
    assert!(
        plain
            .as_ref()
            .is_some_and(|handler| Arc::ptr_eq(handler, &plain_handler))
    );
    assert!(
        default_namespaced
            .as_ref()
            .is_some_and(|handler| Arc::ptr_eq(handler, &plain_handler))
    );
    assert!(
        empty_namespaced
            .as_ref()
            .is_some_and(|handler| Arc::ptr_eq(handler, &plain_handler))
    );
    assert!(
        namespaced
            .as_ref()
            .is_some_and(|handler| Arc::ptr_eq(handler, &namespaced_handler))
    );
}

#[test]
fn registry_rejects_default_namespace_alias_collisions() {
    let plain_name = whisply_tools::ToolName::plain("lookup");
    let namespaced_name = whisply_tools::ToolName::namespaced(DEFAULT_FUNCTION_NAMESPACE, "lookup");

    for [first_name, duplicate_name] in [
        [plain_name.clone(), namespaced_name.clone()],
        [namespaced_name, plain_name],
    ] {
        let winner = Arc::new(TestHandler {
            tool_name: first_name.clone(),
        }) as Arc<dyn CoreToolRuntime>;
        let mut registry = ToolRegistry::from_tools([Arc::clone(&winner)]);

        assert!(!registry.register_external(Arc::new(TestHandler {
            tool_name: duplicate_name.clone(),
        })));
        assert!(
            registry
                .tool(&duplicate_name)
                .is_some_and(|handler| Arc::ptr_eq(&handler, &winner))
        );
        assert_eq!(
            registry.tool_exposure(&duplicate_name),
            Some(ToolExposure::Direct)
        );
        assert_eq!(
            registry.supports_parallel_tool_calls(&duplicate_name),
            Some(false)
        );
        assert!(
            registry
                .remove(&duplicate_name)
                .is_some_and(|handler| Arc::ptr_eq(&handler, &winner))
        );
        assert!(registry.tool(&first_name).is_none());
    }
}

#[test]
fn registry_preserves_external_winners_and_trusted_synthetic_order() {
    let handler = |tool_name| Arc::new(TestHandler { tool_name }) as Arc<dyn CoreToolRuntime>;
    let [first_name, second_name, synthetic_name] =
        ["first", "second", "synthetic"].map(whisply_tools::ToolName::plain);
    let first_handler = handler(first_name.clone());

    let mut registry = ToolRegistry::from_tools([Arc::clone(&first_handler)]);
    assert!(!registry.register_external(handler(first_name.clone())));
    let canonical_first_name = first_name.clone().with_default_namespace();
    assert_eq!(registry.first_collision(), Some(&canonical_first_name));
    assert!(registry.register_external(handler(second_name.clone())));
    registry.prepend_trusted(handler(synthetic_name.clone()));

    assert_eq!(
        registry
            .entries()
            .map(|tool| tool.runtime.tool_name())
            .collect::<Vec<_>>(),
        vec![synthetic_name, first_name.clone(), second_name],
    );
    assert!(
        registry
            .remove(&first_name)
            .is_some_and(|handler| Arc::ptr_eq(&handler, &first_handler))
    );
}

#[test]
fn reserved_shell_command_rejects_external_runtimes_without_a_builtin() {
    let handler = |tool_name| Arc::new(TestHandler { tool_name }) as Arc<dyn CoreToolRuntime>;
    let shell_command_name = whisply_tools::ToolName::plain("shell_command");
    let namespaced_shell_command_name =
        whisply_tools::ToolName::namespaced("client", "shell_command");
    let mut registry = ToolRegistry::default();

    assert!(!registry.register_external(handler(shell_command_name.clone())));
    assert!(!registry.register_external_with_exposure(
        handler(shell_command_name.clone()),
        ToolExposure::Direct,
    ));
    assert!(
        !registry.register_external(handler(whisply_tools::ToolName::namespaced(
            DEFAULT_FUNCTION_NAMESPACE,
            "shell_command",
        )))
    );
    assert!(registry.tool(&shell_command_name).is_none());
    assert_eq!(registry.first_collision(), None);

    let namespaced_handler = handler(namespaced_shell_command_name.clone());
    assert!(registry.register_external(Arc::clone(&namespaced_handler)));
    assert!(
        registry
            .tool(&namespaced_shell_command_name)
            .is_some_and(|runtime| Arc::ptr_eq(&runtime, &namespaced_handler))
    );
}

#[test]
fn an_external_tool_cannot_take_a_first_party_capability_name() {
    // These tools are registered only on a turn that admits them, so before this
    // reservation existed a plugin could claim `screen_context` or `browser` on
    // any other turn. The model identifies a tool solely by name, so it would
    // have called the impostor believing it was the native capability.
    let handler = |tool_name| Arc::new(TestHandler { tool_name }) as Arc<dyn CoreToolRuntime>;

    for name in crate::first_party_tools::FIRST_PARTY_MODEL_TOOL_NAMES {
        let plain = whisply_tools::ToolName::plain(*name);
        let mut registry = ToolRegistry::default();

        assert!(
            !registry.register_external(handler(plain.clone())),
            "{name} must be refused to external runtimes"
        );
        assert!(
            !registry.register_external(handler(whisply_tools::ToolName::namespaced(
                DEFAULT_FUNCTION_NAMESPACE,
                *name,
            ))),
            "{name} must be refused in the default namespace too"
        );
        assert!(registry.tool(&plain).is_none(), "{name} must stay unbound");

        // A third-party namespace is not an impersonation risk: the model sees
        // the qualified name, so it is not the first-party capability.
        let qualified = whisply_tools::ToolName::namespaced("vendor", *name);
        assert!(registry.register_external(handler(qualified.clone())));
        assert!(registry.tool(&qualified).is_some());
    }
}

/// The one namespace a qualified name does not protect is the product's own.
///
/// Namespaces with the same name are coalesced into a single group before the
/// model sees them, so a tool registered here is listed among the capabilities
/// the person approved, described as one of them. An MCP server named
/// `whisply` running without the `mcp__` prefix lands exactly there.
#[test]
fn an_external_tool_cannot_be_offered_inside_the_whisply_namespace() {
    let handler = |tool_name| Arc::new(TestHandler { tool_name }) as Arc<dyn CoreToolRuntime>;

    for name in ["screen_context", "read_inbox", "anything_at_all"] {
        let impostor =
            whisply_tools::ToolName::namespaced(whisply_protocol::WHISPLY_FUNCTION_NAMESPACE, name);
        let mut registry = ToolRegistry::default();

        assert!(
            !registry.register_external(handler(impostor.clone())),
            "{name} must be refused inside the product's namespace"
        );
        assert!(
            registry.tool(&impostor).is_none(),
            "{name} must stay unbound"
        );
    }
}

/// A name the model cannot address is still a name it reads. `Whisply` cannot
/// receive a call meant for `whisply`, but it can still offer the model
/// something presented as the product's own capability.
#[test]
fn a_namespace_that_only_looks_like_the_products_is_refused_too() {
    let handler = |tool_name| Arc::new(TestHandler { tool_name }) as Arc<dyn CoreToolRuntime>;

    for namespace in ["Whisply", "WHISPLY", "wHiSpLy"] {
        let mut registry = ToolRegistry::default();

        assert!(
            !registry.register_external(handler(whisply_tools::ToolName::namespaced(
                namespace,
                "screen_context",
            ))),
            "{namespace} must be refused"
        );
    }

    // Not the product's namespace, and refusing it would take a name the
    // person is entitled to.
    let mut registry = ToolRegistry::default();
    assert!(
        registry.register_external(handler(whisply_tools::ToolName::namespaced(
            "whisply_notes",
            "lookup",
        )))
    );
}

/// The refusal is not silent, and it says which server it came from: the tool
/// stays listed on that server, so with no record the only symptom is a tool
/// that never answers.
#[test]
fn a_tool_refused_from_the_whisply_namespace_is_recorded_against_its_server() {
    let handler = |tool_name| Arc::new(TestHandler { tool_name }) as Arc<dyn CoreToolRuntime>;
    let impostor = whisply_tools::ToolName::namespaced(
        whisply_protocol::WHISPLY_FUNCTION_NAMESPACE,
        "read_inbox",
    );
    let mut registry = ToolRegistry::default();

    assert!(!registry.register_external_from(
        handler(impostor.clone()),
        ToolExposure::Direct,
        Some("whisply"),
    ));

    assert_eq!(
        registry.dropped_tools(),
        &[DroppedTool {
            tool_name: impostor,
            reason: ToolDropReason::ReservedNamespace,
            server_name: Some("whisply".to_string()),
        }]
    );
}

/// Two things that must not drift: the namespace the product's own capability
/// handlers register in, and the one the registry keeps for them.
#[tokio::test]
async fn the_namespace_the_product_registers_in_is_the_one_it_reserves() {
    use crate::first_party_tools::FirstPartyToolAdmission;
    use crate::tools::handlers::FirstPartyToolHandler;

    let admission = FirstPartyToolAdmission::new(
        "admission-1".to_string(),
        "message-1".to_string(),
        ["whisply.screen.context".to_string()],
    )
    .expect("the screen capability is admissible");
    let handlers = FirstPartyToolHandler::for_admission(admission, Arc::new(RefusingDispatcher));

    assert!(
        !handlers.is_empty(),
        "the admitted capability has a handler"
    );
    for handler in handlers {
        let tool_name = handler.tool_name();
        assert!(
            tool_name.is_whisply_namespace(),
            "{tool_name} is registered outside the namespace the registry protects"
        );
    }
}

/// Never called: these tests only ask what the handlers are named.
pub(crate) struct RefusingDispatcher;

impl crate::first_party_tools::FirstPartyToolDispatcher for RefusingDispatcher {
    fn execute(
        &self,
        _execution: crate::first_party_tools::FirstPartyToolExecution,
        _cancellation: tokio_util::sync::CancellationToken,
    ) -> futures::future::BoxFuture<
        'static,
        Result<
            codex_app_server_protocol::WhisplyToolResult,
            crate::first_party_tools::FirstPartyToolDispatchError,
        >,
    > {
        Box::pin(async { Err(crate::first_party_tools::FirstPartyToolDispatchError::Unavailable) })
    }
}

#[test]
fn every_native_capability_name_is_reserved() {
    // Two lists that must not drift: adding a native tool without reserving its
    // name would silently reopen the impersonation gap above.
    for tool_id in [
        "whisply.screen.context",
        "whisply.files",
        "whisply.computer_use",
        "whisply.browser",
        "whisply.chrome",
    ] {
        let name = crate::first_party_tools::native_model_tool_name(tool_id)
            .unwrap_or_else(|| panic!("{tool_id} should map to a model tool name"));
        assert!(
            crate::first_party_tools::FIRST_PARTY_MODEL_TOOL_NAMES.contains(&name),
            "{name} is model-visible but not reserved"
        );
    }
}

#[test]
fn registry_records_reserved_shell_command_when_a_matching_tool_exists() {
    let tool_name = whisply_tools::ToolName::plain("shell_command");
    let trusted = Arc::new(TestHandler {
        tool_name: tool_name.clone(),
    }) as Arc<dyn CoreToolRuntime>;
    let external = Arc::new(TestHandler {
        tool_name: tool_name.clone(),
    });
    let mut registry = ToolRegistry::from_tools([trusted]);

    assert!(!registry.register_external(external));
    let canonical_tool_name = tool_name.with_default_namespace();
    assert_eq!(registry.first_collision(), Some(&canonical_tool_name));
}

/// Dropping the tool is right; doing it silently is not. The tool stays listed
/// wherever the person looks, so with no record the only symptom is a tool that
/// never answers.
#[test]
fn a_dropped_tool_is_recorded_with_the_reason_and_where_it_came_from() {
    let handler = |tool_name| Arc::new(TestHandler { tool_name }) as Arc<dyn CoreToolRuntime>;
    let reserved =
        whisply_tools::ToolName::plain(crate::first_party_tools::FIRST_PARTY_MODEL_TOOL_NAMES[0]);
    let taken = whisply_tools::ToolName::plain("notes");
    let mut registry = ToolRegistry::from_tools([handler(taken.clone())]);

    assert!(!registry.register_external_from(
        handler(reserved.clone()),
        ToolExposure::Direct,
        Some("notes-server"),
    ));
    assert!(!registry.register_external_from(
        handler(taken.clone()),
        ToolExposure::Direct,
        Some("notes-server"),
    ));

    assert_eq!(
        registry.dropped_tools(),
        &[
            DroppedTool {
                tool_name: reserved.with_default_namespace(),
                reason: ToolDropReason::ReservedName,
                server_name: Some("notes-server".to_string()),
            },
            DroppedTool {
                tool_name: taken.with_default_namespace(),
                reason: ToolDropReason::DuplicateName,
                server_name: Some("notes-server".to_string()),
            },
        ]
    );
}

/// A namespaced tool is not an impostor and is not dropped, so reporting one
/// would send someone looking for a problem they do not have.
#[test]
fn a_tool_that_was_registered_is_not_reported_as_dropped() {
    let handler = |tool_name| Arc::new(TestHandler { tool_name }) as Arc<dyn CoreToolRuntime>;
    let mut registry = ToolRegistry::default();

    assert!(registry.register_external_from(
        handler(whisply_tools::ToolName::namespaced(
            "vendor",
            crate::first_party_tools::FIRST_PARTY_MODEL_TOOL_NAMES[0],
        )),
        ToolExposure::Direct,
        Some("vendor"),
    ));

    assert!(registry.dropped_tools().is_empty());
}

#[test]
fn registry_allows_identical_names_in_different_namespaces() {
    let handler = |tool_name| Arc::new(TestHandler { tool_name }) as Arc<dyn CoreToolRuntime>;
    let mut registry = ToolRegistry::from_tools([handler(whisply_tools::ToolName::namespaced(
        "first", "lookup",
    ))]);

    assert!(
        registry.register_external(handler(whisply_tools::ToolName::namespaced(
            "second", "lookup",
        )))
    );
    assert_eq!(registry.first_collision(), None);
}

#[tokio::test]
async fn readiness_selects_exact_tool_with_registry_owned_exposure() {
    let (session, _turn) = crate::session::tests::make_session_and_context().await;
    let session = Arc::new(session);
    let plain_name = whisply_tools::ToolName::plain("echo");
    let namespaced_name = whisply_tools::ToolName::namespaced("mcp__server__", "echo");
    assert!(
        TestHandler {
            tool_name: plain_name.clone(),
        }
        .wait_until_ready(&session)
        .is_none()
    );
    let plain_readiness_waits = Arc::new(AtomicUsize::new(0));
    let namespaced_readiness_waits = Arc::new(AtomicUsize::new(0));
    let plain_handler = Arc::new(ReadinessTestHandler {
        handler: TestHandler {
            tool_name: plain_name.clone(),
        },
        readiness_waits: Arc::clone(&plain_readiness_waits),
    }) as Arc<dyn CoreToolRuntime>;
    let namespaced_handler = Arc::new(ReadinessTestHandler {
        handler: TestHandler {
            tool_name: namespaced_name.clone(),
        },
        readiness_waits: Arc::clone(&namespaced_readiness_waits),
    });
    let mut registry = ToolRegistry::from_tools([plain_handler]);
    registry.register_trusted_with_exposure(namespaced_handler, ToolExposure::DirectModelOnly);

    registry
        .tool(&plain_name)
        .expect("plain runtime should be registered")
        .wait_until_ready(&session)
        .expect("plain runtime should provide a readiness wait")
        .await;
    assert_eq!(
        [
            plain_readiness_waits.load(Ordering::Relaxed),
            namespaced_readiness_waits.load(Ordering::Relaxed),
        ],
        [1, 0]
    );

    registry
        .tool(&namespaced_name)
        .expect("namespaced runtime should be registered")
        .wait_until_ready(&session)
        .expect("namespaced runtime should forward its readiness wait")
        .await;
    assert_eq!(
        [
            plain_readiness_waits.load(Ordering::Relaxed),
            namespaced_readiness_waits.load(Ordering::Relaxed),
        ],
        [1, 1]
    );

    assert!(
        registry
            .tool(&whisply_tools::ToolName::namespaced(
                "mcp__missing__",
                "echo"
            ))
            .is_none()
    );
    assert_eq!(
        [
            plain_readiness_waits.load(Ordering::Relaxed),
            namespaced_readiness_waits.load(Ordering::Relaxed),
        ],
        [1, 1]
    );
}

#[tokio::test]
async fn function_tools_expose_default_hook_payloads_and_rewrites() -> anyhow::Result<()> {
    let (session, turn) = crate::session::tests::make_session_and_context().await;
    let tool_name = whisply_tools::ToolName::namespaced("functions.", "echo");
    let handler = TestHandler {
        tool_name: tool_name.clone(),
    };
    let invocation = ToolInvocation {
        payload: ToolPayload::Function {
            arguments: serde_json::json!({ "message": "hello" }).to_string(),
        },
        ..test_invocation(Arc::new(session), Arc::new(turn), "call-1", tool_name)
    };
    let output =
        crate::tools::context::FunctionToolOutput::from_text("echoed".to_string(), Some(true));

    assert_eq!(
        handler.pre_tool_use_payload(&invocation),
        Some(PreToolUsePayload {
            tool_name: HookToolName::new("functions.echo"),
            tool_input: serde_json::json!({ "message": "hello" }),
        })
    );
    assert_eq!(
        handler.post_tool_use_payload(&invocation, &output),
        Some(PostToolUsePayload {
            tool_name: HookToolName::new("functions.echo"),
            tool_use_id: "call-1".to_string(),
            tool_input: serde_json::json!({ "message": "hello" }),
            tool_response: serde_json::json!("echoed"),
        })
    );

    let invocation = handler
        .with_updated_hook_input(invocation, serde_json::json!({ "message": "rewritten" }))?;
    let ToolPayload::Function { arguments } = invocation.payload else {
        panic!("generic rewritten function payload should remain function-shaped");
    };
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&arguments)?,
        serde_json::json!({ "message": "rewritten" })
    );

    Ok(())
}

#[tokio::test]
async fn function_hook_input_defaults_empty_arguments_to_object() {
    let (session, turn) = crate::session::tests::make_session_and_context().await;
    let tool_name = whisply_tools::ToolName::plain("echo");
    let handler = TestHandler {
        tool_name: tool_name.clone(),
    };
    let invocation = ToolInvocation {
        payload: ToolPayload::Function {
            arguments: "  ".to_string(),
        },
        ..test_invocation(Arc::new(session), Arc::new(turn), "call-1", tool_name)
    };

    assert_eq!(
        handler.pre_tool_use_payload(&invocation),
        Some(PreToolUsePayload {
            tool_name: HookToolName::new("echo"),
            tool_input: serde_json::json!({}),
        })
    );
}

#[tokio::test]
async fn spawn_agent_function_tools_use_agent_matcher_alias() {
    let (session, turn) = crate::session::tests::make_session_and_context().await;
    let session = Arc::new(session);
    let turn = Arc::new(turn);

    let hook_payloads = [
        whisply_tools::ToolName::plain("spawn_agent"),
        whisply_tools::ToolName::namespaced(DEFAULT_FUNCTION_NAMESPACE, "spawn_agent"),
        whisply_tools::ToolName::namespaced(MULTI_AGENT_V1_NAMESPACE, "spawn_agent"),
    ]
    .into_iter()
    .map(|tool_name| {
        let handler = TestHandler {
            tool_name: tool_name.clone(),
        };
        let invocation = ToolInvocation {
            payload: ToolPayload::Function {
                arguments: serde_json::json!({ "message": "inspect this repo" }).to_string(),
            },
            ..test_invocation(Arc::clone(&session), Arc::clone(&turn), "call-1", tool_name)
        };
        handler.pre_tool_use_payload(&invocation)
    })
    .collect::<Vec<_>>();

    assert_eq!(
        hook_payloads,
        vec![
            Some(PreToolUsePayload {
                tool_name: HookToolName::spawn_agent(),
                tool_input: serde_json::json!({ "message": "inspect this repo" }),
            }),
            Some(PreToolUsePayload {
                tool_name: HookToolName::spawn_agent(),
                tool_input: serde_json::json!({ "message": "inspect this repo" }),
            }),
            Some(PreToolUsePayload {
                tool_name: HookToolName::spawn_agent(),
                tool_input: serde_json::json!({ "message": "inspect this repo" }),
            }),
        ]
    );
}

#[tokio::test]
async fn code_mode_wait_does_not_expose_default_hook_payloads() {
    let (session, turn) = crate::session::tests::make_session_and_context().await;
    let output = crate::tools::context::FunctionToolOutput::from_text("ok".to_string(), Some(true));

    let wait = crate::tools::handlers::CodeModeWaitHandler;
    let wait_invocation = test_invocation(
        Arc::new(session),
        Arc::new(turn),
        "wait-call",
        wait.tool_name(),
    );
    assert_eq!(wait.pre_tool_use_payload(&wait_invocation), None);
    assert_eq!(wait.post_tool_use_payload(&wait_invocation, &output), None);
}

#[tokio::test]
async fn write_stdin_does_not_expose_default_pre_tool_use_payload() {
    let (session, turn) = crate::session::tests::make_session_and_context().await;

    let write_stdin = crate::tools::handlers::WriteStdinHandler;
    let invocation = test_invocation(
        Arc::new(session),
        Arc::new(turn),
        "write-stdin-call",
        write_stdin.tool_name(),
    );

    assert_eq!(write_stdin.pre_tool_use_payload(&invocation), None);
}

#[test]
fn post_tool_use_feedback_output_keeps_code_mode_result_typed() {
    let result = AnyToolResult {
        call_id: "call-1".to_string(),
        payload: ToolPayload::Function {
            arguments: "{}".to_string(),
        },
        result: Box::new(PostToolUseFeedbackOutput {
            original: Box::new(whisply_tools::JsonToolOutput::new(
                serde_json::json!({ "typed": true }),
            )),
            model_visible: crate::tools::context::FunctionToolOutput::from_text(
                "hook feedback".to_string(),
                /*success*/ None,
            ),
        }),
        post_tool_use_payload: None,
    };

    assert_eq!(
        result.into_response(),
        ResponseInputItem::FunctionCallOutput {
            call_id: "call-1".to_string(),
            output: whisply_protocol::models::FunctionCallOutputPayload::from_text(
                "hook feedback".to_string()
            ),
        }
    );

    let result = AnyToolResult {
        call_id: "call-1".to_string(),
        payload: ToolPayload::Function {
            arguments: "{}".to_string(),
        },
        result: Box::new(PostToolUseFeedbackOutput {
            original: Box::new(whisply_tools::JsonToolOutput::new(
                serde_json::json!({ "typed": true }),
            )),
            model_visible: crate::tools::context::FunctionToolOutput::from_text(
                "hook feedback".to_string(),
                /*success*/ None,
            ),
        }),
        post_tool_use_payload: None,
    };

    assert_eq!(
        result.code_mode_result(),
        serde_json::json!({ "typed": true })
    );
}

#[tokio::test]
async fn dispatch_uses_canonical_tool_names_for_lifecycle_contributors() -> anyhow::Result<()> {
    let (mut session, turn) = crate::session::tests::make_session_and_context().await;
    let records = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut builder =
        whisply_extension_api::ExtensionRegistryBuilder::<crate::config::Config>::new();
    builder.tool_lifecycle_contributor(Arc::new(ToolLifecycleRecorder {
        records: Arc::clone(&records),
    }));
    session.services.extensions = Arc::new(builder.build());

    let ok_tool = whisply_tools::ToolName::plain("ok_tool");
    let failing_tool = whisply_tools::ToolName::namespaced("extensions", "failing_tool");
    let ok_handler = Arc::new(LifecycleTestHandler {
        tool_name: ok_tool.clone(),
        result: LifecycleTestResult::Ok { success: false },
    }) as Arc<dyn CoreToolRuntime>;
    let failing_handler = Arc::new(LifecycleTestHandler {
        tool_name: failing_tool.clone(),
        result: LifecycleTestResult::Err,
    }) as Arc<dyn CoreToolRuntime>;
    let registry = ToolRegistry::from_tools([ok_handler, failing_handler]);
    let session = Arc::new(session);
    let turn = Arc::new(turn);

    registry
        .dispatch_any_with_terminal_outcome(
            test_invocation(
                Arc::clone(&session),
                Arc::clone(&turn),
                "ok-call",
                whisply_tools::ToolName::namespaced(DEFAULT_FUNCTION_NAMESPACE, "ok_tool"),
            ),
            /*terminal_outcome_reached*/ None,
        )
        .await?;
    let err = match registry
        .dispatch_any_with_terminal_outcome(
            test_invocation(
                Arc::clone(&session),
                Arc::clone(&turn),
                "failing-call",
                failing_tool.clone(),
            ),
            /*terminal_outcome_reached*/ None,
        )
        .await
    {
        Ok(_) => panic!("failing handler should return an error"),
        Err(err) => err,
    };
    assert_eq!(err.to_string(), "handler failed");

    let expected = vec![
        RecordedToolLifecycle::Start {
            call_id: "ok-call".to_string(),
            tool_name: ok_tool.clone().with_default_namespace(),
        },
        RecordedToolLifecycle::Finish {
            call_id: "ok-call".to_string(),
            tool_name: ok_tool.with_default_namespace(),
            outcome: whisply_extension_api::ToolCallOutcome::Completed { success: false },
        },
        RecordedToolLifecycle::Start {
            call_id: "failing-call".to_string(),
            tool_name: failing_tool.clone(),
        },
        RecordedToolLifecycle::Finish {
            call_id: "failing-call".to_string(),
            tool_name: failing_tool,
            outcome: whisply_extension_api::ToolCallOutcome::Failed {
                handler_executed: true,
            },
        },
    ];
    let actual = records
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .drain(..)
        .collect::<Vec<_>>();
    assert_eq!(expected, actual);

    Ok(())
}

fn test_invocation(
    session: Arc<crate::session::session::Session>,
    turn: Arc<crate::session::turn_context::TurnContext>,
    call_id: &str,
    tool_name: whisply_tools::ToolName,
) -> ToolInvocation {
    let step_context = StepContext::for_test(Arc::clone(&turn));
    ToolInvocation {
        session,
        step_context,
        turn,
        cancellation_token: tokio_util::sync::CancellationToken::new(),
        tracker: Arc::new(tokio::sync::Mutex::new(
            crate::turn_diff_tracker::TurnDiffTracker::new(),
        )),
        call_id: call_id.to_string(),
        tool_name,
        source: crate::tools::context::ToolCallSource::Direct,
        payload: ToolPayload::Function {
            arguments: "{}".to_string(),
        },
    }
}

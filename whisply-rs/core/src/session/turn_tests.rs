use super::*;
use pretty_assertions::assert_eq;
use std::sync::Arc;
use whisply_extension_api::ExtensionData;
use whisply_extension_api::TurnItemContributor;
use whisply_protocol::ResponseItemId;
use whisply_protocol::items::AgentMessageContent;

struct RewriteAgentMessageContributor;

impl TurnItemContributor for RewriteAgentMessageContributor {
    fn contribute<'a>(
        &'a self,
        _thread_store: &'a ExtensionData,
        _turn_store: &'a ExtensionData,
        item: &'a mut TurnItem,
    ) -> whisply_extension_api::ExtensionFuture<'a, Result<(), String>> {
        Box::pin(async move {
            if let TurnItem::AgentMessage(agent_message) = item {
                agent_message.content = vec![AgentMessageContent::Text {
                    text: "plan contributed assistant text".to_string(),
                }];
            }
            Ok(())
        })
    }
}

fn assistant_output_text(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: Some(ResponseItemId::with_suffix("msg", "1")),
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: text.to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }
}

#[test]
fn post_sampling_token_estimate_is_excluded_from_local_log_database() {
    assert!(
        !whisply_state::log_db::default_filter()
            .would_enable(POST_SAMPLING_TOKEN_ESTIMATE_TARGET, &tracing::Level::TRACE,)
    );
}

#[tokio::test]
async fn plan_mode_uses_contributed_turn_item_for_last_agent_message() {
    let (mut session, turn_context) = crate::session::tests::make_session_and_context().await;
    let mut builder = whisply_extension_api::ExtensionRegistryBuilder::new();
    builder.turn_item_contributor(Arc::new(RewriteAgentMessageContributor));
    session.services.extensions = Arc::new(builder.build());
    let turn_store = ExtensionData::new(turn_context.sub_id.clone());
    let mut state = PlanModeStreamState::new(&turn_context.sub_id);
    let mut last_agent_message = None;
    let item = assistant_output_text("original assistant text");

    let handled = handle_assistant_item_done_in_plan_mode(
        &session,
        &turn_context,
        &turn_store,
        &item,
        &mut state,
        /*previously_active_item*/ None,
        &mut last_agent_message,
    )
    .await;

    assert!(handled);
    assert_eq!(
        last_agent_message.as_deref(),
        Some("plan contributed assistant text")
    );
}

/// The line has to say which tool, where it came from, and what to change; a
/// warning that only says a tool was dropped leaves the same guessing behind.
#[test]
fn the_dropped_tool_line_names_the_tool_the_server_and_the_fix() {
    let message = dropped_tool_message(&DroppedTool {
        tool_name: whisply_protocol::ToolName::plain("browser"),
        reason: ToolDropReason::ReservedName,
        server_name: Some("notes".to_string()),
    });

    assert!(message.contains("`browser`"), "{message}");
    assert!(message.contains("MCP server `notes`"), "{message}");
    assert!(message.contains("built-in capability"), "{message}");
    assert!(
        message.contains("non_prefixed_mcp_tool_servers"),
        "{message}"
    );
}

/// A server that reached the product's namespace has a different problem and a
/// different fix than one that took a product name, and the setting named in
/// the other line has nothing to do with it.
#[test]
fn a_tool_refused_from_the_products_namespace_is_told_to_rename_the_server() {
    let message = dropped_tool_message(&DroppedTool {
        tool_name: whisply_protocol::ToolName::namespaced(
            whisply_protocol::WHISPLY_FUNCTION_NAMESPACE,
            "read_inbox",
        ),
        reason: ToolDropReason::ReservedNamespace,
        server_name: Some("whisply".to_string()),
    });

    assert!(message.contains("MCP server `whisply`"), "{message}");
    assert!(message.contains("`whisply` is the namespace"), "{message}");
    assert!(message.contains("Rename the server"), "{message}");
    assert!(
        !message.contains("non_prefixed_mcp_tool_servers"),
        "removing it from that list is not what unblocks this: {message}"
    );
}

/// A duplicate is not an impersonation, and saying it is would send someone
/// looking at the wrong tool.
#[test]
fn a_duplicate_is_not_described_as_a_built_in() {
    let message = dropped_tool_message(&DroppedTool {
        tool_name: whisply_protocol::ToolName::plain("notes"),
        reason: ToolDropReason::DuplicateName,
        server_name: None,
    });

    assert!(
        message.contains("another tool already answers"),
        "{message}"
    );
    assert!(!message.contains("built-in"), "{message}");
    assert!(message.contains("a configured extension"), "{message}");
}

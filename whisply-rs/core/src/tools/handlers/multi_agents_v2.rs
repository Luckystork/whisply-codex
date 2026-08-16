//! Implements the MultiAgentV2 collaboration tool surface.

use crate::agent::AgentStatus;
use crate::agent::agent_resolver::resolve_agent_target;
use crate::context::ContextualUserFragment;
use crate::context::InterAgentMessage;
use crate::context::InterAgentMessageType;
use crate::function_tool::FunctionCallError;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;
use crate::tools::context::boxed_tool_output;
use crate::tools::handlers::multi_agents_common::*;
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::CoreToolRuntime;
use crate::tools::registry::ToolExecutor;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value as JsonValue;
use whisply_protocol::AgentPath;
use whisply_protocol::items::CollabAgentTool;
use whisply_protocol::items::CollabAgentToolCallItem;
use whisply_protocol::items::CollabAgentToolCallStatus;
use whisply_protocol::items::SubAgentActivityItem;
use whisply_protocol::items::TurnItem;
use whisply_protocol::models::ResponseInputItem;
use whisply_protocol::openai_models::ReasoningEffort;
use whisply_protocol::protocol::InterAgentCommunication;
use whisply_protocol::protocol::SubAgentActivityKind;
use whisply_tools::ToolName;

pub(crate) use followup_task::Handler as FollowupTaskHandler;
pub(crate) use interrupt_agent::Handler as InterruptAgentHandler;
pub(crate) use list_agents::Handler as ListAgentsHandler;
pub(crate) use send_message::Handler as SendMessageHandler;
pub(crate) use spawn::Handler as SpawnAgentHandler;
pub(crate) use wait::Handler as WaitAgentHandler;

mod followup_task;
mod interrupt_agent;
mod list_agents;
mod message_tool;
mod send_message;
mod spawn;
pub(crate) mod wait;

pub(crate) async fn emit_sub_agent_activity(
    session: &crate::session::session::Session,
    turn: &crate::session::turn_context::TurnContext,
    item: SubAgentActivityItem,
) {
    let item = TurnItem::SubAgentActivity(item);
    session.emit_turn_item_started(turn, &item).await;
    session.emit_turn_item_completed(turn, item).await;
}

fn communication_from_tool_message(
    author: AgentPath,
    recipient: AgentPath,
    message: String,
    source: &crate::tools::context::ToolCallSource,
    trigger_turn: bool,
) -> InterAgentCommunication {
    if !matches!(
        source,
        crate::tools::context::ToolCallSource::DirectPlaintextMessage
    ) {
        return InterAgentCommunication::new_encrypted(
            author,
            recipient,
            Vec::new(),
            message,
            trigger_turn,
        );
    }
    let message_type = if trigger_turn {
        InterAgentMessageType::NewTask
    } else {
        InterAgentMessageType::Message
    };
    let content =
        InterAgentMessage::new(message_type, recipient.clone(), author.clone(), message).render();
    InterAgentCommunication::new(author, recipient, Vec::new(), content, trigger_turn)
}

use serde::Deserialize;
use serde::Serialize;
use serde_json::Value as JsonValue;
use std::collections::HashMap;
use ts_rs::TS;
use whisply_protocol::models::WebSearchAction;

/// Top-level JSONL events emitted by codex exec
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(tag = "type")]
pub enum ThreadEvent {
    /// Emitted when a new thread is started as the first event.
    #[serde(rename = "thread.started")]
    ThreadStarted(ThreadStartedEvent),
    /// Emitted when a turn is started by sending a new prompt to the model.
    /// A turn encompasses all events that happen while agent is processing the prompt.
    #[serde(rename = "turn.started")]
    TurnStarted(TurnStartedEvent),
    /// Emitted when a turn is completed. Typically right after the assistant's response.
    #[serde(rename = "turn.completed")]
    TurnCompleted(TurnCompletedEvent),
    /// Indicates that a turn failed with an error.
    #[serde(rename = "turn.failed")]
    TurnFailed(TurnFailedEvent),
    /// Emitted when a new item is added to the thread. Typically the item will be in an "in progress" state.
    #[serde(rename = "item.started")]
    ItemStarted(ItemStartedEvent),
    /// Emitted when an item is updated.
    #[serde(rename = "item.updated")]
    ItemUpdated(ItemUpdatedEvent),
    /// Signals that an item has reached a terminal state—either success or failure.
    #[serde(rename = "item.completed")]
    ItemCompleted(ItemCompletedEvent),
    /// Represents an unrecoverable error emitted directly by the event stream.
    #[serde(rename = "error")]
    Error(ThreadErrorEvent),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
pub struct ThreadStartedEvent {
    /// The identified of the new thread. Can be used to resume the thread later.
    pub thread_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, Default)]

pub struct TurnStartedEvent {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
pub struct TurnCompletedEvent {
    pub usage: Usage,
    /// What the account was charged, for an account Whisply meters. Absent for
    /// a local or bring-your-own-key run, which Whisply does not meter, and for
    /// a metered run whose account could not be read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_usage: Option<AccountUsage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
pub struct TurnFailedEvent {
    pub error: ThreadErrorEvent,
}

/// Describes the usage of tokens during a turn.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, Default)]
pub struct Usage {
    /// The number of input tokens used during the turn.
    pub input_tokens: i64,
    /// The number of cached input tokens used during the turn.
    pub cached_input_tokens: i64,
    /// The number of input tokens written to the prompt cache during the turn.
    #[serde(default)]
    pub cache_write_input_tokens: i64,
    /// The number of output tokens used during the turn.
    pub output_tokens: i64,
    /// The number of reasoning output tokens used during the turn.
    pub reasoning_output_tokens: i64,
}

/// What a Whisply-metered account has spent, in the cost units it is billed in.
///
/// Cost already has each model's rate multiplier applied. The multiplier table
/// itself is signed and is identified here by `rate_card_version`; it is served
/// by `whisply usage --json` and `whisply models --json` rather than copied,
/// unsigned, into every turn event.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
pub struct AccountUsage {
    /// The account's plan tier.
    pub tier: String,
    /// What the amounts below measure, as declared by the metering contract.
    pub basis: String,
    /// The currency the amounts are denominated in.
    pub currency: String,
    /// The revision of the signed rate card these amounts were priced against.
    pub rate_card_version: String,
    /// Every limit window the account has, spend and transcription alike.
    pub windows: Vec<AccountUsageWindow>,
    /// When the account produced these amounts.
    pub generated_at: String,
    /// Whether the amounts are known to be behind the account's live state.
    pub stale: bool,
}

/// One of the account's limit windows.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
pub struct AccountUsageWindow {
    /// What the window limits: `usage` or `transcription`.
    pub category: String,
    /// The window's period: `five_hour` or `weekly`.
    pub window: String,
    /// Cost already charged in this window.
    pub settled: f64,
    /// Cost held for work that has not settled yet.
    pub reserved: f64,
    /// The window's ceiling.
    pub cap: f64,
    /// Settled plus reserved cost as a fraction of the cap.
    pub used_fraction: f64,
    /// When the window next resets, if a reset is scheduled.
    #[ts(type = "string | null")]
    pub resets_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
pub struct ItemStartedEvent {
    pub item: ThreadItem,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
pub struct ItemCompletedEvent {
    pub item: ThreadItem,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
pub struct ItemUpdatedEvent {
    pub item: ThreadItem,
}

/// Fatal error emitted by the stream.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
pub struct ThreadErrorEvent {
    pub message: String,
}

/// Canonical representation of a thread item and its domain-specific payload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
pub struct ThreadItem {
    pub id: String,
    #[serde(flatten)]
    pub details: ThreadItemDetails,
}

/// Typed payloads for each supported thread item type.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ThreadItemDetails {
    /// Response from the agent.
    /// Either a natural-language response or a JSON string when structured output is requested.
    AgentMessage(AgentMessageItem),
    /// Agent's reasoning summary.
    Reasoning(ReasoningItem),
    /// Tracks a command executed by the agent. The item starts when the command is
    /// spawned, and completes when the process exits with an exit code.
    CommandExecution(CommandExecutionItem),
    /// Represents a set of file changes by the agent. The item is emitted only as a
    /// completed event once the patch succeeds or fails.
    FileChange(FileChangeItem),
    /// Represents a call to an MCP tool. The item starts when the invocation is
    /// dispatched and completes when the MCP server reports success or failure.
    McpToolCall(McpToolCallItem),
    /// Represents a call to a collab tool. The item starts when the collab tool is
    /// invoked and completes when the collab tool reports success or failure.
    CollabToolCall(CollabToolCallItem),
    /// Captures a web search request. It starts when the search is kicked off
    /// and completes when results are returned to the agent.
    WebSearch(WebSearchItem),
    /// Tracks the agent's running to-do list. It starts when the plan is first
    /// issued, updates as steps change state, and completes when the turn ends.
    TodoList(TodoListItem),
    /// Describes a non-fatal error surfaced as an item.
    Error(ErrorItem),
}

/// Response from the agent.
/// Either a natural-language response or a JSON string when structured output is requested.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
pub struct AgentMessageItem {
    pub text: String,
}

/// Agent's reasoning summary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
pub struct ReasoningItem {
    pub text: String,
}

/// The status of a command execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default, TS)]
#[serde(rename_all = "snake_case")]
pub enum CommandExecutionStatus {
    #[default]
    InProgress,
    Completed,
    Failed,
    Declined,
}

/// A command executed by the agent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
pub struct CommandExecutionItem {
    pub command: String,
    pub aggregated_output: String,
    pub exit_code: Option<i32>,
    pub status: CommandExecutionStatus,
}

/// A set of file changes by the agent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
pub struct FileUpdateChange {
    pub path: String,
    pub kind: PatchChangeKind,
}

/// The status of a file change.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(rename_all = "snake_case")]
pub enum PatchApplyStatus {
    InProgress,
    Completed,
    Failed,
}

/// A set of file changes by the agent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
pub struct FileChangeItem {
    pub changes: Vec<FileUpdateChange>,
    pub status: PatchApplyStatus,
}

/// Indicates the type of the file change.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(rename_all = "snake_case")]
pub enum PatchChangeKind {
    Add,
    Delete,
    Update,
}

/// The status of an MCP tool call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default, TS)]
#[serde(rename_all = "snake_case")]
pub enum McpToolCallStatus {
    #[default]
    InProgress,
    Completed,
    Failed,
}

/// The status of a collab tool call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default, TS)]
#[serde(rename_all = "snake_case")]
pub enum CollabToolCallStatus {
    #[default]
    InProgress,
    Completed,
    Failed,
}

/// Supported collab tools.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
pub enum CollabTool {
    SpawnAgent,
    SendInput,
    Wait,
    CloseAgent,
}

/// The status of a collab agent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
pub enum CollabAgentStatus {
    PendingInit,
    Running,
    Interrupted,
    Completed,
    Errored,
    Shutdown,
    NotFound,
}

/// Last known state of a collab agent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS)]
pub struct CollabAgentState {
    pub status: CollabAgentStatus,
    pub message: Option<String>,
}

/// A call to a collab tool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
pub struct CollabToolCallItem {
    pub tool: CollabTool,
    pub sender_thread_id: String,
    pub receiver_thread_ids: Vec<String>,
    pub prompt: Option<String>,
    pub agents_states: HashMap<String, CollabAgentState>,
    pub status: CollabToolCallStatus,
}

/// Result payload produced by an MCP tool invocation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
pub struct McpToolCallItemResult {
    // NOTE: `rmcp::model::Content` (and its `RawContent` variants) would be a
    // more precise Rust representation of MCP content blocks. We intentionally
    // use `serde_json::Value` here because this crate exports JSON schema + TS
    // types (`schemars`/`ts-rs`), and the rmcp model types aren't set up to be
    // schema/TS friendly (and would introduce heavier coupling to rmcp's Rust
    // representations). Using `JsonValue` keeps the payload wire-shaped and
    // easy to export.
    pub content: Vec<JsonValue>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub meta: Option<JsonValue>,
    pub structured_content: Option<JsonValue>,
}

/// Error details reported by a failed MCP tool invocation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
pub struct McpToolCallItemError {
    pub message: String,
}

/// A call to an MCP tool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
pub struct McpToolCallItem {
    pub server: String,
    pub tool: String,
    #[serde(default)]
    pub arguments: JsonValue,
    pub result: Option<McpToolCallItemResult>,
    pub error: Option<McpToolCallItemError>,
    pub status: McpToolCallStatus,
}

/// A web search request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
pub struct WebSearchItem {
    pub id: String,
    pub query: String,
    pub action: WebSearchAction,
}

/// An error notification.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
pub struct ErrorItem {
    pub message: String,
}

/// An item in agent's to-do list.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
pub struct TodoItem {
    pub text: String,
    pub completed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
pub struct TodoListItem {
    pub items: Vec<TodoItem>,
}

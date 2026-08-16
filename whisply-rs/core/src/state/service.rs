use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use crate::HostSkillsService;
use crate::agent::AgentControl;
use crate::agents_md_manager::AgentsMdManager;
use crate::attestation::AttestationProvider;
use crate::client::ModelClient;
use crate::config::NetworkProxyAuditMetadata;
use crate::config::StartedNetworkProxy;
use crate::current_time::TimeProvider;
use crate::elicitation::ElicitationService;
use crate::environment_selection::ThreadEnvironments;
use crate::exec_policy::ExecPolicyManager;
use crate::first_party_tools::FirstPartyToolDispatcher;
use crate::guardian::GuardianRejectionCircuitBreaker;
use crate::mcp::McpManager;
use crate::server_tools::ServerToolDispatcher;
use crate::tools::ExecutedToolCallRecorder;
use crate::tools::code_mode::CodeModeService;
use crate::tools::handlers::ToolSearchHandlerCache;
use crate::tools::network_approval::NetworkApprovalService;
use crate::tools::sandboxing::ApprovalStore;
use crate::unified_exec::UnifiedExecProcessManager;
use arc_swap::ArcSwap;
use arc_swap::ArcSwapOption;
use codex_whisply::ManagedGatewayClient;
use tokio::runtime::Handle;
use tokio::sync::Mutex;
use whisply_analytics::AnalyticsEventsClient;
use whisply_core_plugins::PluginsManager;
use whisply_extension_api::ExtensionData;
use whisply_extension_api::ExtensionDataInit;
use whisply_extension_api::ExtensionRegistry;
use whisply_hooks::Hooks;
use whisply_http_client::RouteAwareClientPool;
use whisply_login::AuthManager;
use whisply_mcp::McpRuntime;
use whisply_models_manager::manager::SharedModelsManager;
use whisply_otel::SessionTelemetry;
use whisply_protocol::capabilities::SelectedCapabilityRoot;
use whisply_protocol::mcp::ClientMcpExtensions;
use whisply_rollout::state_db::StateDbHandle;
use whisply_rollout_trace::ThreadTraceContext;
use whisply_thread_store::LiveThread;
use whisply_thread_store::ThreadStore;

pub(crate) struct SessionServices {
    /// The single owner of live MCP connections for this thread.
    pub(crate) mcp_runtime: Arc<McpRuntime>,
    pub(crate) unified_exec_manager: UnifiedExecProcessManager,
    pub(crate) elicitations: ElicitationService,
    #[cfg_attr(not(unix), allow(dead_code))]
    pub(crate) shell_zsh_path: Option<PathBuf>,
    #[cfg_attr(not(unix), allow(dead_code))]
    pub(crate) main_execve_wrapper_exe: Option<PathBuf>,
    pub(crate) analytics_events_client: AnalyticsEventsClient,
    pub(crate) hooks: ArcSwap<Hooks>,
    pub(crate) rollout_thread_trace: ThreadTraceContext,
    pub(crate) user_shell: Arc<crate::shell::Shell>,
    pub(crate) show_raw_agent_reasoning: bool,
    pub(crate) exec_policy: Arc<ExecPolicyManager>,
    pub(crate) auth_manager: Arc<AuthManager>,
    /// Typed embedded-runtime gateway; ordinary sessions leave this unset.
    pub(crate) managed_gateway_client: Option<Arc<ManagedGatewayClient>>,
    /// Owner-authenticated dispatcher for explicitly admitted native tools.
    /// Ordinary sessions leave this unset, so static registry entries alone
    /// never expose first-party capabilities to the model.
    pub(crate) first_party_tool_dispatcher: Option<Arc<dyn FirstPartyToolDispatcher>>,
    /// Dispatcher for server-owned first-party tools routed through the Mac client.
    pub(crate) server_tool_dispatcher: Option<Arc<dyn ServerToolDispatcher>>,
    /// Upload-only clients shared across turns without logging signed blob URLs.
    pub(crate) openai_file_upload_client_pool: RouteAwareClientPool,
    pub(crate) models_manager: SharedModelsManager,
    pub(crate) session_telemetry: SessionTelemetry,
    pub(crate) tool_approvals: Mutex<ApprovalStore>,
    /// Tools whose absence has already been explained to the person. The drop
    /// is decided again on every turn, so without this the same line would be
    /// repeated until it stopped being read.
    pub(crate) reported_dropped_tools: Mutex<HashSet<String>>,
    pub(crate) guardian_rejection_circuit_breaker: Mutex<GuardianRejectionCircuitBreaker>,
    pub(crate) runtime_handle: Handle,
    pub(crate) skills_service: Arc<HostSkillsService>,
    pub(crate) agents_md_manager: Arc<AgentsMdManager>,
    pub(crate) plugins_manager: Arc<PluginsManager>,
    pub(crate) mcp_manager: Arc<McpManager>,
    pub(crate) extensions: Arc<ExtensionRegistry<crate::config::Config>>,
    pub(crate) session_extension_data: ExtensionData,
    pub(crate) thread_extension_data: ExtensionData,
    /// MCP extensions fixed when this session is created.
    pub(crate) client_mcp_extensions: ClientMcpExtensions,
    /// Raw capability selections for this thread. Each model step resolves them against its
    /// current executor environments before using them.
    pub(crate) selected_capability_roots: Vec<SelectedCapabilityRoot>,
    pub(crate) mcp_thread_init: ExtensionDataInit,
    pub(crate) agent_control: AgentControl,
    pub(crate) network_proxy: ArcSwapOption<StartedNetworkProxy>,
    pub(crate) network_proxy_audit_metadata: NetworkProxyAuditMetadata,
    pub(crate) managed_network_requirements_configured: bool,
    pub(crate) network_approval: Arc<NetworkApprovalService>,
    pub(crate) state_db: Option<StateDbHandle>,
    pub(crate) live_thread: Option<LiveThread>,
    pub(crate) thread_store: Arc<dyn ThreadStore>,
    pub(crate) attestation_provider: Option<Arc<dyn AttestationProvider>>,
    pub(crate) time_provider: Arc<dyn TimeProvider>,
    /// Session-scoped model client shared across turns.
    pub(crate) model_client: ModelClient,
    pub(crate) executed_tool_calls: Option<Arc<ExecutedToolCallRecorder>>,
    pub(crate) code_mode_service: CodeModeService,
    pub(crate) tool_search_handler_cache: ToolSearchHandlerCache,
    pub(crate) turn_environments: Arc<ThreadEnvironments>,
}

use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use whisply_exec_server::EnvironmentManager;
use whisply_exec_server::ExecServerRuntimePaths;
use whisply_extension_api::ExtensionRegistry;
use whisply_extension_api::UserInstructionsProvider;
use whisply_login::AuthManager;
use whisply_protocol::error::CodexErr;
use whisply_protocol::error::Result as CodexResult;
use whisply_protocol::models::ResponseItem;
use whisply_protocol::protocol::SessionSource;
use whisply_protocol::user_input::UserInput;

use crate::config::Config;
use crate::resolve_installation_id;
use crate::session::session::Session;
use crate::session::turn::build_prompt;
use crate::state_db_bridge::StateDbHandle;
use crate::thread_manager::StartThreadOptions;
use crate::thread_manager::ThreadManager;
use crate::thread_manager::thread_store_from_config;

/// Build the model-visible `input` list for a single debug turn.
#[doc(hidden)]
pub async fn build_prompt_input(
    config: Config,
    input: Vec<UserInput>,
    state_db: Option<StateDbHandle>,
    extensions: Arc<ExtensionRegistry<Config>>,
    user_instructions_provider: Arc<dyn UserInstructionsProvider>,
) -> CodexResult<Vec<ResponseItem>> {
    build_debug_prompt(
        config,
        input,
        state_db,
        extensions,
        user_instructions_provider,
    )
    .await
    .map(|prompt| prompt.input)
}

/// Build the layered-composition diagnostics for a single debug turn.
///
/// This runs the same assembly as [`build_prompt_input`] and then observes the
/// result, so the reported layers describe the prompt that would actually be
/// sent rather than a separately constructed approximation.
#[doc(hidden)]
pub async fn build_prompt_composition_diagnostics(
    config: Config,
    input: Vec<UserInput>,
    state_db: Option<StateDbHandle>,
    extensions: Arc<ExtensionRegistry<Config>>,
    user_instructions_provider: Arc<dyn UserInstructionsProvider>,
    // Only the caller knows whether the working directory was explicitly
    // chosen. The contract activates the workspace profile on that choice
    // alone, never on a launcher cwd, so this is not inferred from `config`.
    workspace_selected: bool,
) -> CodexResult<codex_whisply::PromptCompositionDiagnostics> {
    let prompt = build_debug_prompt(
        config,
        input,
        state_db,
        extensions,
        user_instructions_provider,
    )
    .await?;
    crate::prompt_layers::prompt_composition_diagnostics(&prompt, workspace_selected)
        .map_err(|err| CodexErr::Fatal(err.to_string()))
}

async fn build_debug_prompt(
    mut config: Config,
    input: Vec<UserInput>,
    state_db: Option<StateDbHandle>,
    extensions: Arc<ExtensionRegistry<Config>>,
    user_instructions_provider: Arc<dyn UserInstructionsProvider>,
) -> CodexResult<crate::client_common::Prompt> {
    config.ephemeral = true;

    let auth_manager =
        AuthManager::shared_from_config(&config, /*enable_codex_api_key_env*/ false).await;

    let local_runtime_paths = ExecServerRuntimePaths::from_optional_paths(
        config.codex_self_exe.clone(),
        config.codex_linux_sandbox_exe.clone(),
    )?;

    let thread_store = thread_store_from_config(&config, state_db.clone());
    let installation_id = resolve_installation_id(&config.codex_home).await?;
    let thread_manager = ThreadManager::new(
        &config,
        Arc::clone(&auth_manager),
        crate::thread_manager::build_models_manager(&config, Arc::clone(&auth_manager)),
        crate::CodexAppsToolsCache::default(),
        SessionSource::Exec,
        Arc::new(
            EnvironmentManager::from_codex_home(
                config.codex_home.clone(),
                Some(local_runtime_paths),
                config.http_client_factory(),
            )
            .await
            .map_err(|err| CodexErr::Fatal(err.to_string()))?,
        ),
        extensions,
        user_instructions_provider,
        /*analytics_events_client*/ None,
        thread_store,
        crate::local_agent_graph_store_from_state_db(state_db.as_ref()),
        installation_id,
        /*attestation_provider*/ None,
        /*external_time_provider*/ None,
    );
    let thread = thread_manager
        .start_thread(StartThreadOptions::new(config))
        .await?;

    let output = build_prompt_input_from_session(&thread.thread.session, input).await;
    let shutdown = thread.thread.shutdown_and_wait().await;
    let _removed = thread_manager.remove_thread(&thread.thread_id).await;

    shutdown?;
    output
}

pub(crate) async fn build_prompt_input_from_session(
    sess: &Arc<Session>,
    input: Vec<UserInput>,
) -> CodexResult<crate::client_common::Prompt> {
    let turn_context = sess.new_default_turn().await;
    // Prompt debugging builds a standalone request without entering run_turn.
    let step_context = sess
        .capture_step_context(Arc::clone(&turn_context), &CancellationToken::new())
        .await?;
    sess.record_context_updates_and_set_reference_context_item(step_context.as_ref())
        .await?;

    if !input.is_empty() {
        let response_item = sess.response_item_from_user_input(input);
        sess.record_conversation_items(turn_context.as_ref(), std::slice::from_ref(&response_item))
            .await;
    }

    let prompt_input = sess
        .clone_history()
        .await
        .for_prompt(&turn_context.model_info.input_modalities);
    let base_instructions = sess.get_base_instructions().await;
    Ok(build_prompt(
        prompt_input,
        step_context.tool_router.as_ref(),
        turn_context.as_ref(),
        base_instructions,
    ))
}

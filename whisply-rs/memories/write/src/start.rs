use crate::extensions::seed_extension_instructions;
use crate::guard;
use crate::memory_root;
use crate::metrics::MEMORY_STARTUP;
use crate::phase1;
use crate::phase2;
use crate::runtime::MemoryStartupContext;
use std::sync::Arc;
use tracing::warn;
use whisply_core::CodexThread;
use whisply_core::ThreadManager;
use whisply_core::config::Config;
use whisply_features::Feature;
use whisply_login::AuthManager;
use whisply_protocol::ThreadId;
use whisply_protocol::models::PermissionProfile;
use whisply_protocol::protocol::SessionSource;

/// Starts the asynchronous startup memory pipeline for an eligible root session.
///
/// The pipeline is skipped for ephemeral sessions, disabled feature flags, and
/// subagent sessions.
pub fn start_memories_startup_task(
    thread_manager: Arc<ThreadManager>,
    auth_manager: Arc<AuthManager>,
    thread_id: ThreadId,
    thread: Arc<CodexThread>,
    config: Arc<Config>,
    parent_permission_profile: PermissionProfile,
    source: &SessionSource,
) {
    if config.ephemeral
        || !config.features.enabled(Feature::MemoryTool)
        || source.is_non_root_agent()
    {
        return;
    }

    let context = Arc::new(MemoryStartupContext::new(
        thread_manager,
        Arc::clone(&auth_manager),
        thread_id,
        thread,
        config.as_ref(),
        source.clone(),
    ));

    if context.state_db().is_none() {
        warn!("state db unavailable for memories startup pipeline; skipping");
        return;
    }

    tokio::spawn(async move {
        let root = memory_root(&config.codex_home);
        if let Err(err) = tokio::fs::create_dir_all(&root).await {
            warn!("failed creating memories root: {err}");
            return;
        }
        if let Err(err) = seed_extension_instructions(&root).await {
            warn!("failed seeding memory extension instructions: {err}");
        }

        // Clean memories to make preserve DB size. This does not consume tokens so can be
        // done before the quota check.
        phase1::prune(context.as_ref(), &config).await;

        if !guard::rate_limits_ok(&auth_manager, &config).await {
            context.counter(
                MEMORY_STARTUP,
                /*inc*/ 1,
                &[("status", "skipped_rate_limit")],
            );
            return;
        }

        // Both phases spend tokens and write files, and a person can turn
        // memory off while they are still queued, so the switch is read again
        // before each one rather than only at session start.
        if !guard::memories_still_wanted(&config).await {
            context.counter(
                MEMORY_STARTUP,
                /*inc*/ 1,
                &[("status", "skipped_turned_off")],
            );
            return;
        }

        // Run phase 1.
        phase1::run(Arc::clone(&context), Arc::clone(&config)).await;

        if !guard::memories_still_wanted(&config).await {
            context.counter(
                MEMORY_STARTUP,
                /*inc*/ 1,
                &[("status", "skipped_turned_off")],
            );
            return;
        }
        // Run phase 2.
        phase2::run(context, config, parent_permission_profile).await;
    });
}

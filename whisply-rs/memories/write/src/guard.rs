use tracing::info;
use tracing::warn;
use whisply_backend_client::Client as BackendClient;
use whisply_config::config_toml::ConfigToml;
use whisply_core::config::Config;
use whisply_core::config::LoaderOverrides;
use whisply_core::config::load_config_as_toml_with_cli_overrides;
use whisply_features::Feature;
use whisply_features::FeaturesToml;
use whisply_login::AuthManager;
use whisply_protocol::protocol::RateLimitSnapshot;
use whisply_protocol::protocol::RateLimitWindow;

pub(crate) async fn rate_limits_ok(auth_manager: &AuthManager, config: &Config) -> bool {
    rate_limits_check(auth_manager, config)
        .await
        .unwrap_or(true)
}

async fn rate_limits_check(auth_manager: &AuthManager, config: &Config) -> Option<bool> {
    let auth = auth_manager.auth().await?;
    if !auth.uses_codex_backend() {
        return None;
    }

    let client = BackendClient::from_auth(
        config.chatgpt_base_url.clone(),
        &auth,
        config.http_client_factory(),
    );

    let snapshots = client
        .get_rate_limits_many()
        .await
        .map_err(|err| warn!(%err, "failed to fetch rate limits"))
        .ok()?;

    let snapshot = snapshots
        .iter()
        .find(|s| s.limit_id.as_deref() == Some(crate::guard_limits::CODEX_LIMIT_ID))
        .or_else(|| snapshots.first())?;

    let min_remaining_percent = config.memories.min_rate_limit_remaining_percent;
    let allowed = snapshot_allows_startup(snapshot, min_remaining_percent);

    if !allowed {
        info!(
            min_remaining_percent,
            "skipping memories startup because Whisply rate limits are below the configured threshold"
        );
    }

    Some(allowed)
}

fn snapshot_allows_startup(snapshot: &RateLimitSnapshot, min_remaining_percent: i64) -> bool {
    if snapshot.rate_limit_reached_type.is_some() {
        return false;
    }

    let max_used_percent = 100.0 - min_remaining_percent.clamp(0, 100) as f64;
    window_allows_startup(snapshot.primary.as_ref(), max_used_percent)
        && window_allows_startup(snapshot.secondary.as_ref(), max_used_percent)
}

/// Whether the person still wants memory, read now rather than remembered.
///
/// The pipeline is started once, from a config snapshot taken when the session
/// began, and then runs in the background for as long as extraction and
/// consolidation take. Someone who turns memory off during that window has said
/// something about what happens next, not only about the next session, and
/// everything after this point spends their tokens and writes files. Only an
/// explicit "no" stops the run: if the current config cannot be read, or says
/// nothing, the answer the session already had stands.
pub(crate) async fn memories_still_wanted(config: &Config) -> bool {
    let Ok(toml) = load_config_as_toml_with_cli_overrides(
        &config.codex_home,
        /* cwd */ None,
        /* cli_overrides */ Vec::new(),
        LoaderOverrides::default(),
    )
    .await
    else {
        return true;
    };
    memories_wanted_by(&toml)
}

fn memories_wanted_by(toml: &ConfigToml) -> bool {
    let entries = toml
        .features
        .as_ref()
        .map(FeaturesToml::entries)
        .unwrap_or_default();
    let turned_off = [Feature::MemoryTool.key(), "memory_tool"]
        .iter()
        .any(|key| entries.get(*key) == Some(&false));
    let generation_turned_off = toml
        .memories
        .as_ref()
        .and_then(|memories| memories.generate_memories)
        == Some(false);

    if turned_off || generation_turned_off {
        info!("stopping the memory pipeline because memory is now turned off");
        return false;
    }
    true
}

fn window_allows_startup(window: Option<&RateLimitWindow>, max_used_percent: f64) -> bool {
    match window {
        Some(window) => window.used_percent <= max_used_percent,
        None => true,
    }
}

#[cfg(test)]
#[path = "guard_tests.rs"]
mod tests;

use super::session::Session;
use super::turn_context::TurnContext;
use whisply_protocol::config_types::AutoCompactTokenLimitScope;

#[derive(Debug)]
pub(crate) struct ContextWindowTokenStatus {
    // Full active context usage, independent of the configured auto-compact scope.
    pub(crate) active_context_tokens: i64,
    // Usage counted against `model_auto_compact_token_limit` for the current scope.
    pub(crate) auto_compact_scope_tokens: i64,
    pub(crate) auto_compact_scope_limit: Option<i64>,
    auto_compact_limit_including_fallback: Option<i64>,
    pub(crate) full_context_window_limit: Option<i64>,
    pub(crate) base_window_tokens_remaining: Option<i64>,
    pub(crate) auto_compact_window_prefill_tokens: Option<i64>,
    pub(crate) full_context_window_limit_reached: bool,
    pub(crate) token_limit_reached: bool,
}

impl ContextWindowTokenStatus {
    /// Preview incoming input without recording or compacting that unsampled
    /// material. The same model limits and fallback headroom apply as above.
    pub(crate) fn reaches_limit_with_pending_input(&self, pending_tokens: i64) -> bool {
        let pending_tokens = pending_tokens.max(0);
        self.auto_compact_limit_including_fallback
            .is_some_and(|limit| {
                self.auto_compact_scope_tokens
                    .saturating_add(pending_tokens)
                    >= limit
            })
            || self.full_context_window_limit.is_some_and(|limit| {
                self.active_context_tokens.saturating_add(pending_tokens) >= limit
            })
    }
}

fn tokens_remaining(limit: Option<i64>, used: i64) -> Option<i64> {
    limit.map(|limit| limit.saturating_sub(used).max(0))
}

pub(crate) async fn context_window_token_status(
    sess: &Session,
    turn_context: &TurnContext,
) -> ContextWindowTokenStatus {
    let active_context_tokens = sess.get_total_token_usage().await;

    // Count either the full active context or only the tokens added after the initial prefix.
    let (auto_compact_scope_tokens, auto_compact_scope_limit, auto_compact_window_prefill_tokens) =
        match turn_context.config.model_auto_compact_token_limit_scope {
            AutoCompactTokenLimitScope::Total => (
                active_context_tokens,
                turn_context.model_info.auto_compact_token_limit(),
                None,
            ),
            AutoCompactTokenLimitScope::BodyAfterPrefix => {
                let window = sess.auto_compact_window_snapshot().await;
                let baseline = window.prefill_input_tokens.unwrap_or(active_context_tokens);

                let scope_limit = turn_context
                    .config
                    .model_auto_compact_token_limit
                    .or_else(|| turn_context.model_info.auto_compact_token_limit());
                (
                    active_context_tokens.saturating_sub(baseline),
                    scope_limit,
                    window.prefill_input_tokens,
                )
            }
        };

    // The model's full context window is a hard cap, independent of the auto-compaction scope.
    let full_context_window_limit = turn_context.model_context_window();

    // Report remaining tokens against the base (unbuffered) window, capped by the full context.
    let base_window_tokens_remaining = [
        tokens_remaining(auto_compact_scope_limit, auto_compact_scope_tokens),
        tokens_remaining(full_context_window_limit, active_context_tokens),
    ]
    .into_iter()
    .flatten()
    .min();

    // Only reserve the fallback buffer when there is a fallback prompt to use it.
    let auto_compact_fallback_buffer_tokens = turn_context
        .config
        .token_budget
        .as_ref()
        .map_or(0, crate::config::TokenBudgetConfig::fallback_buffer_tokens);
    let buffered_auto_compact_limit = auto_compact_scope_limit
        .map(|limit| limit.saturating_add(auto_compact_fallback_buffer_tokens));

    // Force compaction once the buffered window or the model's full context window is reached.
    let full_context_window_limit_reached =
        full_context_window_limit.is_some_and(|limit| active_context_tokens >= limit);
    let token_limit_reached = buffered_auto_compact_limit
        .is_some_and(|limit| auto_compact_scope_tokens >= limit)
        || full_context_window_limit_reached;

    let status = ContextWindowTokenStatus {
        active_context_tokens,
        auto_compact_scope_tokens,
        auto_compact_scope_limit,
        auto_compact_limit_including_fallback: buffered_auto_compact_limit,
        full_context_window_limit,
        base_window_tokens_remaining,
        auto_compact_window_prefill_tokens,
        full_context_window_limit_reached,
        token_limit_reached,
    };

    // The single place the budget is worked out, so an inspection of how close
    // a conversation is to being shortened reports the same numbers the
    // shortening decision itself used rather than a second estimate.
    sess.observe_compaction_budget(compaction_budget(
        &status,
        turn_context.config.model_auto_compact_token_limit_scope,
    ));

    status
}

fn compaction_budget(
    status: &ContextWindowTokenStatus,
    scope: AutoCompactTokenLimitScope,
) -> codex_whisply::CompactionBudget {
    codex_whisply::CompactionBudget {
        context_window_tokens: status.full_context_window_limit,
        auto_compact_limit_tokens: status.auto_compact_scope_limit,
        limit_scope: match scope {
            AutoCompactTokenLimitScope::Total => codex_whisply::CompactionLimitScope::Total,
            AutoCompactTokenLimitScope::BodyAfterPrefix => {
                codex_whisply::CompactionLimitScope::BodyAfterPrefix
            }
        },
        active_context_tokens: status.active_context_tokens,
        scope_tokens: status.auto_compact_scope_tokens,
        tokens_remaining: status.base_window_tokens_remaining,
        threshold_reached: status.token_limit_reached,
    }
}

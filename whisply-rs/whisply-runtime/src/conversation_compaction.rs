//! What can be said about a long conversation being shortened.
//!
//! Compaction is the one thing the product does to a person's conversation
//! without being asked: at some point the thread stops fitting, the runtime
//! summarizes what came before, and everything the model can still see is that
//! summary plus what came after. When someone later says the assistant forgot
//! something, or that a chat got worse after a while, there is currently no way
//! to answer them -- the facts exist only as an analytics event on its way
//! somewhere else, and as a warning line in a terminal.
//!
//! This is the record that makes those questions answerable: what the budget is
//! and how close the conversation is to it, what triggered each shortening,
//! how much was in the window before and after, and whether the shortened
//! history was actually written down. Whether it was written down is the part
//! that cannot be assumed -- persistence is best-effort in the runtime, and a
//! compaction that failed to persist means the next resume reads the long
//! history back.
//!
//! Deliberately no summary text. The summary is the person's conversation, in
//! compressed form; an inspection surface that printed it would be a way to
//! read someone's chat without opening it. What is carried instead is its size
//! and a digest, which is enough to prove that the summary a resumed thread
//! holds is the one that compaction wrote, and useless for anything else.

use serde::Deserialize;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;

/// The only conversation-compaction inspection contract this runtime accepts.
pub const CONVERSATION_COMPACTION_CONTRACT_VERSION: u16 = 1;

/// How many shortenings are kept for inspection.
///
/// A thread can be compacted many times, and the interesting ones are the
/// recent ones plus the fact that there were others. Keeping every record would
/// let a long-lived conversation grow an unbounded diagnostic tail.
pub const MAX_COMPACTION_RECORDS: usize = 8;

/// Who asked for the conversation to be shortened.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionTriggerKind {
    /// The runtime reached its budget and shortened the thread on its own.
    Automatic,
    /// A person asked for it.
    Requested,
}

/// Which mechanism did the shortening.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionMechanism {
    /// The model was asked to summarize the thread.
    Summarized,
    /// A fresh context window was installed without a summarization turn.
    WindowReset,
}

/// How the shortening ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionOutcome {
    Completed,
    Interrupted,
    Failed,
}

/// Which tokens are counted against the automatic-shortening limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionLimitScope {
    /// Everything currently in the window.
    Total,
    /// Only what was added after the window's initial prefix.
    BodyAfterPrefix,
}

/// How close this conversation is to being shortened.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionBudget {
    /// The model's hard context window, when the model declares one.
    pub context_window_tokens: Option<i64>,
    /// The limit that triggers automatic shortening, when one applies.
    pub auto_compact_limit_tokens: Option<i64>,
    pub limit_scope: CompactionLimitScope,
    /// Everything currently in the window.
    pub active_context_tokens: i64,
    /// What the limit above is measured against, which differs from
    /// `active_context_tokens` only when the scope excludes the prefix.
    pub scope_tokens: i64,
    /// Tokens left before the nearer of the two limits, when either is known.
    pub tokens_remaining: Option<i64>,
    /// Whether the next turn would shorten the conversation.
    pub threshold_reached: bool,
}

impl CompactionBudget {
    /// How much of the budget is spent, as hundredths of a percent.
    ///
    /// Integer rather than a float so the same number can be compared across a
    /// protocol boundary without a rounding argument. `None` when nothing
    /// bounds the conversation, which is a real state and not a zero.
    pub fn used_basis_points(&self) -> Option<u32> {
        let (used, limit) = match (self.auto_compact_limit_tokens, self.context_window_tokens) {
            (Some(limit), _) if limit > 0 => (self.scope_tokens, limit),
            (_, Some(window)) if window > 0 => (self.active_context_tokens, window),
            _ => return None,
        };
        let used = used.clamp(0, limit);
        u32::try_from(used.saturating_mul(10_000) / limit).ok()
    }
}

/// One shortening that happened to this conversation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionRecord {
    /// 1 for the first shortening of this thread, counting up.
    pub sequence: u32,
    pub trigger: CompactionTriggerKind,
    pub mechanism: CompactionMechanism,
    pub outcome: CompactionOutcome,
    pub active_context_tokens_before: i64,
    pub active_context_tokens_after: i64,
    /// The summary's size, when one was produced.
    pub summary_tokens: Option<i64>,
    /// A digest of the summary, never the summary.
    pub summary_digest: Option<String>,
    /// Whether the shortened history reached the thread's stored record.
    ///
    /// Not an assumption: a compaction whose rollout append failed leaves the
    /// long history to be read back on the next resume, and the person is owed
    /// that fact rather than a reassuring blank.
    pub persisted: bool,
    pub recorded_at_unix_ms: i64,
}

/// Everything inspectable about one conversation's shortening.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationCompactionStatus {
    pub contract_version: u16,
    /// Absent until the thread has run a turn, because until then nothing has
    /// measured the window.
    pub budget: Option<CompactionBudget>,
    /// Every shortening this thread has had, including ones no longer kept
    /// individually below.
    pub compaction_count: u32,
    /// The most recent shortenings, oldest first.
    pub records: Vec<CompactionRecord>,
}

/// The running record a session keeps while a conversation is open.
#[derive(Clone, Debug, Default)]
pub struct ConversationCompactionLog {
    budget: Option<CompactionBudget>,
    compaction_count: u32,
    records: Vec<CompactionRecord>,
}

impl ConversationCompactionLog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records what the window looked like when it was last measured.
    pub fn observe_budget(&mut self, budget: CompactionBudget) {
        self.budget = Some(budget);
    }

    /// Records one shortening and returns the sequence number it was given.
    pub fn observe_compaction(&mut self, observation: CompactionObservation) -> u32 {
        self.compaction_count = self.compaction_count.saturating_add(1);
        let record = CompactionRecord {
            sequence: self.compaction_count,
            trigger: observation.trigger,
            mechanism: observation.mechanism,
            outcome: observation.outcome,
            active_context_tokens_before: observation.active_context_tokens_before,
            active_context_tokens_after: observation.active_context_tokens_after,
            summary_tokens: observation.summary_tokens,
            summary_digest: observation.summary.as_deref().map(summary_digest),
            persisted: observation.persisted,
            recorded_at_unix_ms: observation.recorded_at_unix_ms,
        };
        self.records.push(record);
        if self.records.len() > MAX_COMPACTION_RECORDS {
            let excess = self.records.len() - MAX_COMPACTION_RECORDS;
            self.records.drain(..excess);
        }
        self.compaction_count
    }

    pub fn status(&self) -> ConversationCompactionStatus {
        ConversationCompactionStatus {
            contract_version: CONVERSATION_COMPACTION_CONTRACT_VERSION,
            budget: self.budget.clone(),
            compaction_count: self.compaction_count,
            records: self.records.clone(),
        }
    }
}

/// What a compaction reports about itself as it finishes.
///
/// The summary arrives as text and leaves as a digest; nothing here keeps it.
#[derive(Clone, Debug)]
pub struct CompactionObservation {
    pub trigger: CompactionTriggerKind,
    pub mechanism: CompactionMechanism,
    pub outcome: CompactionOutcome,
    pub active_context_tokens_before: i64,
    pub active_context_tokens_after: i64,
    pub summary_tokens: Option<i64>,
    pub summary: Option<String>,
    pub persisted: bool,
    pub recorded_at_unix_ms: i64,
}

/// A stable digest of one summary.
pub fn summary_digest(summary: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(summary.as_bytes());
    hasher
        .finalize()
        .iter()
        .fold(String::with_capacity(64), |mut acc, byte| {
            use std::fmt::Write as _;
            let _ = write!(acc, "{byte:02x}");
            acc
        })
}

#[cfg(test)]
#[path = "conversation_compaction_tests.rs"]
mod tests;

use super::*;

fn budget() -> CompactionBudget {
    CompactionBudget {
        context_window_tokens: Some(200_000),
        auto_compact_limit_tokens: Some(160_000),
        limit_scope: CompactionLimitScope::Total,
        active_context_tokens: 120_000,
        scope_tokens: 120_000,
        tokens_remaining: Some(40_000),
        threshold_reached: false,
    }
}

fn observation(summary: Option<&str>) -> CompactionObservation {
    CompactionObservation {
        trigger: CompactionTriggerKind::Automatic,
        mechanism: CompactionMechanism::Summarized,
        outcome: CompactionOutcome::Completed,
        active_context_tokens_before: 158_000,
        active_context_tokens_after: 9_400,
        summary_tokens: Some(1_200),
        summary: summary.map(str::to_string),
        persisted: true,
        recorded_at_unix_ms: 1_770_000_000_000,
    }
}

#[test]
fn nothing_of_what_was_said_survives_into_the_record() {
    let mut log = ConversationCompactionLog::new();
    log.observe_compaction(observation(Some(
        "The user's bank account ends 4417 and they asked about the transfer.",
    )));

    let status = serde_json::to_string(&log.status()).expect("serialize the compaction status");

    assert!(
        !status.contains("4417") && !status.contains("bank"),
        "the summary is the person's conversation in compressed form; an \
         inspection surface that carried it would be a way to read a chat \
         without opening it: {status}"
    );
    assert!(
        status.contains("summary_digest"),
        "a digest still has to be there, or there is no way to show that a \
         resumed thread holds the summary compaction wrote"
    );
}

#[test]
fn the_same_summary_is_recognisable_after_a_resume() {
    let summary = "Summary of the conversation so far.";

    assert_eq!(
        summary_digest(summary),
        summary_digest(summary),
        "the digest is what proves a resumed thread carries the same summary"
    );
    assert_ne!(summary_digest(summary), summary_digest("something else"));
    assert_eq!(summary_digest(summary).len(), 64);
}

#[test]
fn a_shortening_that_was_never_written_down_says_so() {
    let mut log = ConversationCompactionLog::new();
    let mut unpersisted = observation(Some("Summary."));
    unpersisted.persisted = false;
    log.observe_compaction(unpersisted);

    let status = log.status();

    assert!(
        !status.records[0].persisted,
        "a compaction whose rollout append failed leaves the long history to \
         be read back on the next resume; reporting it as done would send \
         someone looking for the bug anywhere but here"
    );
}

#[test]
fn a_long_conversation_does_not_grow_an_unbounded_tail() {
    let mut log = ConversationCompactionLog::new();
    for _ in 0..(MAX_COMPACTION_RECORDS + 5) {
        log.observe_compaction(observation(Some("Summary.")));
    }

    let status = log.status();

    assert_eq!(status.records.len(), MAX_COMPACTION_RECORDS);
    assert_eq!(
        status.compaction_count as usize,
        MAX_COMPACTION_RECORDS + 5,
        "how many times a thread has been shortened is the fact someone wants \
         first, and it must not be capped along with the detail"
    );
    assert_eq!(
        status.records.first().map(|record| record.sequence),
        Some(6),
        "the recent shortenings are the ones kept"
    );
    assert_eq!(
        status.records.last().map(|record| record.sequence),
        Some(13)
    );
}

#[test]
fn a_thread_that_has_not_run_yet_reports_no_budget_rather_than_an_empty_one() {
    let status = ConversationCompactionLog::new().status();

    assert!(
        status.budget.is_none(),
        "nothing has measured the window yet, and a zeroed budget would read \
         as a conversation with all its room left"
    );
    assert_eq!(status.compaction_count, 0);
    assert!(status.records.is_empty());
}

#[test]
fn how_full_the_conversation_is_counts_against_the_limit_that_will_trigger() {
    let budget = budget();

    assert_eq!(
        budget.used_basis_points(),
        Some(7_500),
        "120k of a 160k shortening limit is three quarters full, even though \
         it is well under the model's 200k window"
    );
}

#[test]
fn a_conversation_with_nothing_bounding_it_does_not_report_an_empty_one() {
    let unbounded = CompactionBudget {
        context_window_tokens: None,
        auto_compact_limit_tokens: None,
        active_context_tokens: 40_000,
        scope_tokens: 40_000,
        tokens_remaining: None,
        ..budget()
    };

    assert_eq!(
        unbounded.used_basis_points(),
        None,
        "no limit is a real state; reporting 0% would say the conversation has \
         all its room left, which is a different claim"
    );
}

#[test]
fn a_conversation_past_its_limit_is_reported_as_full_and_not_as_more_than_full() {
    let over = CompactionBudget {
        scope_tokens: 200_000,
        active_context_tokens: 200_000,
        tokens_remaining: Some(0),
        threshold_reached: true,
        ..budget()
    };

    assert_eq!(over.used_basis_points(), Some(10_000));
}

#[test]
fn the_budget_is_the_last_one_measured_not_the_first() {
    let mut log = ConversationCompactionLog::new();
    log.observe_budget(budget());
    log.observe_budget(CompactionBudget {
        active_context_tokens: 12_000,
        scope_tokens: 12_000,
        tokens_remaining: Some(148_000),
        ..budget()
    });

    assert_eq!(
        log.status()
            .budget
            .and_then(|budget| budget.tokens_remaining),
        Some(148_000),
        "after a shortening the window is nearly empty again, and that is the \
         state someone is asking about"
    );
}

use super::*;
use pretty_assertions::assert_eq;

fn frame(bytes: &[u8]) -> WhisplyHistoryRecoveryFrame {
    WhisplyHistoryRecoveryFrame {
        archive_id: "a".repeat(64),
        offset: 0,
        data_base64: STANDARD.encode(bytes),
        final_byte_count: Some(bytes.len() as u64),
        final_sha256: Some(format!("{:x}", Sha256::digest(bytes))),
    }
}
fn source(value: &str) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(&RecoveryRecord {
        speaker: RecoverySpeaker::User,
        kind: RecoveryPartKind::Text,
        value: value.to_owned(),
    })
    .unwrap();
    bytes.push(b'\n');
    bytes
}

#[test]
fn bounded_transfer_retries_commit_exactly_once_and_reject_different_prefixes() {
    let bytes = source(&"🙂 historical text ".repeat(20_000));
    let mut state = HistoryRecoveryState::default();
    let whole = frame(&bytes);
    let mut records = Vec::new();
    for (index, chunk) in bytes.chunks(MAX_FRAME_BYTES).enumerate() {
        let end = index * MAX_FRAME_BYTES + chunk.len();
        let part = WhisplyHistoryRecoveryFrame {
            archive_id: whole.archive_id.clone(),
            offset: (index * MAX_FRAME_BYTES) as u64,
            data_base64: STANDARD.encode(chunk),
            final_byte_count: (end == bytes.len()).then_some(bytes.len() as u64),
            final_sha256: (end == bytes.len()).then(|| whole.final_sha256.clone().unwrap()),
        };
        assert!(state.validate_frame(&part).unwrap());
        state.install_frame(&part).unwrap();
        assert!(!state.validate_frame(&part).unwrap());
        records.push(RolloutItem::WhisplyHistoryRecovery(
            WhisplyHistoryRecoveryRecord::Frame { frame: part },
        ));
    }
    let restored = HistoryRecoveryState::restore(&records);
    assert!(restored.committed && !restored.invalid);
    assert_eq!(restored.data, bytes);
    assert_eq!(
        restored.next_record().unwrap().unwrap().0.value,
        "🙂 historical text ".repeat(20_000)
    );
    let mut changed = frame(&source("different"));
    assert!(restored.validate_frame(&changed).is_err());
    changed.archive_id = "b".repeat(64);
    assert!(restored.validate_frame(&changed).is_err());
    let mut out_of_order = frame(b"a");
    out_of_order.offset = (bytes.len() + 1) as u64;
    assert!(restored.validate_frame(&out_of_order).is_err());
}

#[test]
fn committed_pending_source_can_extend_only_its_identical_prefix() {
    let original = source("first saved turn");
    let mut state = HistoryRecoveryState::default();
    state.install_frame(&frame(&original)).unwrap();
    let prefix_cursor = WhisplyHistoryRecoveryCursor {
        archive_id: "a".repeat(64),
        text_offset: 5,
        ..Default::default()
    };
    state.cursor = Some(prefix_cursor.clone());
    let mut extended = original.clone();
    extended.extend(source("saved interrupted fresh turn"));
    state.install_frame(&frame(&extended)).unwrap();
    assert_eq!(state.cursor, Some(prefix_cursor));
    assert_eq!(state.data, extended);
    assert!(state.committed);
}

#[test]
fn atomic_summary_cursor_and_later_raw_item_resume_without_replaying_a_paid_prefix() {
    let mut bytes = source("already summarized");
    let second_offset = bytes.len() as u64;
    bytes.extend(source("not yet summarized"));
    let cursor = WhisplyHistoryRecoveryCursor {
        archive_id: "a".repeat(64),
        record_offset: second_offset,
        compaction_id: Some("attempt-1".to_owned()),
        ..Default::default()
    };
    let checkpoint = CompactedItem {
        message: "durable summary".to_owned(),
        whisply_history_recovery: Some(cursor.clone()),
        replacement_history: Some(vec![]),
        ..Default::default()
    };
    let mut records = vec![
        RolloutItem::WhisplyHistoryRecovery(WhisplyHistoryRecoveryRecord::Frame {
            frame: frame(&bytes),
        }),
        RolloutItem::WhisplyHistoryRecovery(WhisplyHistoryRecoveryRecord::CompactionStarted {
            turn_id: Some("prior-turn".to_owned()),
            cursor: cursor.clone(),
        }),
        RolloutItem::Compacted(checkpoint),
    ];
    let state = HistoryRecoveryState::restore(&records);
    assert!(state.in_flight.is_none());
    assert_eq!(
        state.next_record().unwrap().unwrap().0.value,
        "not yet summarized"
    );
    let partial = WhisplyHistoryRecoveryCursor {
        text_offset: 4,
        ..cursor.clone()
    };
    let mut item = response_item(
        &RecoveryRecord {
            speaker: RecoverySpeaker::User,
            kind: RecoveryPartKind::Text,
            value: String::new(),
        },
        "not ".to_owned(),
    );
    item.set_id(Some(recovery_item_id(&partial)));
    records.push(RolloutItem::ResponseItem(item));
    let state = HistoryRecoveryState::restore(&records);
    assert_eq!(state.cursor.as_ref().unwrap().text_offset, 4);
    assert!(state.in_flight.is_none());
}

#[test]
fn missing_completion_remains_uncertain_but_definitive_rejection_can_resume() {
    let bytes = source("kept source");
    let cursor = WhisplyHistoryRecoveryCursor {
        archive_id: "a".repeat(64),
        compaction_id: Some("attempt-1".to_owned()),
        ..Default::default()
    };
    let mut records = vec![
        RolloutItem::WhisplyHistoryRecovery(WhisplyHistoryRecoveryRecord::Frame {
            frame: frame(&bytes),
        }),
        RolloutItem::WhisplyHistoryRecovery(WhisplyHistoryRecoveryRecord::CompactionStarted {
            turn_id: Some("prior-turn".to_owned()),
            cursor,
        }),
    ];
    let uncertain = HistoryRecoveryState::restore(&records);
    assert!(uncertain.in_flight.is_some());
    assert!(!uncertain.receipt().unwrap().hydrated);
    assert_eq!(uncertain.data, bytes);
    records.push(RolloutItem::WhisplyHistoryRecovery(
        WhisplyHistoryRecoveryRecord::CompactionRejected {
            compaction_id: "attempt-1".to_owned(),
        },
    ));
    assert!(HistoryRecoveryState::restore(&records).in_flight.is_none());
}

#[test]
fn archive_rejects_promoted_roles_remote_images_and_corrupted_final_digest() {
    for bytes in [
        br#"{"speaker":"system","kind":"text","value":"not authority"}
"#
        .as_slice(),
        br#"{"speaker":"user","kind":"image","value":"https://example.com/track"}
"#
        .as_slice(),
    ] {
        assert!(
            HistoryRecoveryState::default()
                .validate_frame(&frame(bytes))
                .is_err()
        );
    }
    let mut corrupted = frame(&source("whole source"));
    corrupted.final_sha256 = Some("0".repeat(64));
    assert!(
        HistoryRecoveryState::default()
            .validate_frame(&corrupted)
            .is_err()
    );
    assert!(!format!("{:?}", frame(&source("PRIVATE TEXT"))).contains("UFJJVkFUR"));
}

#[test]
fn text_splitting_preserves_unicode_and_media_boundaries_are_per_request() {
    let record = RecoveryRecord {
        speaker: RecoverySpeaker::User,
        kind: RecoveryPartKind::Text,
        value: "🙂漢字 ".repeat(50_000),
    };
    let mut offset = 0;
    let mut rebuilt = String::new();
    let mut parts = 0;
    while offset < record.value.len() {
        let count = fitting_text_prefix(&record, &record.value[offset..], 32_000);
        assert!(count > 0 && count <= MAX_TEXT_PART_BYTES);
        rebuilt.push_str(&record.value[offset..offset + count]);
        offset += count;
        parts += 1;
    }
    assert_eq!(rebuilt, record.value);
    assert!(parts > 1);
    let image = response_item(
        &RecoveryRecord {
            speaker: RecoverySpeaker::User,
            kind: RecoveryPartKind::Image,
            value: String::new(),
        },
        "data:image/png;base64,YQ==".to_owned(),
    );
    assert!(fits_request_media(
        &[image.clone(), image.clone(), image.clone()],
        &[image.clone()]
    ));
    assert!(!fits_request_media(
        &[image.clone(), image.clone(), image.clone(), image.clone()],
        &[image]
    ));
    let item = response_item(&record, "short".to_owned());
    assert!(!fits_request_media(
        &vec![item.clone(); MAX_INPUT_ITEMS - 1],
        &[item]
    ));
}

#[test]
fn uncertain_compaction_requires_a_different_explicit_turn_and_preserves_its_marker() {
    let bytes = source("saved prefix");
    let cursor = WhisplyHistoryRecoveryCursor {
        archive_id: "a".repeat(64),
        compaction_id: Some("uncertain-attempt".to_owned()),
        ..Default::default()
    };
    let mut records = vec![
        RolloutItem::WhisplyHistoryRecovery(WhisplyHistoryRecoveryRecord::Frame {
            frame: frame(&bytes),
        }),
        RolloutItem::WhisplyHistoryRecovery(WhisplyHistoryRecoveryRecord::CompactionStarted {
            cursor,
            turn_id: Some("original-turn".to_owned()),
        }),
    ];
    let state = HistoryRecoveryState::restore(&records);
    assert!(
        state
            .interrupted_compaction_for_continuation("original-turn")
            .is_err()
    );
    assert_eq!(
        state
            .interrupted_compaction_for_continuation("new-user-turn")
            .unwrap()
            .as_deref(),
        Some("uncertain-attempt")
    );
    records.push(RolloutItem::WhisplyHistoryRecovery(
        WhisplyHistoryRecoveryRecord::CompactionContinued {
            compaction_id: "uncertain-attempt".to_owned(),
            turn_id: "new-user-turn".to_owned(),
        },
    ));
    let continued = HistoryRecoveryState::restore(&records);
    assert!(continued.in_flight.is_none());
    assert_eq!(continued.data, bytes);
    assert!(matches!(
        &records[1],
        RolloutItem::WhisplyHistoryRecovery(WhisplyHistoryRecoveryRecord::CompactionStarted { .. })
    ));
}

#[test]
fn uncertain_final_compaction_allows_append_only_source_extension_and_clears_eof() {
    let original = source("original complete source");
    let mut state = HistoryRecoveryState::default();
    state.install_frame(&frame(&original)).unwrap();
    let cursor = WhisplyHistoryRecoveryCursor {
        archive_id: "a".repeat(64),
        record_offset: original.len() as u64,
        completed: true,
        compaction_id: Some("uncertain-final".to_owned()),
        ..Default::default()
    };
    state.cursor = Some(cursor.clone());
    state.in_flight = Some(cursor);
    state.in_flight_turn_id = Some("stopped-turn".to_owned());
    let mut extended = original.clone();
    extended.extend(source("saved stopped question"));
    assert!(state.validate_frame(&frame(&extended)).unwrap());
    state.install_frame(&frame(&extended)).unwrap();
    assert!(!state.cursor.as_ref().unwrap().completed);
    assert_eq!(
        state.next_record().unwrap().unwrap().0.value,
        "saved stopped question"
    );
    assert!(!state.receipt().unwrap().hydrated);
}

#[test]
fn definitive_rejection_at_eof_preserves_debt_without_an_uncertain_paid_attempt() {
    let original = source("original complete source");
    let mut state = HistoryRecoveryState::default();
    let source_frame = frame(&original);
    state.install_frame(&source_frame).unwrap();
    let cursor = WhisplyHistoryRecoveryCursor {
        archive_id: "a".repeat(64),
        record_offset: original.len() as u64,
        completed: true,
        compaction_id: Some("rejected-final".to_owned()),
        ..Default::default()
    };
    let mut last_item = response_item(
        &RecoveryRecord {
            speaker: RecoverySpeaker::User,
            kind: RecoveryPartKind::Text,
            value: String::new(),
        },
        "original complete source".to_owned(),
    );
    last_item.set_id(Some(recovery_item_id(&cursor)));
    state.observe_item(&last_item);
    assert!(state.receipt().unwrap().hydrated);
    let mut extended = original.clone();
    extended.extend(source("saved rejected question"));
    assert!(
        !state.validate_frame(&frame(&extended)).unwrap(),
        "a healthy completed archive cannot be reopened"
    );

    state.in_flight = Some(cursor.clone());
    state.in_flight_turn_id = Some("rejected-turn".to_owned());
    state.observe_rejected_compaction("unrelated-rejection");
    assert!(state.in_flight.is_some());
    state.observe_rejected_compaction("rejected-final");
    assert!(state.in_flight.is_none());
    assert!(state.in_flight_turn_id.is_none());
    assert!(!state.receipt().unwrap().hydrated);
    assert!(state.validate_frame(&frame(&extended)).unwrap());

    let records = vec![
        RolloutItem::WhisplyHistoryRecovery(WhisplyHistoryRecoveryRecord::Frame {
            frame: source_frame,
        }),
        RolloutItem::ResponseItem(last_item),
        RolloutItem::WhisplyHistoryRecovery(WhisplyHistoryRecoveryRecord::CompactionStarted {
            cursor,
            turn_id: Some("rejected-turn".to_owned()),
        }),
        RolloutItem::WhisplyHistoryRecovery(WhisplyHistoryRecoveryRecord::CompactionRejected {
            compaction_id: "rejected-final".to_owned(),
        }),
    ];
    let mut restored = HistoryRecoveryState::restore(&records);
    assert!(!restored.receipt().unwrap().hydrated);
    assert!(restored.in_flight.is_none());
    assert_eq!(restored.data, original);
    restored.install_frame(&frame(&extended)).unwrap();
    assert_eq!(
        restored.next_record().unwrap().unwrap().0.value,
        "saved rejected question"
    );
}

//! Owed-history recovery. Stored records stay in the account-owned rollout;
//! only the next real user turn brings bounded portions into model context.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use whisply_protocol::error::{CodexErr, Result as CodexResult};
use whisply_protocol::models::ResponseItem;
use whisply_protocol::protocol::{
    CompactedItem, RolloutItem, WhisplyHistoryRecoveryCursor, WhisplyHistoryRecoveryFrame,
    WhisplyHistoryRecoveryReceipt, WhisplyHistoryRecoveryRecord,
};

pub(crate) const MAX_FRAME_BYTES: usize = 192 * 1024;
// Existing direct runtime and Mac media boundaries. These bound each
// compaction request, never the retained conversation/archive length.
pub(crate) const MAX_INPUT_ITEMS: usize = 1_024;
pub(crate) const MAX_TEXT_PART_BYTES: usize = 200_000;
pub(crate) const MAX_IMAGES: usize = 4;
pub(crate) const MAX_IMAGE_BYTES: usize = 4 * 1_024 * 1_024;
pub(crate) const MAX_MODEL_INPUT_BYTES: usize = 12 * 1_024 * 1_024 - 256 * 1_024;
pub(crate) const ITEM_PREFIX: &str = "msg_whisply_recovered_";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum RecoverySpeaker {
    User,
    Assistant,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum RecoveryPartKind {
    Text,
    Image,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RecoveryRecord {
    pub speaker: RecoverySpeaker,
    pub kind: RecoveryPartKind,
    pub value: String,
}

pub(crate) fn invalid(message: &str) -> CodexErr {
    CodexErr::InvalidRequest(message.to_owned())
}
fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

#[derive(Default)]
pub(crate) struct HistoryRecoveryState {
    pub archive_id: Option<String>,
    data: Vec<u8>,
    pub committed: bool,
    pub archive_sha256: Option<String>,
    pub cursor: Option<WhisplyHistoryRecoveryCursor>,
    pub in_flight: Option<WhisplyHistoryRecoveryCursor>,
    pub in_flight_turn_id: Option<String>,
    pub invalid: bool,
}

impl HistoryRecoveryState {
    pub(crate) fn receipt(&self) -> Option<WhisplyHistoryRecoveryReceipt> {
        Some(WhisplyHistoryRecoveryReceipt {
            archive_id: self.archive_id.clone()?,
            accepted_bytes: if self.data.is_empty() {
                self.cursor
                    .as_ref()
                    .map_or(0, |cursor| cursor.record_offset)
            } else {
                self.data.len() as u64
            },
            committed: self.committed,
            hydrated: !self.invalid
                && self.committed
                && self.cursor.as_ref().is_some_and(|cursor| cursor.completed)
                && self.in_flight.is_none(),
            archive_sha256: self.archive_sha256.clone(),
        })
    }

    /// Validate before persistence; callers install the same frame only after
    /// its normal rollout append and flush succeed. Exact retries need no write.
    pub(crate) fn validate_frame(&self, frame: &WhisplyHistoryRecoveryFrame) -> CodexResult<bool> {
        if self.invalid
            || !valid_digest(&frame.archive_id)
            || frame.data_base64.len() > MAX_FRAME_BYTES.div_ceil(3) * 4
        {
            return Err(invalid("Invalid bounded history recovery frame"));
        }
        if self
            .archive_id
            .as_ref()
            .is_some_and(|id| id != &frame.archive_id)
            && self.committed
        {
            return Err(invalid("History recovery belongs to a different source"));
        }
        if self.cursor.as_ref().is_some_and(|cursor| cursor.completed) && self.in_flight.is_none() {
            return Ok(false);
        }
        if frame.final_byte_count.is_some() != frame.final_sha256.is_some()
            || frame
                .final_sha256
                .as_ref()
                .is_some_and(|hash| !valid_digest(hash))
        {
            return Err(invalid("History recovery final receipt is invalid"));
        }
        let bytes = STANDARD
            .decode(&frame.data_base64)
            .map_err(|_| invalid("Invalid history recovery frame encoding"))?;
        if bytes.len() > MAX_FRAME_BYTES {
            return Err(invalid("History recovery frame is too large"));
        }
        let same = self
            .archive_id
            .as_ref()
            .is_none_or(|id| id == &frame.archive_id);
        let existing = if same { self.data.as_slice() } else { &[] };
        if !same && frame.offset != 0 {
            return Err(invalid("History recovery source changed during transfer"));
        }
        let offset = usize::try_from(frame.offset)
            .map_err(|_| invalid("Invalid history recovery offset"))?;
        if offset > existing.len() {
            return Err(invalid("History recovery frames arrived out of order"));
        }
        let overlap = (existing.len() - offset).min(bytes.len());
        if existing[offset..offset + overlap] != bytes[..overlap] {
            return Err(invalid(
                "History recovery retry differs from its stored frame",
            ));
        }
        if let Some(count) = frame.final_byte_count {
            let length = existing
                .len()
                .checked_add(bytes.len() - overlap)
                .ok_or_else(|| invalid("History recovery length overflow"))?;
            if count == 0 || count != length as u64 {
                return Err(invalid("History recovery final length does not match"));
            }
            let mut digest = Sha256::new();
            digest.update(existing);
            digest.update(&bytes[overlap..]);
            if Some(format!("{:x}", digest.finalize())) != frame.final_sha256 {
                return Err(invalid("History recovery archive digest does not match"));
            }
            // No truncation or fabricated role promotion: every archived part
            // must be one original user/assistant text or image record.
            let mut archive = Vec::with_capacity(length);
            archive.extend_from_slice(existing);
            archive.extend_from_slice(&bytes[overlap..]);
            if !archive.ends_with(b"\n") {
                return Err(invalid("History recovery archive is incomplete"));
            }
            let mut record_count = 0;
            for line in archive[..archive.len() - 1].split(|byte| *byte == b'\n') {
                record_count += 1;
                let record: RecoveryRecord = serde_json::from_slice(line)
                    .map_err(|_| invalid("Stored history contains an invalid record"))?;
                if record.value.is_empty()
                    || (record.kind == RecoveryPartKind::Image
                        && (record.speaker != RecoverySpeaker::User
                            || !valid_inline_image(&record.value)))
                {
                    return Err(invalid("Stored history contains invalid content"));
                }
            }
            if record_count == 0 {
                return Err(invalid("Stored history recovery archive is empty"));
            }
        }
        Ok(!same
            || bytes.len() > overlap
            || (frame.final_byte_count.is_some()
                && (!self.committed || self.archive_sha256 != frame.final_sha256)))
    }

    pub(crate) fn install_frame(&mut self, frame: &WhisplyHistoryRecoveryFrame) -> CodexResult<()> {
        if !self.validate_frame(frame)? {
            return Ok(());
        }
        if self
            .archive_id
            .as_ref()
            .is_some_and(|id| id != &frame.archive_id)
        {
            *self = Self::default();
        }
        self.archive_id = Some(frame.archive_id.clone());
        let bytes = STANDARD
            .decode(&frame.data_base64)
            .map_err(|_| invalid("Invalid history recovery frame encoding"))?;
        let offset = usize::try_from(frame.offset)
            .map_err(|_| invalid("Invalid history recovery offset"))?;
        let overlap = (self.data.len() - offset).min(bytes.len());
        if bytes.len() > overlap {
            self.committed = false;
            self.archive_sha256 = None;
            self.data.extend_from_slice(&bytes[overlap..]);
            if let Some(cursor) = &mut self.cursor {
                cursor.completed = false;
            }
        }
        if frame.final_byte_count.is_some() {
            self.committed = true;
            self.archive_sha256 = frame.final_sha256.clone();
            self.cursor
                .get_or_insert_with(|| WhisplyHistoryRecoveryCursor {
                    archive_id: frame.archive_id.clone(),
                    ..Default::default()
                });
        }
        Ok(())
    }

    pub(crate) fn next_record(&self) -> CodexResult<Option<(RecoveryRecord, u64)>> {
        let cursor = self
            .cursor
            .as_ref()
            .ok_or_else(|| invalid("History recovery transfer is incomplete"))?;
        if !self.committed || self.invalid || self.archive_id.as_ref() != Some(&cursor.archive_id) {
            return Err(invalid("History recovery archive is not verified"));
        }
        let offset = usize::try_from(cursor.record_offset)
            .map_err(|_| invalid("Invalid history recovery cursor"))?;
        if offset > self.data.len() {
            return Err(invalid("History recovery cursor is outside its archive"));
        }
        if offset == self.data.len() {
            return Ok(None);
        }
        let length = self.data[offset..]
            .iter()
            .position(|byte| *byte == b'\n')
            .ok_or_else(|| invalid("Incomplete history recovery record"))?;
        let record: RecoveryRecord = serde_json::from_slice(&self.data[offset..offset + length])
            .map_err(|_| invalid("Stored history is not readable"))?;
        let text_offset = usize::try_from(cursor.text_offset)
            .map_err(|_| invalid("Invalid history recovery text offset"))?;
        if text_offset > record.value.len()
            || !record.value.is_char_boundary(text_offset)
            || (record.kind == RecoveryPartKind::Image && text_offset != 0)
        {
            return Err(invalid("History recovery cursor splits stored content"));
        }
        Ok(Some((record, (offset + length + 1) as u64)))
    }

    pub(crate) fn observe_checkpoint(&mut self, checkpoint: &CompactedItem) {
        if let Some(cursor) = &checkpoint.whisply_history_recovery {
            if self.archive_id.is_none()
                && cursor.completed
                && valid_digest(&cursor.archive_id)
                && cursor.text_offset == 0
                && cursor.record_offset > 0
            {
                self.archive_id = Some(cursor.archive_id.clone());
                self.committed = true;
            }
            if self.archive_id.as_ref() == Some(&cursor.archive_id) {
                self.cursor = Some(cursor.clone());
                if self
                    .in_flight
                    .as_ref()
                    .is_some_and(|pending| pending.compaction_id == cursor.compaction_id)
                {
                    self.in_flight = None;
                }
            } else {
                self.invalid = true;
            }
        }
    }

    pub(crate) fn observe_item(&mut self, item: &ResponseItem) {
        let Some(id) = item.id().and_then(|id| id.strip_prefix(ITEM_PREFIX)) else {
            return;
        };
        let mut fields = id.split('_');
        let (Some(archive_id), Some(record_offset), Some(text_offset), None) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            self.invalid = true;
            return;
        };
        if self.archive_id.as_deref() != Some(archive_id) {
            self.invalid = true;
            return;
        }
        let (Ok(record_offset), Ok(text_offset)) =
            (record_offset.parse::<u64>(), text_offset.parse::<u64>())
        else {
            self.invalid = true;
            return;
        };
        if let Some(cursor) = &mut self.cursor {
            if (record_offset, text_offset) < (cursor.record_offset, cursor.text_offset) {
                self.invalid = true;
                return;
            }
            cursor.record_offset = record_offset;
            cursor.text_offset = text_offset;
            cursor.completed = record_offset == self.data.len() as u64 && text_offset == 0;
        }
    }

    pub(crate) fn interrupted_compaction_for_continuation(
        &self,
        turn_id: &str,
    ) -> CodexResult<Option<String>> {
        let Some(pending) = &self.in_flight else {
            return Ok(None);
        };
        if self.in_flight_turn_id.as_deref() == Some(turn_id) {
            return Err(invalid(
                "History restoration was interrupted during this turn. Your saved history is intact; send a new message to continue with a new Usage-authorized attempt.",
            ));
        }
        pending
            .compaction_id
            .clone()
            .map(Some)
            .ok_or_else(|| invalid("The interrupted history attempt has no durable identity"))
    }

    fn observe_rejected_compaction(&mut self, compaction_id: &str) {
        if self
            .in_flight
            .as_ref()
            .and_then(|cursor| cursor.compaction_id.as_deref())
            != Some(compaction_id)
        {
            return;
        }
        self.in_flight = None;
        self.in_flight_turn_id = None;
        // Even at archive EOF, the fresh input that required this compaction
        // has not been recorded. Keep the recovery debt so the product can
        // append that saved failed question before the next distinct turn.
        // An explicit Retry may instead submit it once as current input.
        if let Some(cursor) = &mut self.cursor {
            cursor.completed = false;
        }
    }

    pub(crate) fn restore(items: &[RolloutItem]) -> Self {
        let mut state = Self::default();
        for item in items {
            match item {
                RolloutItem::WhisplyHistoryRecovery(WhisplyHistoryRecoveryRecord::Frame {
                    frame,
                }) => {
                    if state.install_frame(frame).is_err() {
                        state.invalid = true;
                    }
                }
                RolloutItem::WhisplyHistoryRecovery(
                    WhisplyHistoryRecoveryRecord::CompactionStarted { cursor, turn_id },
                ) => {
                    state.in_flight = Some(cursor.clone());
                    state.in_flight_turn_id = turn_id.clone();
                }
                RolloutItem::WhisplyHistoryRecovery(
                    WhisplyHistoryRecoveryRecord::CompactionRejected { compaction_id },
                ) => state.observe_rejected_compaction(compaction_id),
                RolloutItem::WhisplyHistoryRecovery(
                    WhisplyHistoryRecoveryRecord::CompactionContinued { compaction_id, .. },
                ) => {
                    if state
                        .in_flight
                        .as_ref()
                        .and_then(|cursor| cursor.compaction_id.as_ref())
                        == Some(compaction_id)
                    {
                        state.in_flight = None;
                        state.in_flight_turn_id = None;
                    }
                }
                RolloutItem::EventMsg(whisply_protocol::protocol::EventMsg::ThreadRolledBack(
                    _,
                )) if state.archive_id.is_some()
                    && !state.cursor.as_ref().is_some_and(|cursor| cursor.completed) =>
                {
                    // A legacy rollback can remove unsummarized ResponseItems.
                    // Never advance its archive cursor past content it removed.
                    state.invalid = true;
                }
                RolloutItem::Compacted(checkpoint) => state.observe_checkpoint(checkpoint),
                RolloutItem::ResponseItem(item) => state.observe_item(item),
                _ => {}
            }
        }
        state
    }
}

pub(crate) fn recovery_item_id(
    cursor: &WhisplyHistoryRecoveryCursor,
) -> whisply_protocol::ResponseItemId {
    whisply_protocol::ResponseItemId::with_suffix(
        "msg_whisply_recovered",
        format!(
            "{}_{}_{}",
            cursor.archive_id, cursor.record_offset, cursor.text_offset
        ),
    )
}

use super::session::Session;
use super::turn_context::TurnContext;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use whisply_protocol::models::{ContentItem, MessagePhase};

impl Session {
    pub(crate) async fn accept_history_recovery_frame(
        &self,
        frame: WhisplyHistoryRecoveryFrame,
    ) -> CodexResult<WhisplyHistoryRecoveryReceipt> {
        let active = self.active_turn.lock().await;
        if active.as_ref().is_some_and(|turn| turn.task.is_some()) {
            return Err(invalid(
                "Stored history cannot be restored while a turn is active",
            ));
        }
        let mut recovery = self.history_recovery.lock().await;
        if frame.offset == 0 && frame.data_base64.is_empty() && frame.final_byte_count.is_none() {
            if !valid_digest(&frame.archive_id) {
                return Err(invalid("Invalid history recovery source"));
            }
            return Ok(recovery.receipt().unwrap_or(WhisplyHistoryRecoveryReceipt {
                archive_id: frame.archive_id,
                accepted_bytes: 0,
                committed: false,
                hydrated: false,
                archive_sha256: None,
            }));
        }
        if recovery.archive_id.is_none() && !self.clone_history().await.raw_items().is_empty() {
            return Err(invalid(
                "Stored history can only be restored into its new empty runtime thread",
            ));
        }
        if recovery.validate_frame(&frame)? {
            let record = RolloutItem::WhisplyHistoryRecovery(WhisplyHistoryRecoveryRecord::Frame {
                frame: frame.clone(),
            });
            if !self.persist_rollout_items(&[record]).await {
                return Err(CodexErr::StorageFull);
            }
            recovery.install_frame(&frame)?;
        }
        self.flush_rollout().await?;
        recovery
            .receipt()
            .ok_or_else(|| invalid("Stored history transfer did not produce a receipt"))
    }

    /// Append recovered source data through the same durable rollout writer.
    /// The reserved ID is assigned after ordinary image/input normalization;
    /// untrusted normal writers cannot manufacture this cursor namespace.
    pub(crate) async fn append_history_recovery_item(
        &self,
        turn: &TurnContext,
        item: ResponseItem,
        mut cursor: WhisplyHistoryRecoveryCursor,
    ) -> CodexResult<()> {
        cursor.completed = {
            let recovery = self.history_recovery.lock().await;
            cursor.record_offset == recovery.data.len() as u64 && cursor.text_offset == 0
        };
        let source_items = [item];
        let (items, _) = self.prepare_conversation_items_for_history(turn, &source_items);
        let mut items = items.into_owned();
        if image_count(&source_items) != image_count(&items) {
            return Err(invalid(
                "Whisply could not read an original saved image. Its data is intact; it was not skipped or sent as a text placeholder.",
            ));
        }
        if !fits_request_media(self.clone_history().await.raw_items(), &items) {
            return Err(invalid(
                "The prepared saved image exceeds this request's media limit. The original history is intact.",
            ));
        }
        let Some(last) = items.last_mut() else {
            return Err(invalid("Stored history produced no model input"));
        };
        last.set_id(Some(recovery_item_id(&cursor)));
        let records: Vec<RolloutItem> = items
            .iter()
            .cloned()
            .map(RolloutItem::ResponseItem)
            .collect();
        if !self.persist_rollout_items(&records).await {
            return Err(CodexErr::StorageFull);
        }
        {
            let mut state = self.state.lock().await;
            state.record_items(items.iter(), turn.model_info.truncation_policy.into());
        }
        self.history_recovery.lock().await.cursor = Some(cursor);
        // A failed flush never authorizes the following model request. A same
        // process retry flushes this already-accepted append before continuing.
        self.flush_rollout().await?;
        self.recompute_token_usage(turn).await;
        Ok(())
    }

    pub(crate) async fn begin_history_recovery_compaction(&self, turn_id: &str) -> CodexResult<()> {
        let mut recovery = self.history_recovery.lock().await;
        let mut cursor = recovery
            .cursor
            .clone()
            .ok_or_else(|| invalid("Stored history recovery has no cursor"))?;
        cursor.compaction_id = Some(uuid::Uuid::new_v4().to_string());
        let item =
            RolloutItem::WhisplyHistoryRecovery(WhisplyHistoryRecoveryRecord::CompactionStarted {
                cursor: cursor.clone(),
                turn_id: Some(turn_id.to_owned()),
            });
        if !self.persist_rollout_items(&[item]).await {
            return Err(CodexErr::StorageFull);
        }
        // Remember an accepted intent before flushing: a flush failure must
        // not let this process start the same uncertain paid work again.
        recovery.in_flight = Some(cursor);
        recovery.in_flight_turn_id = Some(turn_id.to_owned());
        self.flush_rollout().await?;
        Ok(())
    }

    pub(crate) async fn continue_history_recovery_compaction(
        &self,
        turn_id: &str,
    ) -> CodexResult<()> {
        let mut recovery = self.history_recovery.lock().await;
        let Some(compaction_id) = recovery.interrupted_compaction_for_continuation(turn_id)? else {
            return Ok(());
        };
        let record = RolloutItem::WhisplyHistoryRecovery(
            WhisplyHistoryRecoveryRecord::CompactionContinued {
                compaction_id,
                turn_id: turn_id.to_owned(),
            },
        );
        if !self.persist_rollout_items(&[record]).await {
            return Err(CodexErr::StorageFull);
        }
        self.flush_rollout().await?;
        recovery.in_flight = None;
        recovery.in_flight_turn_id = None;
        Ok(())
    }

    pub(crate) async fn reject_history_recovery_compaction(&self) -> CodexResult<()> {
        let mut recovery = self.history_recovery.lock().await;
        if let Some(compaction_id) = recovery
            .in_flight
            .as_ref()
            .and_then(|cursor| cursor.compaction_id.clone())
        {
            let item = RolloutItem::WhisplyHistoryRecovery(
                WhisplyHistoryRecoveryRecord::CompactionRejected {
                    compaction_id: compaction_id.clone(),
                },
            );
            if !self.persist_rollout_items(&[item]).await {
                return Err(CodexErr::StorageFull);
            }
            self.flush_rollout().await?;
            recovery.observe_rejected_compaction(&compaction_id);
        }
        Ok(())
    }
}

pub(crate) fn response_item(record: &RecoveryRecord, value: String) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: match record.speaker {
            RecoverySpeaker::User => "user",
            RecoverySpeaker::Assistant => "assistant",
        }
        .to_string(),
        content: vec![match record.kind {
            RecoveryPartKind::Image => ContentItem::InputImage {
                image_url: value,
                detail: None,
            },
            RecoveryPartKind::Text if record.speaker == RecoverySpeaker::Assistant => {
                ContentItem::OutputText { text: value }
            }
            RecoveryPartKind::Text => ContentItem::InputText { text: value },
        }],
        phase: (record.speaker == RecoverySpeaker::Assistant).then_some(MessagePhase::FinalAnswer),
        internal_chat_message_metadata_passthrough: None,
    }
}

/// Largest complete character prefix whose normal runtime token estimate fits
/// the available selected-model window. No fixed per-turn text cap is imposed.
pub(crate) fn fitting_text_prefix(record: &RecoveryRecord, remainder: &str, room: i64) -> usize {
    let mut low = 0;
    let mut high = remainder.len().min(MAX_TEXT_PART_BYTES);
    while !remainder.is_char_boundary(high) {
        high -= 1;
    }
    while low < high {
        let mut end = low + (high - low).div_ceil(2);
        while end > low && !remainder.is_char_boundary(end) {
            end -= 1;
        }
        if end == low {
            end = remainder[low..]
                .chars()
                .next()
                .map_or(low, |ch| low + ch.len_utf8());
            if end > high {
                break;
            }
        }
        let item = response_item(record, remainder[..end].to_owned());
        if crate::context_manager::estimate_item_token_count(&item) < room {
            low = end;
        } else {
            high = end.saturating_sub(1);
            while high > low && !remainder.is_char_boundary(high) {
                high -= 1;
            }
        }
    }
    low
}

pub(crate) struct RecoveryRunningGuard(pub Arc<Session>);
impl Drop for RecoveryRunningGuard {
    fn drop(&mut self) {
        self.0
            .history_recovery_running
            .store(false, Ordering::Release);
    }
}

fn valid_inline_image(value: &str) -> bool {
    let Some((mime, encoded)) = value.split_once(";base64,") else {
        return false;
    };
    if !matches!(
        mime,
        "data:image/jpeg" | "data:image/png" | "data:image/webp"
    ) || encoded.len() > MAX_IMAGE_BYTES.div_ceil(3) * 4
    {
        return false;
    }
    STANDARD
        .decode(encoded)
        .is_ok_and(|bytes| !bytes.is_empty() && bytes.len() <= MAX_IMAGE_BYTES)
}

pub(crate) fn image_count(items: &[ResponseItem]) -> usize {
    items
        .iter()
        .map(|item| match item {
            ResponseItem::Message { content, .. } => content
                .iter()
                .filter(|part| matches!(part, ContentItem::InputImage { .. }))
                .count(),
            _ => 0,
        })
        .sum()
}

pub(crate) fn fits_request_media(history: &[ResponseItem], pending: &[ResponseItem]) -> bool {
    // Leave one input item for the normal synthesized compaction prompt and
    // envelope space for its instructions/metadata. Gateway checks remain final.
    history.iter().chain(pending).all(|item| match item {
        ResponseItem::Message { content, .. } => content.iter().all(|part| match part {
            ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                text.len() <= MAX_TEXT_PART_BYTES
            }
            _ => true,
        }),
        _ => true,
    }) && history.len().saturating_add(pending.len()) < MAX_INPUT_ITEMS
        && image_count(history).saturating_add(image_count(pending)) <= MAX_IMAGES
        && serde_json::to_vec(&(history, pending))
            .is_ok_and(|bytes| bytes.len() < MAX_MODEL_INPUT_BYTES)
}

#[cfg(test)]
#[path = "history_recovery_tests.rs"]
mod tests;

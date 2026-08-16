//! Coordinates asynchronous `/usage` cards in the chat widget.
//!
//! The slash command builds a composite history cell immediately, but the widget
//! keeps that cell transient while the account request runs. The transient card is
//! rendered above the composer through [`ChatWidget::pending_token_activity_output`]
//! so loading never requires clearing or rewriting transcript history. When the
//! matching response arrives, [`TokenActivityHandle`] updates the shared card state
//! and [`ChatWidget::finish_token_activity_refresh`] moves the cell into a completed
//! slot. Event dispatch commits that completed cell into history only after active
//! output and stream consolidation no longer block insertion.
//!
//! Pure chart rendering and date bucketing live in [`chart`]. This module owns
//! request correlation, transient/completed card state, and integration with
//! `ChatWidget` history insertion.

mod chart;

use std::sync::Arc;
use std::sync::RwLock;

use chrono::NaiveDate;
use chrono::Utc;
use codex_app_server_protocol::GetAccountTokenUsageResponse;
use ratatui::style::Stylize;
use ratatui::text::Line;

use super::ChatWidget;
use crate::app_event::AppEvent;
use crate::history_cell::CompositeHistoryCell;
use crate::history_cell::HistoryCell;
use crate::history_cell::PlainHistoryCell;
use crate::history_cell::plain_lines;

pub(crate) use chart::TokenActivityView;

/// Tracks the renderable lifecycle of one token activity history cell.
#[derive(Debug)]
enum TokenActivityState {
    Loading,
    Loaded {
        response: GetAccountTokenUsageResponse,
        today: NaiveDate,
    },
    /// Usage read from the Whisply-managed account through the native broker.
    ///
    /// Whisply meters plan windows rather than the dated token buckets the
    /// upstream chart draws, so this renders the projection as text instead of
    /// forcing it into a shape it does not have. It stays in this card so
    /// `/usage` keeps one lifecycle and one place in the transcript.
    LoadedManaged {
        body: String,
    },
    Error,
    /// A managed read that failed, carrying the reason the broker gave.
    ///
    /// Managed failures are actionable in a way the upstream account error is
    /// not: the app may be closed, or the account signed out.
    ManagedError {
        message: String,
    },
}

/// Completes an asynchronously rendered token activity history cell.
///
/// Clones share the same card state, allowing the background request path to
/// update a cell still owned by the widget's transient-output state. The widget
/// remains responsible for request-ID matching, redraws, and history insertion.
#[derive(Clone, Debug)]
pub(super) struct TokenActivityHandle {
    state: Arc<RwLock<TokenActivityState>>,
}

/// Holds the one transient token activity card waiting on its background response.
///
/// The request ID prevents late results from mutating a newer `/usage` card. The
/// cell stays out of transcript history until the matching response completes and
/// the widget confirms that active output no longer blocks insertion.
pub(super) struct PendingTokenActivityOutput {
    request_id: u64,
    cell: CompositeHistoryCell,
    handle: TokenActivityHandle,
}

impl TokenActivityHandle {
    /// Replaces the loading state with either fetched activity or an unavailable state.
    ///
    /// This method intentionally discards the error string because the TUI exposes
    /// one stable unavailable message. Calling it more than once replaces the prior
    /// terminal state, so request-ID matching should happen before completion.
    pub(super) fn finish(&self, result: Result<GetAccountTokenUsageResponse, String>) {
        self.finish_with_today(result, Utc::now().date_naive());
    }

    fn finish_with_today(
        &self,
        result: Result<GetAccountTokenUsageResponse, String>,
        today: NaiveDate,
    ) {
        let state = match result {
            Ok(response) => TokenActivityState::Loaded { response, today },
            Err(_) => TokenActivityState::Error,
        };
        #[expect(clippy::expect_used)]
        let mut current = self.state.write().expect("token activity state poisoned");
        *current = state;
    }

    /// Completes a card fed by the Whisply-managed usage projection.
    ///
    /// Unlike [`Self::finish`], the failure reason is kept: the broker
    /// distinguishes an unreachable app from a signed-out account, and the user
    /// needs to know which one happened.
    pub(super) fn finish_managed(&self, result: Result<String, String>) {
        let state = match result {
            Ok(body) => TokenActivityState::LoadedManaged { body },
            Err(message) => TokenActivityState::ManagedError { message },
        };
        #[expect(clippy::expect_used)]
        let mut current = self.state.write().expect("token activity state poisoned");
        *current = state;
    }
}

/// Renders one `/usage` card from shared asynchronous state.
#[derive(Debug)]
struct TokenActivityHistoryCell {
    view: TokenActivityView,
    /// Heading shown while loading and on failure. The managed projection is
    /// plan usage rather than dated token activity, so the two sources cannot
    /// share one label without misdescribing the contents.
    title: &'static str,
    state: Arc<RwLock<TokenActivityState>>,
}

/// Creates the card contents and completion handle for one `/usage` invocation.
///
/// The composite cell includes the echoed slash command and a loading card from
/// the start. Callers must retain the returned handle and complete it when the
/// matching background response arrives; otherwise the transient card stays loading.
pub(super) fn new_token_activity_output(
    view: TokenActivityView,
) -> (CompositeHistoryCell, TokenActivityHandle) {
    new_usage_output(
        view,
        format!("/usage {}", view.label().to_lowercase()),
        " Token activity",
    )
}

/// Creates the card for a Whisply-managed `/usage` invocation.
///
/// The managed projection is not split into the chart's daily/weekly views, so
/// the echoed command carries no view suffix. The card itself is the same one
/// the upstream path uses, which keeps `/usage` a single surface.
pub(super) fn new_managed_usage_output() -> (CompositeHistoryCell, TokenActivityHandle) {
    new_usage_output(TokenActivityView::Daily, "/usage".to_string(), " Usage")
}

fn new_usage_output(
    view: TokenActivityView,
    echoed_command: String,
    title: &'static str,
) -> (CompositeHistoryCell, TokenActivityHandle) {
    let command = PlainHistoryCell::new(vec![echoed_command.magenta().into()]);
    let state = Arc::new(RwLock::new(TokenActivityState::Loading));
    let handle = TokenActivityHandle {
        state: Arc::clone(&state),
    };
    let card = TokenActivityHistoryCell { view, title, state };
    (
        CompositeHistoryCell::new(vec![Box::new(command), Box::new(card)]),
        handle,
    )
}

impl HistoryCell for TokenActivityHistoryCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        #[expect(clippy::expect_used)]
        let state = self.state.read().expect("token activity state poisoned");
        match &*state {
            TokenActivityState::Loading => {
                vec![self.title.bold().into(), "   Loading...".dim().into()]
            }
            TokenActivityState::Error => vec![
                self.title.bold().into(),
                "   Token activity unavailable".dim().into(),
            ],
            TokenActivityState::Loaded { response, today } => {
                chart::loaded_lines(self.view, response, *today, width)
            }
            TokenActivityState::LoadedManaged { body } => {
                let mut lines: Vec<Line<'static>> = vec![self.title.bold().into()];
                lines.extend(
                    body.lines()
                        .map(|line| Line::<'static>::from(format!("   {line}"))),
                );
                lines
            }
            TokenActivityState::ManagedError { message } => vec![
                self.title.bold().into(),
                format!("   {message}").dim().into(),
            ],
        }
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        plain_lines(self.display_lines(u16::MAX))
    }
}

impl ChatWidget {
    /// Starts a Whisply-managed usage card and returns its request ID.
    ///
    /// The caller performs the broker read, which is blocking, and reports the
    /// result back with this ID so a late response cannot overwrite a newer card.
    pub(crate) fn add_managed_usage_output(&mut self) -> u64 {
        let (cell, handle) = new_managed_usage_output();
        self.install_pending_usage_card(cell, handle)
    }

    /// Starts a token activity refresh and replaces the current transient card.
    ///
    /// Each invocation receives a request ID so background responses update only
    /// their own card. The card remains outside transcript history until completion,
    /// which keeps loading visible without disturbing existing transcript content.
    pub(crate) fn add_token_activity_output(&mut self, view: TokenActivityView) {
        let (cell, handle) = new_token_activity_output(view);
        let request_id = self.install_pending_usage_card(cell, handle);
        self.app_event_tx
            .send(AppEvent::RefreshTokenActivity { request_id });
    }

    fn install_pending_usage_card(
        &mut self,
        cell: CompositeHistoryCell,
        handle: TokenActivityHandle,
    ) -> u64 {
        let request_id = self.next_token_activity_request_id;
        self.next_token_activity_request_id =
            self.next_token_activity_request_id.wrapping_add(/*rhs*/ 1);
        self.completed_token_activity_output = None;
        self.refreshing_token_activity_output = Some(PendingTokenActivityOutput {
            request_id,
            cell,
            handle,
        });
        self.bump_active_cell_revision();
        self.request_redraw();
        request_id
    }

    /// Returns the transient token activity card that should render above the composer.
    ///
    /// A loading card takes precedence over a completed card waiting for history
    /// insertion. Callers should render the returned cell but leave ownership with
    /// the widget so completion and insertion can update it safely.
    pub(super) fn pending_token_activity_output(&self) -> Option<&dyn HistoryCell> {
        self.refreshing_token_activity_output
            .as_ref()
            .map(|output| &output.cell as &dyn HistoryCell)
            .or_else(|| {
                self.completed_token_activity_output
                    .as_ref()
                    .map(|cell| cell as &dyn HistoryCell)
            })
    }

    /// Applies a background token activity result to its matching transient card.
    ///
    /// Returns `true` when the pending request matched and moved into the completed
    /// slot. Late responses return `false`, including responses for cards replaced
    /// by a newer `/usage` invocation or cleared during transcript changes.
    /// Completes a Whisply-managed usage card with the broker's projection.
    pub(crate) fn finish_managed_usage_refresh(
        &mut self,
        request_id: u64,
        result: Result<String, String>,
    ) -> bool {
        let Some(output) = self.take_matching_usage_card(request_id) else {
            return false;
        };
        output.handle.finish_managed(result);
        self.settle_usage_card(output);
        true
    }

    pub(crate) fn finish_token_activity_refresh(
        &mut self,
        request_id: u64,
        result: Result<GetAccountTokenUsageResponse, String>,
    ) -> bool {
        let Some(output) = self.take_matching_usage_card(request_id) else {
            return false;
        };
        output.handle.finish(result);
        self.settle_usage_card(output);
        true
    }

    fn take_matching_usage_card(&mut self, request_id: u64) -> Option<PendingTokenActivityOutput> {
        let output = self.refreshing_token_activity_output.take()?;
        if output.request_id != request_id {
            self.refreshing_token_activity_output = Some(output);
            return None;
        }
        Some(output)
    }

    fn settle_usage_card(&mut self, output: PendingTokenActivityOutput) {
        self.completed_token_activity_output = Some(output.cell);
        self.bump_active_cell_revision();
        self.request_redraw();
    }

    /// Reports whether completed asynchronous usage output must wait before insertion.
    ///
    /// Inserting while a stream, queued consolidation, or active transcript cell is
    /// present can reorder output relative to visible work, so callers retry once
    /// these barriers clear.
    pub(crate) fn usage_history_insertion_blocked(&self) -> bool {
        self.stream_controller.is_some()
            || self.plan_stream_controller.is_some()
            || self.pending_stream_consolidations > 0
            || self.transcript.active_cell.is_some()
            || self.active_hook_cell.is_some()
    }

    /// Records a stream consolidation barrier that delays token card insertion.
    ///
    /// Each queued consolidation should eventually call
    /// [`ChatWidget::note_stream_consolidation_completed`].
    pub(crate) fn note_stream_consolidation_queued(&mut self) {
        self.pending_stream_consolidations =
            self.pending_stream_consolidations.saturating_add(/*rhs*/ 1);
    }

    /// Releases one queued stream consolidation barrier.
    ///
    /// The counter saturates at zero so an unmatched completion does not underflow,
    /// but paired queue/completion calls are still the intended contract.
    pub(crate) fn note_stream_consolidation_completed(&mut self) {
        self.pending_stream_consolidations =
            self.pending_stream_consolidations.saturating_sub(/*rhs*/ 1);
    }

    /// Transfers the completed token activity card into the history insertion path.
    ///
    /// Callers should use this only after
    /// [`ChatWidget::usage_history_insertion_blocked`] returns `false`;
    /// taking the card removes it from the transient render area.
    pub(crate) fn take_completed_token_activity_output(&mut self) -> Option<CompositeHistoryCell> {
        let output = self.completed_token_activity_output.take()?;
        self.bump_active_cell_revision();
        Some(output)
    }

    /// Requests another insertion attempt when completed usage output is waiting.
    ///
    /// This is used after stream or history lifecycle events that may have cleared
    /// the insertion barriers without directly owning the completed output.
    pub(crate) fn request_pending_usage_output_insertion(&self) {
        if self.completed_token_activity_output.is_some()
            || self.pending_rate_limit_reset_hint().is_some()
        {
            self.app_event_tx.send(AppEvent::CommitPendingUsageOutput);
        }
    }

    pub(crate) fn request_pending_usage_output_insertion_after_stream_shutdown(&self) {
        if self.completed_token_activity_output.is_some()
            || self.pending_rate_limit_reset_hint().is_some()
        {
            self.app_event_tx
                .send(AppEvent::CommitPendingUsageOutputAfterStreamShutdown);
        }
    }

    /// Drops transient and completed token cards that must no longer update.
    ///
    /// Late background responses cannot mutate cards after a transcript reset,
    /// backtrack, or replacement flow clears this widget-owned state.
    pub(crate) fn clear_pending_token_activity_refreshes(&mut self) {
        let cleared_refresh = self.refreshing_token_activity_output.take().is_some();
        let cleared_completed = self.completed_token_activity_output.take().is_some();
        if cleared_refresh || cleared_completed {
            self.bump_active_cell_revision();
            self.request_redraw();
        }
    }
}

#[cfg(test)]
#[path = "tokens_tests.rs"]
mod tests;

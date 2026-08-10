//! Canonical app-server backed `whisply sessions list` and `sessions export`.
//!
//! This deliberately does not read rollout JSONL files directly. The app-server
//! is the only session authority, so archived and active session operations all
//! observe the same account home, name resolution, and migration behavior.

use std::cmp::Reverse;
use std::collections::BTreeSet;
use codex_app_server_protocol::Thread;
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::ThreadListParams;
use codex_app_server_protocol::ThreadSortKey;
use codex_app_server_protocol::UserInput;
use color_eyre::eyre::Result;
use color_eyre::eyre::WrapErr;
use color_eyre::eyre::eyre;
use serde::Serialize;

use crate::app_server_session::AppServerSession;
use crate::session_archive_commands::DeleteConfirmation;
use crate::session_archive_commands::SessionArchiveAction;
use crate::session_archive_commands::SessionArchiveCommandOptions;
use crate::session_archive_commands::resolve_session_target;
use crate::session_archive_commands::start_app_server_for_archive_command;
use crate::session_archive_commands::managed_runtime_home;

const MAX_SESSION_LIST_LIMIT: u32 = 200;
const APP_SERVER_PAGE_SIZE: u32 = 100;
const MAX_SESSION_LIST_PAGES_PER_COLLECTION: usize = 10;
const MAX_EXPORTED_MESSAGES: usize = 500;
const MAX_EXPORTED_MESSAGE_CHARS: usize = 8_000;
const MAX_EXPORTED_TOTAL_CHARS: usize = 1_000_000;
const TRANSCRIPT_SCHEMA_VERSION: u16 = 1;

/// Which persisted session collection to list. This mirrors the archive
/// commands instead of using a private CLI-only store.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionCollectionScope {
    Active,
    Archived,
    All,
}

impl SessionCollectionScope {
    fn archived_values(self) -> &'static [bool] {
        match self {
            Self::Active => &[false],
            Self::Archived => &[true],
            Self::All => &[false, true],
        }
    }
}

/// Non-sensitive, bounded list row. Paths, rollout locations, and user input
/// are intentionally absent from the list projection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionListEntry {
    pub id: String,
    pub name: Option<String>,
    pub preview: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub archived: bool,
    pub forked_from_id: Option<String>,
}

/// A redacted, portable public session export. It intentionally excludes
/// tool calls/results, local paths, images/audio, hook prompts, hidden
/// reasoning, plans, and non-text user input.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionExport {
    pub schema_version: u16,
    pub session: SessionExportMetadata,
    pub transcript: Vec<SessionTranscriptMessage>,
    pub transcript_truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionExportMetadata {
    pub id: String,
    pub session_id: String,
    pub name: Option<String>,
    pub preview: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub forked_from_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionTranscriptMessage {
    pub turn_id: String,
    pub role: SessionTranscriptRole,
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionTranscriptRole {
    User,
    Assistant,
}

pub async fn run_session_list_command(
    options: SessionArchiveCommandOptions,
    scope: SessionCollectionScope,
    limit: u32,
    search: Option<String>,
) -> Result<Vec<SessionListEntry>> {
    if !(1..=MAX_SESSION_LIST_LIMIT).contains(&limit) {
        return Err(eyre!(
            "session list limit must be between 1 and {MAX_SESSION_LIST_LIMIT}"
        ));
    }
    if search.as_deref().is_some_and(|value| value.len() > 128) {
        return Err(eyre!("session search must not exceed 128 characters"));
    }

    let codex_home = managed_runtime_home()?;
    let mut app_server = start_app_server_for_archive_command(options, codex_home.clone())
        .await
        .wrap_err("failed to start the managed session service")?;
    let result = list_with_app_server(&mut app_server, scope, limit, search.as_deref()).await;
    let shutdown_result = app_server.shutdown().await;
    let entries = result?;
    shutdown_result.wrap_err("failed to close the managed session service")?;
    Ok(entries)
}

async fn list_with_app_server(
    app_server: &mut AppServerSession,
    scope: SessionCollectionScope,
    limit: u32,
    search: Option<&str>,
) -> Result<Vec<SessionListEntry>> {
    let mut entries = Vec::new();
    for &archived in scope.archived_values() {
        let collection_start = entries.len();
        let mut cursor = None;
        let mut seen_cursors = BTreeSet::new();
        let mut pages = 0_usize;
        loop {
            if pages >= MAX_SESSION_LIST_PAGES_PER_COLLECTION {
                return Err(eyre!("managed session list exceeded its pagination budget"));
            }
            pages += 1;
            let response = app_server
                .thread_list(ThreadListParams {
                    cursor: cursor.take(),
                    limit: Some(APP_SERVER_PAGE_SIZE.min(limit)),
                    sort_key: Some(ThreadSortKey::UpdatedAt),
                    sort_direction: None,
                    model_providers: None,
                    // A public session list includes managed Mac/app-server
                    // threads as well as CLI sessions, but not subagents.
                    source_kinds: Some(crate::resume_source_kinds(
                        /*include_non_interactive*/ true,
                    )),
                    archived: Some(archived),
                    section_id: None,
                    cwd: None,
                    use_state_db_only: false,
                    search_term: search.map(ToOwned::to_owned),
                    parent_thread_id: None,
                    ancestor_thread_id: None,
                })
                .await
                .wrap_err("failed to list managed sessions")?;

            entries.extend(
                response
                    .data
                    .into_iter()
                    .filter(|thread| !thread.ephemeral)
                    .map(|thread| list_entry(thread, archived)),
            );
            cursor = response.next_cursor;
            if entries.len().saturating_sub(collection_start) >= limit as usize || cursor.is_none() {
                break;
            }
            let next_cursor = cursor
                .as_ref()
                .expect("cursor is present after the pagination termination check");
            if !seen_cursors.insert(next_cursor.clone()) {
                return Err(eyre!("managed session list returned a repeated cursor"));
            }
        }
    }
    entries.sort_by_key(|entry| (Reverse(entry.updated_at), entry.id.clone()));
    entries.truncate(limit as usize);
    Ok(entries)
}

pub async fn run_session_export_command(
    options: SessionArchiveCommandOptions,
    target: String,
) -> Result<SessionExport> {
    let codex_home = managed_runtime_home()?;
    let mut app_server = start_app_server_for_archive_command(options, codex_home.clone())
        .await
        .wrap_err("failed to start the managed session service")?;
    let result = export_with_app_server(&mut app_server, codex_home.as_path(), &target).await;
    let shutdown_result = app_server.shutdown().await;
    let export = result?;
    shutdown_result.wrap_err("failed to close the managed session service")?;
    Ok(export)
}

async fn export_with_app_server(
    app_server: &mut AppServerSession,
    codex_home: &std::path::Path,
    target: &str,
) -> Result<SessionExport> {
    let resolved = resolve_session_target(
        app_server,
        codex_home,
        // This resolver checks both collections without performing an action.
        SessionArchiveAction::Delete(DeleteConfirmation::Skip),
        target,
    )
    .await?;
    let thread = app_server
        .thread_read(resolved.session_id, /*include_turns*/ true)
        .await
        .wrap_err("failed to read the managed session for export")?;
    Ok(export_thread(thread))
}

fn list_entry(thread: Thread, archived: bool) -> SessionListEntry {
    SessionListEntry {
        id: thread.id,
        name: thread
            .name
            .map(|name| redact_and_bound_text(&name, 160)),
        preview: redact_and_bound_text(&thread.preview, 280),
        created_at: thread.created_at,
        updated_at: thread.updated_at,
        archived,
        forked_from_id: thread.forked_from_id,
    }
}

fn export_thread(thread: Thread) -> SessionExport {
    let mut transcript = Vec::new();
    let mut total_chars = 0;
    let mut truncated = false;
    for turn in thread.turns {
        for item in turn.items {
            let (role, message) = match item {
                ThreadItem::UserMessage { content, .. } => (
                    SessionTranscriptRole::User,
                    content
                    .into_iter()
                    .filter_map(|input| match input {
                        UserInput::Text { text, .. } => Some(text),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
                ),
                ThreadItem::AgentMessage { text, .. } => (SessionTranscriptRole::Assistant, text),
                _ => continue,
            };
            if message.is_empty() {
                continue;
            }
            if transcript.len() >= MAX_EXPORTED_MESSAGES || total_chars >= MAX_EXPORTED_TOTAL_CHARS {
                truncated = true;
                break;
            }
            let text = redact_and_bound_text(&message, MAX_EXPORTED_MESSAGE_CHARS);
            total_chars = total_chars.saturating_add(text.chars().count());
            if total_chars > MAX_EXPORTED_TOTAL_CHARS {
                truncated = true;
                break;
            }
            transcript.push(SessionTranscriptMessage {
                turn_id: turn.id.clone(),
                role,
                text,
            });
        }
        if truncated {
            break;
        }
    }
    SessionExport {
        schema_version: TRANSCRIPT_SCHEMA_VERSION,
        session: SessionExportMetadata {
            id: thread.id,
            session_id: thread.session_id,
            name: thread
                .name
                .map(|name| redact_and_bound_text(&name, 160)),
            preview: redact_and_bound_text(&thread.preview, 280),
            created_at: thread.created_at,
            updated_at: thread.updated_at,
            forked_from_id: thread.forked_from_id,
        },
        transcript,
        transcript_truncated: truncated,
    }
}

fn bounded_public_text(input: &str, max_chars: usize) -> String {
    let mut output = input.chars().take(max_chars).collect::<String>();
    if input.chars().nth(max_chars).is_some() {
        output.push_str("…");
    }
    output
}

fn redact_and_bound_text(input: &str, max_chars: usize) -> String {
    let mut output = String::new();
    for line in input.lines() {
        if line_has_sensitive_material(line) {
            output.push_str("[REDACTED SENSITIVE CONTENT]");
        } else {
            output.push_str(line);
        }
        output.push('\n');
        if output.chars().count() > max_chars {
            return bounded_public_text(&output, max_chars);
        }
    }
    bounded_public_text(output.trim_end_matches('\n'), max_chars)
}

fn line_has_sensitive_material(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    [
        "authorization:",
        "bearer ",
        "set-cookie:",
        "cookie:",
        "api_key",
        "api-key",
        "access_token",
        "refresh_token",
        "client_secret",
        "password=",
        "token=",
        "secret=",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
        || line.split_whitespace().any(|word| {
            word.starts_with("sk-")
                || word.starts_with("ghp_")
                || word.starts_with("xox")
                || word.starts_with("eyJ")
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_thread(items: Vec<ThreadItem>) -> Thread {
        Thread {
            id: "018f0000-0000-7000-8000-000000000001".to_string(),
            extra: None,
            session_id: "018f0000-0000-7000-8000-000000000001".to_string(),
            forked_from_id: None,
            parent_thread_id: None,
            preview: "preview\u{1b}[2J".to_string(),
            ephemeral: false,
            section: None,
            section_entered_at: None,
            history_mode: Default::default(),
            model_provider: "whisply".to_string(),
            created_at: 1,
            updated_at: 2,
            recency_at: None,
            status: codex_app_server_protocol::ThreadStatus::Idle,
            path: None,
            cwd: codex_utils_absolute_path::AbsolutePathBuf::from_absolute_path(
                std::env::temp_dir(),
            )
            .expect("temporary directory is absolute"),
            cli_version: "test".to_string(),
            source: codex_app_server_protocol::SessionSource::Cli,
            can_accept_direct_input: None,
            thread_source: None,
            agent_nickname: None,
            agent_role: None,
            git_info: None,
            name: Some("saved\u{1b}[31m session".to_string()),
            turns: vec![codex_app_server_protocol::Turn {
                id: "018f0000-0000-7000-8000-000000000002".to_string(),
                items,
                items_view: Default::default(),
                status: codex_app_server_protocol::TurnStatus::Completed,
                error: None,
                started_at: Some(1),
                completed_at: Some(2),
                duration_ms: Some(1),
            }],
        }
    }

    #[test]
    fn transcript_redacts_credential_markers() {
        let text = redact_and_bound_text(
            "safe line\nAuthorization: Bearer secret\napi_key=abc\nsk-private-value",
            500,
        );
        assert_eq!(
            text,
            "safe line\n[REDACTED SENSITIVE CONTENT]\n[REDACTED SENSITIVE CONTENT]\n[REDACTED SENSITIVE CONTENT]"
        );
    }

    #[test]
    fn public_text_is_bounded() {
        assert_eq!(bounded_public_text("abcdef", 3), "abc…");
    }

    #[test]
    fn all_scope_includes_both_collections() {
        assert_eq!(SessionCollectionScope::All.archived_values(), &[false, true]);
    }

    #[test]
    fn export_uses_only_user_and_assistant_text_and_redacts_secrets() {
        let thread = sample_thread(vec![
            ThreadItem::UserMessage {
                id: "user".to_string(),
                client_id: None,
                content: vec![UserInput::Text {
                    text: "Authorization: Bearer private".to_string(),
                    text_elements: Vec::new(),
                }],
            },
            ThreadItem::Reasoning {
                id: "reasoning".to_string(),
                summary: vec!["hidden reasoning".to_string()],
                content: vec!["private internal prompt".to_string()],
            },
            ThreadItem::AgentMessage {
                id: "assistant".to_string(),
                text: "Safe final answer".to_string(),
                phase: None,
                memory_citation: None,
            },
        ]);

        let export = export_thread(thread);

        assert_eq!(export.transcript.len(), 2);
        assert_eq!(export.transcript[0].role, SessionTranscriptRole::User);
        assert_eq!(
            export.transcript[0].text,
            "[REDACTED SENSITIVE CONTENT]"
        );
        assert_eq!(export.transcript[1].role, SessionTranscriptRole::Assistant);
        assert_eq!(export.transcript[1].text, "Safe final answer");
        let encoded = serde_json::to_string(&export).expect("export serializes");
        assert!(!encoded.contains("hidden reasoning"));
        assert!(!encoded.contains("private internal prompt"));
    }

}

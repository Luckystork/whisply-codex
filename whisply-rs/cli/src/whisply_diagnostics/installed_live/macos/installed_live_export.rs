//! Bounded redacted export for verified installed-app live diagnostics.
//!
//! The controller first verifies the complete private run bundle, then carries
//! only an allowlisted JSON projection into a new 0600 archive. Raw event
//! transcripts, structured logs, hashes, screenshots, accessibility values,
//! and visible copy never leave the owner-only run directory.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::io::Cursor;
use std::io::Write;
use std::path::Path;

use anyhow::Context;
use serde_json::Map;
use serde_json::Value;
use sha2::Digest as _;
use sha2::Sha256;
use whisply_utils_string::redact_log_line;
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

use super::LIVE_PROTOCOL;
use super::LiveSession;
use super::MAX_EVENT_LINES;
use super::MAX_EVIDENCE_BYTES;
use super::MAX_JSON_BYTES;
use super::canonical_uuid;
use super::collect_regular_file_hashes;
use super::hex_encode;
use super::read_private_json;
use super::sha256_file;
use super::valid_relative_path;
use super::write_new_private;

pub(super) const MAX_EXPORT_BYTES: usize = 64 * 1024 * 1024;
pub(super) const REDACTED_EXPORT_SCHEMA_VERSION: u64 = 2;
pub(super) const REDACTED_EVENT_JSONL_PATH: &str = "events.jsonl";
pub(super) const REDACTED_HASH_MANIFEST_PATH: &str = "hashes.sha256";
pub(super) const SAFE_JSON_PATHS: &[&str] = &[
    "run.json",
    "result.json",
    "authority.json",
    "animation.json",
    "authority/started.json",
    "authority/finished.json",
];
pub(super) const REDACTED_SCHEMA_PATHS: &[&str] = &[
    "schemas/export-manifest.json",
    "schemas/summary.json",
    "schemas/events.json",
    "schemas/screenshot.json",
];
pub(super) const REDACTED_SCREENSHOT_METADATA_PATHS: &[&str] =
    &["screenshots/started.json", "screenshots/finished.json"];
pub(super) const REDACTED_DERIVED_JSON_PATHS: &[&str] = &[
    "summary.json",
    "schemas/export-manifest.json",
    "schemas/summary.json",
    "schemas/events.json",
    "schemas/screenshot.json",
    "screenshots/started.json",
    "screenshots/finished.json",
];
pub(super) const RAW_EVIDENCE_PATHS: &[&str] = &[
    "events.jsonl",
    "structured-log.jsonl",
    "hashes.sha256",
    "task-usage.json",
];
pub(super) const RAW_EVIDENCE_PREFIXES: &[&str] = &[
    "accessibility/",
    "layout/",
    "screenshots/",
    "snapshots/",
    "visible-copy/",
];
const PRIVATE_TEXT_KEYS: &[&str] = &[
    "assistant",
    "assistanttext",
    "content",
    "errorbanner",
    "message",
    "prompt",
    "text",
    "transcript",
    "user",
    "visibleprompt",
    "visiblestate",
];
const CREDENTIAL_KEYS: &[&str] = &[
    "accesstoken",
    "apikey",
    "authorization",
    "bearer",
    "callbackurl",
    "clientsecret",
    "cookie",
    "credential",
    "password",
    "privatekey",
    "proxyauthorization",
    "refreshtoken",
    "secret",
    "sessionkey",
    "setcookie",
    "signingkey",
    "token",
];

pub(super) fn export_run(
    session: &LiveSession,
    run_id: &str,
    output: &Path,
) -> anyhow::Result<Value> {
    let run_id = canonical_uuid(run_id)?;
    let assertion = session.assert_run(&run_id)?;
    let run = session.run_directory(&run_id)?;

    let mut source_files = BTreeMap::new();
    let mut source_bytes = 0_u64;
    collect_regular_file_hashes(&run, &run, &mut source_files, &mut source_bytes)?;
    if source_bytes > MAX_EVIDENCE_BYTES {
        anyhow::bail!("Installed live diagnostic evidence is too large to export.");
    }

    let mut redaction_count = 0_u64;
    let mut archive_entries = BTreeMap::new();
    for relative in SAFE_JSON_PATHS {
        if !source_files.contains_key(*relative) {
            continue;
        }
        let value: Value = read_private_json(&run.join(relative))?;
        let (value, count) = redact_json(&value);
        redaction_count = redaction_count.saturating_add(count);
        archive_entries.insert((*relative).to_string(), json_entry_bytes(&value)?);
    }

    let event_records = session
        .event_summaries(&run_id, 0, MAX_EVENT_LINES)?
        .iter()
        .map(redacted_event_record)
        .collect::<anyhow::Result<Vec<_>>>()?;
    archive_entries.insert(
        REDACTED_EVENT_JSONL_PATH.to_string(),
        jsonl_entry_bytes(&event_records)?,
    );

    let screenshot_presence = REDACTED_SCREENSHOT_METADATA_PATHS
        .iter()
        .map(|relative| {
            let label = relative
                .strip_prefix("screenshots/")
                .and_then(|value| value.strip_suffix(".json"))
                .ok_or_else(|| anyhow::anyhow!("Redacted screenshot metadata path is invalid."))?;
            let source_path = format!("screenshots/{label}.png");
            let present = source_files.contains_key(&source_path);
            archive_entries.insert(
                (*relative).to_string(),
                json_entry_bytes(&serde_json::json!({
                    "schemaVersion": REDACTED_EXPORT_SCHEMA_VERSION,
                    "label": label,
                    "artifactPresent": present,
                    "pixelsExported": false,
                }))?,
            );
            Ok((label, present))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let screenshot_artifact_count = screenshot_presence
        .iter()
        .filter(|(_, present)| *present)
        .count();
    let result_category = redacted_result_category(
        assertion
            .get("result")
            .ok_or_else(|| anyhow::anyhow!("Installed live diagnostic result is invalid."))?,
    )?;
    archive_entries.insert(
        "summary.json".to_string(),
        json_entry_bytes(&serde_json::json!({
            "protocol": LIVE_PROTOCOL,
            "schemaVersion": REDACTED_EXPORT_SCHEMA_VERSION,
            "runID": run_id,
            "releaseManifestSha256": session.release_manifest_sha256,
            "result": result_category,
            "eventCount": event_records.len(),
            "screenshotArtifactCount": screenshot_artifact_count,
            "redacted": true,
            "exactArchive": false,
        }))?,
    );
    for (path, schema) in redacted_schema_entries() {
        archive_entries.insert(path.to_string(), json_entry_bytes(&schema)?);
    }

    let mut omitted_source_paths = BTreeSet::new();
    for relative in source_files.keys() {
        if is_excluded_evidence_path(relative) {
            omitted_source_paths.insert(relative.clone());
        }
    }
    for path in RAW_EVIDENCE_PATHS {
        omitted_source_paths.insert((*path).to_string());
    }
    archive_entries.insert(
        "export-manifest.json".to_string(),
        json_entry_bytes(&serde_json::json!({
            "protocol": LIVE_PROTOCOL,
            "schemaVersion": REDACTED_EXPORT_SCHEMA_VERSION,
            "runID": run_id,
            "releaseManifestSha256": session.release_manifest_sha256,
            "exportKind": "redacted-live-evidence",
            "exactArchive": false,
            "redactionCount": redaction_count,
            "sourceOmittedPaths": omitted_source_paths.into_iter().collect::<Vec<_>>(),
            "summaryPath": "summary.json",
            "eventsPath": REDACTED_EVENT_JSONL_PATH,
            "hashManifestPath": REDACTED_HASH_MANIFEST_PATH,
            "schemaPaths": REDACTED_SCHEMA_PATHS,
            "screenshotMetadataPaths": REDACTED_SCREENSHOT_METADATA_PATHS,
        }))?,
    );
    archive_entries.insert(
        REDACTED_HASH_MANIFEST_PATH.to_string(),
        redacted_archive_hash_manifest(&archive_entries),
    );

    let archive = archive_entry_bytes(&archive_entries)?;
    write_new_private(output, &archive)?;
    let archive_validation = super::live_export_validation::assert_export(
        output,
        Some(&run_id),
        Some(&session.release_manifest_sha256),
    )?;
    let replay_validation = super::live_export_validation::replay_export(
        output,
        Some(&run_id),
        Some(&session.release_manifest_sha256),
    )?;
    let retained_export = super::live_export_retention::retain_export(
        output,
        &run_id,
        &session.id,
        &session.release_manifest_sha256,
    )?;
    Ok(serde_json::json!({
        "ok": true,
        "protocol": LIVE_PROTOCOL,
        "runID": run_id,
        "releaseManifestSha256": session.release_manifest_sha256,
        "export": output.display().to_string(),
        "sha256": sha256_file(output)?,
        "redacted": true,
        "exactArchive": false,
        "schemaVersion": REDACTED_EXPORT_SCHEMA_VERSION,
        "redactionCount": redaction_count,
        "sourceOmittedPathCount": archive_entries
            .get("export-manifest.json")
            .and_then(|bytes| serde_json::from_slice::<Value>(bytes).ok())
            .and_then(|value| value.get("sourceOmittedPaths").cloned())
            .and_then(|value| value.as_array().map(Vec::len))
            .unwrap_or(0),
        "archiveValidation": archive_validation,
        "replayValidation": replay_validation,
        "retainedExport": retained_export,
        "evidence": assertion.get("evidence").cloned().unwrap_or(Value::Null),
    }))
}

pub(super) fn json_entry_bytes(value: &Value) -> anyhow::Result<Vec<u8>> {
    let mut json = serde_json::to_vec_pretty(value)?;
    json.push(b'\n');
    if json.len() > MAX_JSON_BYTES {
        anyhow::bail!("Redacted live diagnostic export entry is too large.");
    }
    Ok(json)
}

pub(super) fn jsonl_entry_bytes(values: &[Value]) -> anyhow::Result<Vec<u8>> {
    let mut jsonl = Vec::new();
    for value in values {
        let json = serde_json::to_vec(value)?;
        if json.len() > MAX_JSON_BYTES {
            anyhow::bail!("Redacted live diagnostic export entry is too large.");
        }
        jsonl.extend_from_slice(&json);
        jsonl.push(b'\n');
        if jsonl.len() > MAX_JSON_BYTES {
            anyhow::bail!("Redacted live diagnostic export entry is too large.");
        }
    }
    Ok(jsonl)
}

/// Test and legacy-v1 helper for JSON-only redacted archives. New exports use
/// [`archive_entry_bytes`] so their JSONL and hash-manifest bytes remain
/// deterministic too.
pub(super) fn archive_bytes(entries: &BTreeMap<String, Value>) -> anyhow::Result<Vec<u8>> {
    let bytes = entries
        .iter()
        .map(|(relative, value)| Ok((relative.clone(), json_entry_bytes(value)?)))
        .collect::<anyhow::Result<BTreeMap<_, _>>>()?;
    archive_entry_bytes(&bytes)
}

pub(super) fn archive_entry_bytes(entries: &BTreeMap<String, Vec<u8>>) -> anyhow::Result<Vec<u8>> {
    let cursor = Cursor::new(Vec::new());
    let mut archive = ZipWriter::new(cursor);
    let options = SimpleFileOptions::default();
    for (relative, bytes) in entries {
        if !valid_relative_path(relative) || bytes.len() > MAX_JSON_BYTES {
            anyhow::bail!("Redacted live diagnostic export entry is too large.");
        }
        archive
            .start_file(relative, options)
            .context("Unable to create the redacted live diagnostic archive.")?;
        archive
            .write_all(bytes)
            .context("Unable to create the redacted live diagnostic archive.")?;
    }
    let bytes = archive
        .finish()
        .context("Unable to create the redacted live diagnostic archive.")?
        .into_inner();
    if bytes.len() > MAX_EXPORT_BYTES {
        anyhow::bail!("Redacted live diagnostic export exceeds 64 MiB.");
    }
    Ok(bytes)
}

pub(super) fn redacted_archive_hash_manifest(entries: &BTreeMap<String, Vec<u8>>) -> Vec<u8> {
    let mut manifest = String::new();
    for (relative, bytes) in entries {
        let digest = Sha256::digest(bytes);
        manifest.push_str(&hex_encode(&digest));
        manifest.push_str("  ");
        manifest.push_str(relative);
        manifest.push('\n');
    }
    manifest.into_bytes()
}

fn redacted_event_record(summary: &Value) -> anyhow::Result<Value> {
    let sequence = summary
        .get("sequence")
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow::anyhow!("Installed live diagnostic event evidence is invalid."))?;
    let timestamp_ms = summary
        .get("timestampMs")
        .and_then(Value::as_i64)
        .filter(|timestamp_ms| *timestamp_ms >= 0)
        .ok_or_else(|| anyhow::anyhow!("Installed live diagnostic event evidence is invalid."))?;
    let name = summary
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty() && name.len() <= 160)
        .ok_or_else(|| anyhow::anyhow!("Installed live diagnostic event evidence is invalid."))?;

    Ok(serde_json::json!({
        "sequence": sequence,
        "timestampMs": timestamp_ms,
        "kind": redacted_event_category(name),
    }))
}

fn redacted_event_category(name: &str) -> &'static str {
    match name {
        "run.started" => "run-started",
        "run.finished" => "run-finished",
        "provider.usage" => "provider-usage",
        _ if name.starts_with("computer-use.") => "computer-use",
        _ if name.starts_with("diagnostic.") => "diagnostic-artifact",
        _ => "other",
    }
}

fn redacted_result_category(value: &Value) -> anyhow::Result<&'static str> {
    match value.as_str() {
        Some("completed") | Some("finished") => Ok("completed"),
        Some("cancelled") => Ok("cancelled"),
        Some("failed") => Ok("failed"),
        Some("incomplete") | Some("detached") | Some("unknown") => Ok("incomplete"),
        _ => anyhow::bail!("Installed live diagnostic result is invalid."),
    }
}

pub(super) fn redacted_schema_entries() -> [(&'static str, Value); 4] {
    [
        (
            "schemas/export-manifest.json",
            serde_json::json!({
                "schemaVersion": REDACTED_EXPORT_SCHEMA_VERSION,
                "kind": "export-manifest",
                "required": [
                    "protocol",
                    "schemaVersion",
                    "runID",
                    "releaseManifestSha256",
                    "exportKind",
                    "exactArchive",
                    "redactionCount",
                    "sourceOmittedPaths",
                    "summaryPath",
                    "eventsPath",
                    "hashManifestPath",
                    "schemaPaths",
                    "screenshotMetadataPaths",
                ],
            }),
        ),
        (
            "schemas/summary.json",
            serde_json::json!({
                "schemaVersion": REDACTED_EXPORT_SCHEMA_VERSION,
                "kind": "summary",
                "required": [
                    "protocol",
                    "schemaVersion",
                    "runID",
                    "releaseManifestSha256",
                    "result",
                    "eventCount",
                    "screenshotArtifactCount",
                    "redacted",
                    "exactArchive",
                ],
            }),
        ),
        (
            "schemas/events.json",
            serde_json::json!({
                "schemaVersion": REDACTED_EXPORT_SCHEMA_VERSION,
                "kind": "events-jsonl",
                "recordFields": ["sequence", "timestampMs", "kind"],
                "kinds": [
                    "run-started",
                    "run-finished",
                    "provider-usage",
                    "computer-use",
                    "diagnostic-artifact",
                    "other",
                ],
            }),
        ),
        (
            "schemas/screenshot.json",
            serde_json::json!({
                "schemaVersion": REDACTED_EXPORT_SCHEMA_VERSION,
                "kind": "screenshot-metadata",
                "required": ["schemaVersion", "label", "artifactPresent", "pixelsExported"],
            }),
        ),
    ]
}

fn redact_json(value: &Value) -> (Value, u64) {
    redact_json_value(value, None)
}

fn redact_json_value(value: &Value, key: Option<&str>) -> (Value, u64) {
    if key.is_some_and(|key| should_redact_value(key, value)) {
        return (Value::String("<redacted>".to_string()), 1);
    }
    match value {
        Value::Array(values) => {
            let mut redaction_count = 0_u64;
            let values = values
                .iter()
                .map(|value| {
                    let (value, count) = redact_json_value(value, None);
                    redaction_count = redaction_count.saturating_add(count);
                    value
                })
                .collect();
            (Value::Array(values), redaction_count)
        }
        Value::Object(values) => {
            let mut redaction_count = 0_u64;
            let mut output = Map::new();
            for (key, value) in values {
                let (value, count) = redact_json_value(value, Some(key));
                redaction_count = redaction_count.saturating_add(count);
                output.insert(key.clone(), value);
            }
            (Value::Object(output), redaction_count)
        }
        Value::String(value) => {
            let redacted = redact_log_line(value);
            if redacted.as_ref() == value {
                (Value::String(value.clone()), 0)
            } else {
                (Value::String(redacted.into_owned()), 1)
            }
        }
        _ => (value.clone(), 0),
    }
}

pub(super) fn should_redact_value(key: &str, value: &Value) -> bool {
    let normalized = key
        .bytes()
        .filter(u8::is_ascii_alphanumeric)
        .map(char::from)
        .collect::<String>()
        .to_ascii_lowercase();
    PRIVATE_TEXT_KEYS.contains(&normalized.as_str())
        || (CREDENTIAL_KEYS
            .iter()
            .any(|needle| normalized.contains(needle))
            && !matches!(value, Value::Null | Value::Bool(_) | Value::Number(_)))
}

pub(super) fn is_excluded_evidence_path(relative: &str) -> bool {
    RAW_EVIDENCE_PATHS.contains(&relative)
        || RAW_EVIDENCE_PREFIXES
            .iter()
            .any(|prefix| relative.starts_with(prefix))
        || !SAFE_JSON_PATHS.contains(&relative)
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use pretty_assertions::assert_eq;
    use zip::ZipArchive;

    use super::*;

    #[test]
    fn redaction_preserves_measurements_but_removes_visible_and_credential_text() {
        let (value, count) = redact_json(&serde_json::json!({
            "inputTokens": 42,
            "request": { "prompt": "private prompt" },
            "visibleState": { "assistantText": "private response" },
            "authorization": "Bearer private-secret",
            "safeSummary": "usage completed",
        }));

        assert_eq!(
            value,
            serde_json::json!({
                "inputTokens": 42,
                "request": { "prompt": "<redacted>" },
                "visibleState": "<redacted>",
                "authorization": "<redacted>",
                "safeSummary": "usage completed",
            })
        );
        assert_eq!(count, 3);
    }

    #[test]
    fn archive_contains_only_the_supplied_redacted_json_entries() {
        let entries = BTreeMap::from([
            (
                "run.json".to_string(),
                serde_json::json!({ "prompt": "<redacted>" }),
            ),
            (
                "export-manifest.json".to_string(),
                serde_json::json!({ "exactArchive": false }),
            ),
        ]);
        let bytes = archive_bytes(&entries).expect("archive");
        let mut archive = ZipArchive::new(Cursor::new(bytes)).expect("open archive");
        let mut run = String::new();
        archive
            .by_name("run.json")
            .expect("run entry")
            .read_to_string(&mut run)
            .expect("read run entry");

        assert_eq!(archive.len(), 2);
        assert!(run.contains("<redacted>"));
        assert!(!run.contains("private prompt"));
    }
}

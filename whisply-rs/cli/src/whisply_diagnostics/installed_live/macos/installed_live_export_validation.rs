//! Offline validation for bounded redacted installed-app diagnostic archives.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::io::Cursor;
use std::io::Read;
use std::path::Path;

use anyhow::Context;
use serde_json::Value;
use sha2::Digest as _;
use sha2::Sha256;
use whisply_utils_string::redact_log_line;
use zip::ZipArchive;

use super::LIVE_PROTOCOL;
use super::MAX_EVENT_LINES;
use super::MAX_JSON_BYTES;
use super::canonical_uuid;
use super::hex_encode;
use super::live_export::MAX_EXPORT_BYTES;
use super::live_export::RAW_EVIDENCE_PATHS;
use super::live_export::REDACTED_DERIVED_JSON_PATHS;
use super::live_export::REDACTED_EVENT_JSONL_PATH;
use super::live_export::REDACTED_EXPORT_SCHEMA_VERSION;
use super::live_export::REDACTED_HASH_MANIFEST_PATH;
use super::live_export::REDACTED_SCHEMA_PATHS;
use super::live_export::REDACTED_SCREENSHOT_METADATA_PATHS;
use super::live_export::SAFE_JSON_PATHS;
use super::live_export::is_excluded_evidence_path;
use super::live_export::redacted_schema_entries;
use super::live_export::should_redact_value;
use super::read_private_bytes;
use super::sha256_file;
use super::valid_lowercase_sha256;
use super::valid_relative_path;

const REDACTION_MARKER: &str = "<redacted>";
const MAX_V1_ARCHIVE_ENTRIES: usize = SAFE_JSON_PATHS.len() + 1;
const MAX_V2_ARCHIVE_ENTRIES: usize = SAFE_JSON_PATHS.len() + REDACTED_DERIVED_JSON_PATHS.len() + 3;
const REDACTED_EVENT_KINDS: &[&str] = &[
    "run-started",
    "run-finished",
    "provider-usage",
    "computer-use",
    "diagnostic-artifact",
    "other",
];
const REDACTED_RESULT_KINDS: &[&str] = &["completed", "cancelled", "failed", "incomplete", "other"];

#[derive(Debug)]
struct ValidatedExport {
    run_id: String,
    release_manifest_sha256: String,
    schema_version: u64,
    redaction_count: u64,
    omitted_path_count: usize,
    outcome: Value,
}

pub(super) fn assert_export(
    path: &Path,
    expected_run_id: Option<&str>,
    expected_release_manifest_sha256: Option<&str>,
) -> anyhow::Result<Value> {
    let export = read_validated_export(path, expected_run_id, expected_release_manifest_sha256)?;
    Ok(serde_json::json!({
        "ok": true,
        "protocol": LIVE_PROTOCOL,
        "export": path.display().to_string(),
        "sha256": sha256_file(path)?,
        "runID": export.run_id,
        "releaseManifestSha256": export.release_manifest_sha256,
        "redacted": true,
        "exactArchive": false,
        "schemaVersion": export.schema_version,
        "redactionCount": export.redaction_count,
        "omittedPathCount": export.omitted_path_count,
    }))
}

pub(super) fn replay_export(
    path: &Path,
    expected_run_id: Option<&str>,
    expected_release_manifest_sha256: Option<&str>,
) -> anyhow::Result<Value> {
    let export = read_validated_export(path, expected_run_id, expected_release_manifest_sha256)?;
    Ok(serde_json::json!({
        "ok": true,
        "protocol": LIVE_PROTOCOL,
        "export": path.display().to_string(),
        "sha256": sha256_file(path)?,
        "runID": export.run_id,
        "releaseManifestSha256": export.release_manifest_sha256,
        "redacted": true,
        "exactArchive": false,
        "schemaVersion": export.schema_version,
        "redactionCount": export.redaction_count,
        "omittedPathCount": export.omitted_path_count,
        "replayable": true,
        "replayContract": format!("redacted-live-evidence-v{}", export.schema_version),
        "result": export.outcome,
    }))
}

fn read_validated_export(
    path: &Path,
    expected_run_id: Option<&str>,
    expected_release_manifest_sha256: Option<&str>,
) -> anyhow::Result<ValidatedExport> {
    let bytes = read_private_bytes(path, MAX_EXPORT_BYTES)?;
    validate_export_bytes(&bytes, expected_run_id, expected_release_manifest_sha256)
}

fn validate_export_bytes(
    bytes: &[u8],
    expected_run_id: Option<&str>,
    expected_release_manifest_sha256: Option<&str>,
) -> anyhow::Result<ValidatedExport> {
    let entries = read_archive_entries(bytes)?;
    let export_manifest = json_archive_entry(&entries, "export-manifest.json")?;
    match export_manifest.get("schemaVersion").and_then(Value::as_u64) {
        Some(1) => validate_v1_export(
            &entries,
            &export_manifest,
            expected_run_id,
            expected_release_manifest_sha256,
        ),
        Some(REDACTED_EXPORT_SCHEMA_VERSION) => validate_v2_export(
            &entries,
            &export_manifest,
            expected_run_id,
            expected_release_manifest_sha256,
        ),
        _ => anyhow::bail!("Redacted live diagnostic archive manifest is invalid."),
    }
}

fn read_archive_entries(bytes: &[u8]) -> anyhow::Result<BTreeMap<String, Vec<u8>>> {
    let mut archive = ZipArchive::new(Cursor::new(bytes))
        .context("Unable to read the redacted live diagnostic archive.")?;
    if archive.len() > MAX_V2_ARCHIVE_ENTRIES {
        anyhow::bail!("Redacted live diagnostic archive contains too many entries.");
    }

    let mut entries = BTreeMap::new();
    let mut names = BTreeSet::new();
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .context("Unable to read the redacted live diagnostic archive.")?;
        let name = entry.name().to_string();
        if entry.is_dir()
            || !valid_relative_path(&name)
            || name.len() > 256
            || !names.insert(name.clone())
        {
            anyhow::bail!("Redacted live diagnostic archive has an invalid entry.");
        }
        if entry.size() > u64::try_from(MAX_JSON_BYTES).unwrap_or(u64::MAX) {
            anyhow::bail!("Redacted live diagnostic archive entry is too large.");
        }
        let mut contents = Vec::with_capacity(entry.size() as usize);
        entry
            .read_to_end(&mut contents)
            .context("Unable to read the redacted live diagnostic archive.")?;
        if contents.len() > MAX_JSON_BYTES {
            anyhow::bail!("Redacted live diagnostic archive entry is too large.");
        }
        entries.insert(name, contents);
    }
    Ok(entries)
}

fn validate_v1_export(
    entries: &BTreeMap<String, Vec<u8>>,
    export_manifest: &Value,
    expected_run_id: Option<&str>,
    expected_release_manifest_sha256: Option<&str>,
) -> anyhow::Result<ValidatedExport> {
    if entries.len() > MAX_V1_ARCHIVE_ENTRIES
        || entries.keys().any(|path| !allowed_v1_archive_path(path))
    {
        anyhow::bail!("Redacted live diagnostic archive has an invalid entry.");
    }
    for path in entries.keys() {
        let _ = json_archive_entry(entries, path)?;
    }

    let run_manifest = json_archive_entry(entries, "run.json")?;
    let result_manifest = json_archive_entry(entries, "result.json")?;
    let _ = json_archive_entry(entries, "authority.json")?;

    let run_id = manifest_run_id(export_manifest)?;
    let release_manifest_sha256 = manifest_release_hash(export_manifest)?;
    if export_manifest.get("protocol").and_then(Value::as_str) != Some(LIVE_PROTOCOL)
        || export_manifest.get("schemaVersion").and_then(Value::as_u64) != Some(1)
        || export_manifest.get("exportKind").and_then(Value::as_str)
            != Some("redacted-live-evidence")
        || export_manifest.get("exactArchive").and_then(Value::as_bool) != Some(false)
    {
        anyhow::bail!("Redacted live diagnostic archive manifest is invalid.");
    }
    let redaction_count = export_manifest
        .get("redactionCount")
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow::anyhow!("Redacted live diagnostic archive manifest is invalid."))?;
    let omitted_paths = validated_omitted_paths(export_manifest, "omittedPaths")?;

    validate_exported_run_metadata(&run_manifest, &run_id, &release_manifest_sha256)?;
    validate_exported_result_metadata(&result_manifest, &run_id, &release_manifest_sha256)?;
    validate_expected_identity(
        &run_id,
        &release_manifest_sha256,
        expected_run_id,
        expected_release_manifest_sha256,
    )?;

    Ok(ValidatedExport {
        run_id,
        release_manifest_sha256,
        schema_version: 1,
        redaction_count,
        omitted_path_count: omitted_paths.len(),
        outcome: result_manifest
            .get("result")
            .cloned()
            .unwrap_or(Value::Null),
    })
}

fn validate_v2_export(
    entries: &BTreeMap<String, Vec<u8>>,
    export_manifest: &Value,
    expected_run_id: Option<&str>,
    expected_release_manifest_sha256: Option<&str>,
) -> anyhow::Result<ValidatedExport> {
    if entries.len() > MAX_V2_ARCHIVE_ENTRIES
        || entries.keys().any(|path| !allowed_v2_archive_path(path))
    {
        anyhow::bail!("Redacted live diagnostic archive has an invalid entry.");
    }
    for required in [
        "run.json",
        "result.json",
        "authority.json",
        "summary.json",
        REDACTED_EVENT_JSONL_PATH,
        REDACTED_HASH_MANIFEST_PATH,
    ]
    .into_iter()
    .chain(REDACTED_SCHEMA_PATHS.iter().copied())
    .chain(REDACTED_SCREENSHOT_METADATA_PATHS.iter().copied())
    {
        if !entries.contains_key(required) {
            anyhow::bail!("Redacted live diagnostic archive is missing required evidence.");
        }
    }
    for path in entries
        .keys()
        .filter(|path| path.as_str() != REDACTED_EVENT_JSONL_PATH)
        .filter(|path| path.as_str() != REDACTED_HASH_MANIFEST_PATH)
    {
        let _ = json_archive_entry(entries, path)?;
    }

    let run_manifest = json_archive_entry(entries, "run.json")?;
    let result_manifest = json_archive_entry(entries, "result.json")?;
    let _ = json_archive_entry(entries, "authority.json")?;
    let run_id = manifest_run_id(export_manifest)?;
    let release_manifest_sha256 = manifest_release_hash(export_manifest)?;
    if !has_exact_object_keys(
        export_manifest,
        &[
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
    ) || export_manifest.get("protocol").and_then(Value::as_str) != Some(LIVE_PROTOCOL)
        || export_manifest.get("schemaVersion").and_then(Value::as_u64)
            != Some(REDACTED_EXPORT_SCHEMA_VERSION)
        || export_manifest.get("exportKind").and_then(Value::as_str)
            != Some("redacted-live-evidence")
        || export_manifest.get("exactArchive").and_then(Value::as_bool) != Some(false)
        || export_manifest
            .get("redactionCount")
            .and_then(Value::as_u64)
            .is_none()
        || export_manifest.get("summaryPath").and_then(Value::as_str) != Some("summary.json")
        || export_manifest.get("eventsPath").and_then(Value::as_str)
            != Some(REDACTED_EVENT_JSONL_PATH)
        || export_manifest
            .get("hashManifestPath")
            .and_then(Value::as_str)
            != Some(REDACTED_HASH_MANIFEST_PATH)
        || !has_exact_string_array(export_manifest.get("schemaPaths"), REDACTED_SCHEMA_PATHS)
        || !has_exact_string_array(
            export_manifest.get("screenshotMetadataPaths"),
            REDACTED_SCREENSHOT_METADATA_PATHS,
        )
    {
        anyhow::bail!("Redacted live diagnostic archive manifest is invalid.");
    }
    let omitted_paths = validated_omitted_paths(export_manifest, "sourceOmittedPaths")?;

    validate_exported_run_metadata(&run_manifest, &run_id, &release_manifest_sha256)?;
    validate_exported_result_metadata(&result_manifest, &run_id, &release_manifest_sha256)?;
    let source_result = source_result_category(&result_manifest)?;

    let summary = json_archive_entry(entries, "summary.json")?;
    if !has_exact_object_keys(
        &summary,
        &[
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
    ) || summary.get("protocol").and_then(Value::as_str) != Some(LIVE_PROTOCOL)
        || summary.get("schemaVersion").and_then(Value::as_u64)
            != Some(REDACTED_EXPORT_SCHEMA_VERSION)
        || summary.get("runID").and_then(Value::as_str) != Some(run_id.as_str())
        || summary.get("releaseManifestSha256").and_then(Value::as_str)
            != Some(release_manifest_sha256.as_str())
        || summary.get("result").and_then(Value::as_str) != Some(source_result)
        || summary
            .get("result")
            .and_then(Value::as_str)
            .is_none_or(|result| !REDACTED_RESULT_KINDS.contains(&result))
        || summary.get("redacted").and_then(Value::as_bool) != Some(true)
        || summary.get("exactArchive").and_then(Value::as_bool) != Some(false)
    {
        anyhow::bail!("Redacted live diagnostic summary is invalid.");
    }

    for (path, expected) in redacted_schema_entries() {
        if json_archive_entry(entries, path)? != expected {
            anyhow::bail!("Redacted live diagnostic archive schema is invalid.");
        }
    }
    let screenshot_artifact_count = REDACTED_SCREENSHOT_METADATA_PATHS
        .iter()
        .map(|path| validate_screenshot_metadata(path, &json_archive_entry(entries, path)?))
        .collect::<anyhow::Result<Vec<_>>>()?
        .into_iter()
        .filter(|present| *present)
        .count();
    let event_count = validate_redacted_events(entries)?;
    if summary.get("eventCount").and_then(Value::as_u64)
        != Some(u64::try_from(event_count).unwrap_or(u64::MAX))
        || summary
            .get("screenshotArtifactCount")
            .and_then(Value::as_u64)
            != Some(u64::try_from(screenshot_artifact_count).unwrap_or(u64::MAX))
    {
        anyhow::bail!("Redacted live diagnostic summary is invalid.");
    }
    validate_redacted_hash_manifest(entries)?;
    validate_expected_identity(
        &run_id,
        &release_manifest_sha256,
        expected_run_id,
        expected_release_manifest_sha256,
    )?;

    Ok(ValidatedExport {
        run_id,
        release_manifest_sha256,
        schema_version: REDACTED_EXPORT_SCHEMA_VERSION,
        redaction_count: export_manifest
            .get("redactionCount")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        omitted_path_count: omitted_paths.len(),
        outcome: summary.get("result").cloned().unwrap_or(Value::Null),
    })
}

fn allowed_v1_archive_path(path: &str) -> bool {
    path == "export-manifest.json" || SAFE_JSON_PATHS.contains(&path)
}

fn allowed_v2_archive_path(path: &str) -> bool {
    path == "export-manifest.json"
        || SAFE_JSON_PATHS.contains(&path)
        || REDACTED_DERIVED_JSON_PATHS.contains(&path)
        || path == REDACTED_EVENT_JSONL_PATH
        || path == REDACTED_HASH_MANIFEST_PATH
}

fn json_archive_entry(entries: &BTreeMap<String, Vec<u8>>, path: &str) -> anyhow::Result<Value> {
    let bytes = entries.get(path).ok_or_else(|| match path {
        "export-manifest.json" => {
            anyhow::anyhow!("Redacted live diagnostic archive is missing its manifest.")
        }
        "run.json" => anyhow::anyhow!("Redacted live diagnostic archive is missing run evidence."),
        "result.json" => {
            anyhow::anyhow!("Redacted live diagnostic archive is missing result evidence.")
        }
        "authority.json" => {
            anyhow::anyhow!("Redacted live diagnostic archive is missing authority evidence.")
        }
        _ => anyhow::anyhow!("Redacted live diagnostic archive is missing required evidence."),
    })?;
    let value: Value = serde_json::from_slice(bytes)
        .context("Redacted live diagnostic archive has invalid JSON.")?;
    if contains_unredacted_value(&value) {
        anyhow::bail!("Redacted live diagnostic archive contains sensitive content.");
    }
    Ok(value)
}

fn validated_omitted_paths(value: &Value, field: &str) -> anyhow::Result<BTreeSet<String>> {
    let paths = value
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("Redacted live diagnostic archive manifest is invalid."))?;
    if paths.len() > 4_096 {
        anyhow::bail!("Redacted live diagnostic archive omits an invalid evidence set.");
    }
    let output = paths
        .iter()
        .map(|path| {
            path.as_str()
                .filter(|path| is_excluded_evidence_path(path))
                .map(str::to_string)
                .ok_or_else(|| {
                    anyhow::anyhow!("Redacted live diagnostic archive manifest is invalid.")
                })
        })
        .collect::<anyhow::Result<BTreeSet<_>>>()?;
    if output.len() != paths.len()
        || RAW_EVIDENCE_PATHS
            .iter()
            .any(|path| !output.contains(*path))
    {
        anyhow::bail!("Redacted live diagnostic archive omits an invalid evidence set.");
    }
    Ok(output)
}

fn has_exact_object_keys(value: &Value, expected: &[&str]) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    object.len() == expected.len() && expected.iter().all(|key| object.contains_key(*key))
}

fn has_exact_string_array(value: Option<&Value>, expected: &[&str]) -> bool {
    value.and_then(Value::as_array).is_some_and(|values| {
        values.len() == expected.len()
            && values
                .iter()
                .zip(expected)
                .all(|(value, expected)| value.as_str() == Some(*expected))
    })
}

fn source_result_category(value: &Value) -> anyhow::Result<&'static str> {
    match value.get("result").and_then(Value::as_str) {
        Some("completed") | Some("finished") => Ok("completed"),
        Some("cancelled") => Ok("cancelled"),
        Some("failed") => Ok("failed"),
        Some("incomplete") | Some("detached") | Some("unknown") => Ok("incomplete"),
        _ => anyhow::bail!("Redacted live diagnostic archive result evidence is invalid."),
    }
}

fn validate_screenshot_metadata(path: &str, value: &Value) -> anyhow::Result<bool> {
    let label = path
        .strip_prefix("screenshots/")
        .and_then(|path| path.strip_suffix(".json"))
        .ok_or_else(|| anyhow::anyhow!("Redacted screenshot metadata path is invalid."))?;
    if !has_exact_object_keys(
        value,
        &[
            "schemaVersion",
            "label",
            "artifactPresent",
            "pixelsExported",
        ],
    ) || value.get("schemaVersion").and_then(Value::as_u64)
        != Some(REDACTED_EXPORT_SCHEMA_VERSION)
        || value.get("label").and_then(Value::as_str) != Some(label)
        || value.get("pixelsExported").and_then(Value::as_bool) != Some(false)
    {
        anyhow::bail!("Redacted screenshot metadata is invalid.");
    }
    value
        .get("artifactPresent")
        .and_then(Value::as_bool)
        .ok_or_else(|| anyhow::anyhow!("Redacted screenshot metadata is invalid."))
}

fn validate_redacted_events(entries: &BTreeMap<String, Vec<u8>>) -> anyhow::Result<usize> {
    let bytes = entries.get(REDACTED_EVENT_JSONL_PATH).ok_or_else(|| {
        anyhow::anyhow!("Redacted live diagnostic archive is missing required evidence.")
    })?;
    let text = std::str::from_utf8(bytes)
        .context("Redacted live diagnostic archive has invalid JSONL.")?;
    if text.is_empty() || !text.ends_with('\n') {
        anyhow::bail!("Redacted live diagnostic archive has invalid JSONL.");
    }

    let mut event_count = 0_usize;
    let mut previous_sequence = None;
    for line in text.lines() {
        if line.is_empty() {
            anyhow::bail!("Redacted live diagnostic archive has invalid JSONL.");
        }
        let event: Value = serde_json::from_str(line)
            .context("Redacted live diagnostic archive has invalid JSONL.")?;
        if contains_unredacted_value(&event)
            || !has_exact_object_keys(&event, &["sequence", "timestampMs", "kind"])
        {
            anyhow::bail!("Redacted live diagnostic archive contains sensitive content.");
        }
        let sequence = event
            .get("sequence")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                anyhow::anyhow!("Redacted live diagnostic archive has invalid JSONL.")
            })?;
        let timestamp_ms = event
            .get("timestampMs")
            .and_then(Value::as_i64)
            .filter(|timestamp_ms| *timestamp_ms >= 0)
            .ok_or_else(|| {
                anyhow::anyhow!("Redacted live diagnostic archive has invalid JSONL.")
            })?;
        let kind = event
            .get("kind")
            .and_then(Value::as_str)
            .filter(|kind| REDACTED_EVENT_KINDS.contains(kind))
            .ok_or_else(|| {
                anyhow::anyhow!("Redacted live diagnostic archive has invalid JSONL.")
            })?;
        let _ = (timestamp_ms, kind);
        if previous_sequence.is_some_and(|previous| sequence <= previous) {
            anyhow::bail!("Redacted live diagnostic archive has invalid JSONL.");
        }
        previous_sequence = Some(sequence);
        event_count = event_count.saturating_add(1);
        if event_count > MAX_EVENT_LINES {
            anyhow::bail!("Redacted live diagnostic archive has too many event records.");
        }
    }
    if event_count == 0 {
        anyhow::bail!("Redacted live diagnostic archive has invalid JSONL.");
    }
    Ok(event_count)
}

fn validate_redacted_hash_manifest(entries: &BTreeMap<String, Vec<u8>>) -> anyhow::Result<()> {
    let bytes = entries.get(REDACTED_HASH_MANIFEST_PATH).ok_or_else(|| {
        anyhow::anyhow!("Redacted live diagnostic archive is missing required evidence.")
    })?;
    let text = std::str::from_utf8(bytes)
        .context("Redacted live diagnostic archive has an invalid hash manifest.")?;
    if text.is_empty() || !text.ends_with('\n') {
        anyhow::bail!("Redacted live diagnostic archive has an invalid hash manifest.");
    }
    let mut paths = BTreeSet::new();
    for line in text.lines() {
        let (digest, path) = line.split_once("  ").ok_or_else(|| {
            anyhow::anyhow!("Redacted live diagnostic archive has an invalid hash manifest.")
        })?;
        let entry = entries
            .get(path)
            .filter(|_| path != REDACTED_HASH_MANIFEST_PATH);
        if !valid_lowercase_sha256(digest)
            || !valid_relative_path(path)
            || !paths.insert(path.to_string())
            || entry.is_none()
        {
            anyhow::bail!("Redacted live diagnostic archive has an invalid hash manifest.");
        }
        let entry = entry.expect("entry checked above");
        let actual = hex_encode(&Sha256::digest(entry));
        if digest != actual {
            anyhow::bail!("Redacted live diagnostic archive hash verification failed.");
        }
    }
    if paths.len() != entries.len().saturating_sub(1) {
        anyhow::bail!("Redacted live diagnostic archive has an invalid hash manifest.");
    }
    Ok(())
}

fn validate_expected_identity(
    run_id: &str,
    release_manifest_sha256: &str,
    expected_run_id: Option<&str>,
    expected_release_manifest_sha256: Option<&str>,
) -> anyhow::Result<()> {
    if let Some(expected_run_id) = expected_run_id {
        if canonical_uuid(expected_run_id)? != run_id {
            anyhow::bail!("Redacted live diagnostic archive has a different run identifier.");
        }
    }
    if let Some(expected_release_manifest_sha256) = expected_release_manifest_sha256 {
        if !valid_lowercase_sha256(expected_release_manifest_sha256)
            || expected_release_manifest_sha256 != release_manifest_sha256
        {
            anyhow::bail!("Redacted live diagnostic archive has a different release identity.");
        }
    }
    Ok(())
}

fn manifest_run_id(value: &Value) -> anyhow::Result<String> {
    let run_id = value
        .get("runID")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("Redacted live diagnostic archive manifest is invalid."))?;
    canonical_uuid(run_id)
}

fn manifest_release_hash(value: &Value) -> anyhow::Result<String> {
    let value = value
        .get("releaseManifestSha256")
        .and_then(Value::as_str)
        .filter(|value| valid_lowercase_sha256(value))
        .ok_or_else(|| anyhow::anyhow!("Redacted live diagnostic archive manifest is invalid."))?;
    Ok(value.to_string())
}

fn validate_exported_run_metadata(
    value: &Value,
    run_id: &str,
    release_manifest_sha256: &str,
) -> anyhow::Result<()> {
    if value.get("protocol").and_then(Value::as_str) != Some(LIVE_PROTOCOL)
        || value.get("runID").and_then(Value::as_str) != Some(run_id)
        || value.get("releaseManifestSha256").and_then(Value::as_str)
            != Some(release_manifest_sha256)
        || value
            .get("hiddenReasoningCaptured")
            .and_then(Value::as_bool)
            != Some(false)
    {
        anyhow::bail!("Redacted live diagnostic archive run evidence is invalid.");
    }
    Ok(())
}

fn validate_exported_result_metadata(
    value: &Value,
    run_id: &str,
    release_manifest_sha256: &str,
) -> anyhow::Result<()> {
    validate_exported_run_metadata(value, run_id, release_manifest_sha256)?;
    if value.get("result").and_then(Value::as_str).is_none() {
        anyhow::bail!("Redacted live diagnostic archive result evidence is invalid.");
    }
    Ok(())
}

fn contains_unredacted_value(value: &Value) -> bool {
    match value {
        Value::Array(values) => values.iter().any(contains_unredacted_value),
        Value::Object(values) => values.iter().any(|(key, value)| {
            (should_redact_value(key, value)
                && !matches!(value, Value::String(value) if value == REDACTION_MARKER))
                || contains_unredacted_value(value)
        }),
        Value::String(value) => redact_log_line(value).as_ref() != value,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::super::live_export::archive_bytes;
    use super::super::live_export::archive_entry_bytes;
    use super::super::live_export::json_entry_bytes;
    use super::super::live_export::jsonl_entry_bytes;
    use super::super::live_export::redacted_archive_hash_manifest;
    use super::super::live_export::redacted_schema_entries;
    use super::*;

    const RUN_ID: &str = "11111111-1111-4111-8111-111111111111";
    const RELEASE_HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn redacted_entries() -> BTreeMap<String, Value> {
        BTreeMap::from([
            (
                "run.json".to_string(),
                serde_json::json!({
                    "protocol": LIVE_PROTOCOL,
                    "runID": RUN_ID,
                    "releaseManifestSha256": RELEASE_HASH,
                    "hiddenReasoningCaptured": false,
                    "request": { "prompt": REDACTION_MARKER },
                }),
            ),
            (
                "result.json".to_string(),
                serde_json::json!({
                    "protocol": LIVE_PROTOCOL,
                    "runID": RUN_ID,
                    "releaseManifestSha256": RELEASE_HASH,
                    "hiddenReasoningCaptured": false,
                    "result": "finished",
                    "visibleState": REDACTION_MARKER,
                }),
            ),
            (
                "authority.json".to_string(),
                serde_json::json!({ "commercialBoundary": "normal-product-gates" }),
            ),
            (
                "export-manifest.json".to_string(),
                serde_json::json!({
                    "protocol": LIVE_PROTOCOL,
                    "schemaVersion": 1,
                    "runID": RUN_ID,
                    "releaseManifestSha256": RELEASE_HASH,
                    "exportKind": "redacted-live-evidence",
                    "exactArchive": false,
                    "redactionCount": 2,
                    "omittedPaths": RAW_EVIDENCE_PATHS,
                }),
            ),
        ])
    }

    fn v2_redacted_entries() -> BTreeMap<String, Vec<u8>> {
        let source_omitted_paths = RAW_EVIDENCE_PATHS
            .iter()
            .copied()
            .chain(["screenshots/started.png"])
            .collect::<Vec<_>>();
        let mut entries = BTreeMap::from([
            (
                "run.json".to_string(),
                json_entry_bytes(&serde_json::json!({
                    "protocol": LIVE_PROTOCOL,
                    "runID": RUN_ID,
                    "releaseManifestSha256": RELEASE_HASH,
                    "hiddenReasoningCaptured": false,
                    "request": { "prompt": REDACTION_MARKER },
                }))
                .expect("run entry"),
            ),
            (
                "result.json".to_string(),
                json_entry_bytes(&serde_json::json!({
                    "protocol": LIVE_PROTOCOL,
                    "runID": RUN_ID,
                    "releaseManifestSha256": RELEASE_HASH,
                    "hiddenReasoningCaptured": false,
                    "result": "completed",
                    "visibleState": REDACTION_MARKER,
                }))
                .expect("result entry"),
            ),
            (
                "authority.json".to_string(),
                json_entry_bytes(&serde_json::json!({
                    "commercialBoundary": "normal-product-gates",
                }))
                .expect("authority entry"),
            ),
            (
                "summary.json".to_string(),
                json_entry_bytes(&serde_json::json!({
                    "protocol": LIVE_PROTOCOL,
                    "schemaVersion": REDACTED_EXPORT_SCHEMA_VERSION,
                    "runID": RUN_ID,
                    "releaseManifestSha256": RELEASE_HASH,
                    "result": "completed",
                    "eventCount": 2,
                    "screenshotArtifactCount": 1,
                    "redacted": true,
                    "exactArchive": false,
                }))
                .expect("summary entry"),
            ),
            (
                "screenshots/started.json".to_string(),
                json_entry_bytes(&serde_json::json!({
                    "schemaVersion": REDACTED_EXPORT_SCHEMA_VERSION,
                    "label": "started",
                    "artifactPresent": true,
                    "pixelsExported": false,
                }))
                .expect("started screenshot metadata"),
            ),
            (
                "screenshots/finished.json".to_string(),
                json_entry_bytes(&serde_json::json!({
                    "schemaVersion": REDACTED_EXPORT_SCHEMA_VERSION,
                    "label": "finished",
                    "artifactPresent": false,
                    "pixelsExported": false,
                }))
                .expect("finished screenshot metadata"),
            ),
            (
                REDACTED_EVENT_JSONL_PATH.to_string(),
                jsonl_entry_bytes(&[
                    serde_json::json!({
                        "sequence": 1,
                        "timestampMs": 1_000,
                        "kind": "run-started",
                    }),
                    serde_json::json!({
                        "sequence": 2,
                        "timestampMs": 1_500,
                        "kind": "run-finished",
                    }),
                ])
                .expect("events entry"),
            ),
        ]);
        for (path, schema) in redacted_schema_entries() {
            entries.insert(
                path.to_string(),
                json_entry_bytes(&schema).expect("schema entry"),
            );
        }
        entries.insert(
            "export-manifest.json".to_string(),
            json_entry_bytes(&serde_json::json!({
                "protocol": LIVE_PROTOCOL,
                "schemaVersion": REDACTED_EXPORT_SCHEMA_VERSION,
                "runID": RUN_ID,
                "releaseManifestSha256": RELEASE_HASH,
                "exportKind": "redacted-live-evidence",
                "exactArchive": false,
                "redactionCount": 2,
                "sourceOmittedPaths": source_omitted_paths,
                "summaryPath": "summary.json",
                "eventsPath": REDACTED_EVENT_JSONL_PATH,
                "hashManifestPath": REDACTED_HASH_MANIFEST_PATH,
                "schemaPaths": REDACTED_SCHEMA_PATHS,
                "screenshotMetadataPaths": REDACTED_SCREENSHOT_METADATA_PATHS,
            }))
            .expect("export manifest"),
        );
        entries.insert(
            REDACTED_HASH_MANIFEST_PATH.to_string(),
            redacted_archive_hash_manifest(&entries),
        );
        entries
    }

    #[test]
    fn validates_the_bounded_redacted_replay_contract() {
        let bytes = archive_bytes(&redacted_entries()).expect("archive");
        let export = validate_export_bytes(&bytes, Some(RUN_ID), Some(RELEASE_HASH))
            .expect("validated export");

        assert_eq!(export.run_id, RUN_ID);
        assert_eq!(export.release_manifest_sha256, RELEASE_HASH);
        assert_eq!(export.redaction_count, 2);
        assert_eq!(export.omitted_path_count, RAW_EVIDENCE_PATHS.len());
        assert_eq!(export.outcome, serde_json::json!("finished"));
    }

    #[test]
    fn rejects_raw_event_transcripts_and_unredacted_text() {
        let mut entries = redacted_entries();
        entries.insert(
            "events.jsonl".to_string(),
            serde_json::json!({ "event": "private prompt" }),
        );
        let bytes = archive_bytes(&entries).expect("archive");
        assert!(validate_export_bytes(&bytes, None, None).is_err());

        let mut entries = redacted_entries();
        entries.insert(
            "run.json".to_string(),
            serde_json::json!({
                "protocol": LIVE_PROTOCOL,
                "runID": RUN_ID,
                "releaseManifestSha256": RELEASE_HASH,
                "hiddenReasoningCaptured": false,
                "request": { "prompt": "private prompt" },
            }),
        );
        let bytes = archive_bytes(&entries).expect("archive");
        assert!(validate_export_bytes(&bytes, None, None).is_err());
    }

    #[test]
    fn validates_v2_derived_jsonl_schema_screenshot_metadata_and_hashes() {
        let entries = v2_redacted_entries();
        assert!(!entries.contains_key("screenshots/started.png"));
        let events = std::str::from_utf8(
            entries
                .get(REDACTED_EVENT_JSONL_PATH)
                .expect("events entry"),
        )
        .expect("events UTF-8");
        assert!(!events.contains("\"name\""));
        assert!(!events.contains("private prompt"));

        let bytes = archive_entry_bytes(&entries).expect("archive");
        let export = validate_export_bytes(&bytes, Some(RUN_ID), Some(RELEASE_HASH))
            .expect("validated v2 export");

        assert_eq!(export.schema_version, REDACTED_EXPORT_SCHEMA_VERSION);
        assert_eq!(export.outcome, serde_json::json!("completed"));
        assert_eq!(export.omitted_path_count, RAW_EVIDENCE_PATHS.len() + 1);
    }

    #[test]
    fn rejects_v2_raw_event_fields_and_hash_tampering() {
        let mut entries = v2_redacted_entries();
        entries.remove(REDACTED_HASH_MANIFEST_PATH);
        entries.insert(
            REDACTED_EVENT_JSONL_PATH.to_string(),
            jsonl_entry_bytes(&[serde_json::json!({
                "sequence": 1,
                "timestampMs": 1_000,
                "name": "private prompt",
            })])
            .expect("tampered events entry"),
        );
        entries.insert(
            REDACTED_HASH_MANIFEST_PATH.to_string(),
            redacted_archive_hash_manifest(&entries),
        );
        let bytes = archive_entry_bytes(&entries).expect("archive");
        assert!(validate_export_bytes(&bytes, None, None).is_err());

        let mut entries = v2_redacted_entries();
        entries
            .get_mut("summary.json")
            .expect("summary entry")
            .push(b' ');
        let bytes = archive_entry_bytes(&entries).expect("archive");
        assert!(validate_export_bytes(&bytes, None, None).is_err());
    }
}

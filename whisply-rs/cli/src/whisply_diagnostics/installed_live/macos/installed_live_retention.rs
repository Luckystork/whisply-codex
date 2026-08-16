//! Owner-only retained storage for replay-validated diagnostic exports.
//!
//! Retention is deliberately separate from public export construction: the
//! caller receives its requested archive, while this module makes one bounded,
//! replay-validated copy under the current user's diagnostic root. The store
//! never accepts raw evidence, follows no links, expires entries after 30 days,
//! and keeps only the newest sixteen records.

use std::collections::BTreeSet;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use chrono::DateTime;
use chrono::NaiveDateTime;
use chrono::Utc;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;

use super::LIVE_PROTOCOL;
use super::canonical_uuid;
use super::ensure_private_directory;
use super::read_private_bytes;
use super::read_private_json;
use super::sessions_root;
use super::sha256_file;
use super::unix_ms;
use super::valid_lowercase_sha256;
use super::write_new_json;
use super::write_new_private;

const MAX_RETAINED_EXPORTS: usize = 16;
const MAX_RETENTION_AGE_MS: i64 = 30 * 24 * 60 * 60 * 1_000;

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct RetainedExportManifest {
    protocol: String,
    export_kind: String,
    run_id: String,
    session_id: String,
    release_manifest_sha256: String,
    retained_at_ms: i64,
    export_sha256: String,
    source_export: String,
}

#[derive(Debug)]
struct RetainedExportRecord {
    basename: String,
    retained_at_ms: i64,
}

pub(super) fn retain_export(
    export: &Path,
    run_id: &str,
    session_id: &str,
    release_manifest_sha256: &str,
) -> anyhow::Result<Value> {
    let sessions = sessions_root()?;
    let diagnostic_root = sessions
        .parent()
        .ok_or_else(|| anyhow::anyhow!("The owner-only live diagnostic root is invalid."))?;
    let root = diagnostic_root.join("RetainedExports");
    ensure_private_directory(&root)?;
    retain_export_at(
        export,
        run_id,
        session_id,
        release_manifest_sha256,
        &root,
        unix_ms()?,
    )
}

fn retain_export_at(
    export: &Path,
    run_id: &str,
    session_id: &str,
    release_manifest_sha256: &str,
    root: &Path,
    now_ms: i64,
) -> anyhow::Result<Value> {
    let run_id = canonical_uuid(run_id)?;
    let session_id = canonical_uuid(session_id)?;
    if !valid_lowercase_sha256(release_manifest_sha256) {
        anyhow::bail!("Retained live export release identity is invalid.");
    }
    let source_validation = super::live_export_validation::assert_export(
        export,
        Some(&run_id),
        Some(release_manifest_sha256),
    )?;
    let source_bytes = read_private_bytes(export, super::live_export::MAX_EXPORT_BYTES)?;

    ensure_private_directory(root)?;
    purge_retained_exports(root, now_ms)?;

    let basename = format!("{}-{run_id}", retained_export_timestamp(now_ms)?);
    let retained_export = root.join(format!("{basename}.zip"));
    let retained_manifest = root.join(format!("{basename}.json"));
    for path in [&retained_export, &retained_manifest] {
        match fs::symlink_metadata(path) {
            Ok(_) => anyhow::bail!("Retained live export destination already exists."),
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }

    write_new_private(&retained_export, &source_bytes)?;
    let retained_result = (|| {
        let export_sha256 = sha256_file(&retained_export)?;
        let replay_validation = super::live_export_validation::replay_export(
            &retained_export,
            Some(&run_id),
            Some(release_manifest_sha256),
        )?;
        let manifest = RetainedExportManifest {
            protocol: LIVE_PROTOCOL.to_string(),
            export_kind: "redacted-live-evidence".to_string(),
            run_id: run_id.clone(),
            session_id: session_id.clone(),
            release_manifest_sha256: release_manifest_sha256.to_string(),
            retained_at_ms: now_ms,
            export_sha256: export_sha256.clone(),
            source_export: export.display().to_string(),
        };
        write_new_json(&retained_manifest, &manifest)?;
        // Enforce the record cap after the complete pair exists. Doing this
        // only before insertion would let the seventeenth archive survive
        // until a later export happened to trigger another cleanup pass.
        purge_retained_exports(root, now_ms)?;
        if fs::symlink_metadata(&retained_export).is_err() {
            anyhow::bail!(
                "Retained live export is older than the newest sixteen records and was removed."
            );
        }
        Ok(serde_json::json!({
            "ok": true,
            "retainedExport": retained_export.display().to_string(),
            "retainedManifest": retained_manifest.display().to_string(),
            "runID": run_id,
            "sessionID": session_id,
            "releaseManifestSha256": release_manifest_sha256,
            "retainedAtMs": now_ms,
            "exportSha256": export_sha256,
            "sourceValidation": source_validation,
            "replayValidation": replay_validation,
        }))
    })();
    if retained_result.is_err() {
        let _ = remove_private_file_if_present(&retained_manifest);
        let _ = remove_private_file_if_present(&retained_export);
    }
    retained_result
}

fn purge_retained_exports(root: &Path, now_ms: i64) -> anyhow::Result<()> {
    ensure_private_directory(root)?;
    let mut basenames = BTreeSet::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if let Some(basename) = name.strip_suffix(".json") {
            basenames.insert(basename.to_string());
        } else if let Some(basename) = name.strip_suffix(".zip") {
            basenames.insert(basename.to_string());
        }
    }

    let mut records = Vec::new();
    for basename in basenames {
        match read_retained_record(root, &basename) {
            Ok(record) if !retention_expired(record.retained_at_ms, now_ms) => records.push(record),
            Ok(_) | Err(_) => remove_retained_pair(root, &basename)?,
        }
    }
    records.sort_by(|left, right| {
        (left.retained_at_ms, &left.basename).cmp(&(right.retained_at_ms, &right.basename))
    });
    while records.len() > MAX_RETAINED_EXPORTS {
        let record = records.remove(0);
        remove_retained_pair(root, &record.basename)?;
    }
    Ok(())
}

fn read_retained_record(root: &Path, basename: &str) -> anyhow::Result<RetainedExportRecord> {
    let (timestamp_ms, filename_run_id) = retained_export_timestamp_ms(basename)
        .ok_or_else(|| anyhow::anyhow!("Retained live export name is invalid."))?;
    let manifest_path = root.join(format!("{basename}.json"));
    let export_path = root.join(format!("{basename}.zip"));
    let manifest: RetainedExportManifest = read_private_json(&manifest_path)?;
    let run_id = canonical_uuid(&manifest.run_id)?;
    if manifest.protocol != LIVE_PROTOCOL
        || manifest.export_kind != "redacted-live-evidence"
        || run_id != filename_run_id
        || canonical_uuid(&manifest.session_id).is_err()
        || !valid_lowercase_sha256(&manifest.release_manifest_sha256)
        || !valid_lowercase_sha256(&manifest.export_sha256)
        || manifest.source_export.is_empty()
        || manifest.retained_at_ms.div_euclid(1_000) != timestamp_ms.div_euclid(1_000)
        || sha256_file(&export_path)? != manifest.export_sha256
    {
        anyhow::bail!("Retained live export metadata is invalid.");
    }
    Ok(RetainedExportRecord {
        basename: basename.to_string(),
        retained_at_ms: manifest.retained_at_ms,
    })
}

fn retained_export_timestamp(now_ms: i64) -> anyhow::Result<String> {
    let timestamp = DateTime::<Utc>::from_timestamp_millis(now_ms)
        .ok_or_else(|| anyhow::anyhow!("Retained live export time is invalid."))?;
    Ok(timestamp.format("%Y%m%dT%H%M%SZ").to_string())
}

fn retained_export_timestamp_ms(basename: &str) -> Option<(i64, String)> {
    let (stamp, run_id) = basename.split_once('-')?;
    let timestamp = NaiveDateTime::parse_from_str(stamp, "%Y%m%dT%H%M%SZ")
        .ok()?
        .and_utc()
        .timestamp_millis();
    Some((timestamp, canonical_uuid(run_id).ok()?))
}

fn retention_expired(retained_at_ms: i64, now_ms: i64) -> bool {
    now_ms >= retained_at_ms && now_ms.saturating_sub(retained_at_ms) > MAX_RETENTION_AGE_MS
}

fn remove_retained_pair(root: &Path, basename: &str) -> anyhow::Result<()> {
    remove_private_file_if_present(&root.join(format!("{basename}.json")))?;
    remove_private_file_if_present(&root.join(format!("{basename}.zip")))
}

fn remove_private_file_if_present(path: &Path) -> anyhow::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() || metadata.file_type().is_symlink() => {
            fs::remove_file(path)?;
            Ok(())
        }
        Ok(_) => anyhow::bail!("Retained live export storage contains an invalid entry."),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
#[path = "installed_live_retention_tests.rs"]
mod tests;

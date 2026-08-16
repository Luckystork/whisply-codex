use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::path::PathBuf;

use pretty_assertions::assert_eq;
use tempfile::TempDir;

use super::super::live_export::RAW_EVIDENCE_PATHS;
use super::super::live_export::archive_bytes;
use super::*;

const RELEASE_HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const SESSION_ID: &str = "22222222-2222-4222-8222-222222222222";
const NOW_MS: i64 = 1_800_000_000_000;

fn run_id(index: usize) -> String {
    format!("11111111-1111-4111-8111-{index:012}")
}

fn create_source_export(root: &Path, run_id: &str) -> PathBuf {
    let entries = BTreeMap::from([
        (
            "run.json".to_string(),
            serde_json::json!({
                "protocol": LIVE_PROTOCOL,
                "runID": run_id,
                "releaseManifestSha256": RELEASE_HASH,
                "hiddenReasoningCaptured": false,
                "request": { "prompt": "<redacted>" },
            }),
        ),
        (
            "result.json".to_string(),
            serde_json::json!({
                "protocol": LIVE_PROTOCOL,
                "runID": run_id,
                "releaseManifestSha256": RELEASE_HASH,
                "hiddenReasoningCaptured": false,
                "result": "finished",
                "visibleState": "<redacted>",
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
                "runID": run_id,
                "releaseManifestSha256": RELEASE_HASH,
                "exportKind": "redacted-live-evidence",
                "exactArchive": false,
                "redactionCount": 2,
                "omittedPaths": RAW_EVIDENCE_PATHS,
            }),
        ),
    ]);
    let path = root.join(format!("{run_id}.zip"));
    super::super::write_new_private(&path, &archive_bytes(&entries).expect("archive"))
        .expect("owner-only source export");
    path
}

fn private_retained_root(temp: &TempDir) -> PathBuf {
    let root = temp.path().join("RetainedExports");
    super::super::create_new_private_directory(&root).expect("owner-only retention root");
    root
}

fn private_source_root(temp: &TempDir) -> PathBuf {
    let root = temp.path().join("SourceExports");
    super::super::create_new_private_directory(&root).expect("owner-only source root");
    root
}

#[test]
fn retains_an_owner_only_archive_that_replays_without_source_authority() {
    let temp = TempDir::new().expect("temporary root");
    let root = private_retained_root(&temp);
    let source_root = private_source_root(&temp);
    let run_id = run_id(1);
    let source = create_source_export(&source_root, &run_id);

    let retained = retain_export_at(&source, &run_id, SESSION_ID, RELEASE_HASH, &root, NOW_MS)
        .expect("retained export");
    let retained_path = PathBuf::from(
        retained
            .get("retainedExport")
            .and_then(serde_json::Value::as_str)
            .expect("retained archive path"),
    );

    assert_eq!(
        fs::symlink_metadata(&retained_path)
            .expect("metadata")
            .mode()
            & 0o077,
        0
    );
    super::super::live_export_validation::assert_export(
        &retained_path,
        Some(&run_id),
        Some(RELEASE_HASH),
    )
    .expect("replayable retained archive");
    assert!(source.exists());
}

#[test]
fn expires_old_pairs_and_keeps_only_the_sixteen_newest_records() {
    let temp = TempDir::new().expect("temporary root");
    let root = private_retained_root(&temp);
    let source_root = private_source_root(&temp);
    let expired_run_id = run_id(0);
    let expired_source = create_source_export(&source_root, &expired_run_id);
    let expired = retain_export_at(
        &expired_source,
        &expired_run_id,
        SESSION_ID,
        RELEASE_HASH,
        &root,
        NOW_MS - MAX_RETENTION_AGE_MS - 1,
    )
    .expect("expired retained export");
    let expired_path = PathBuf::from(
        expired
            .get("retainedExport")
            .and_then(serde_json::Value::as_str)
            .expect("expired archive path"),
    );

    let mut oldest_retained = None;
    for index in 1..=MAX_RETAINED_EXPORTS + 1 {
        let run_id = run_id(index);
        let source = create_source_export(&source_root, &run_id);
        let retained = retain_export_at(
            &source,
            &run_id,
            SESSION_ID,
            RELEASE_HASH,
            &root,
            NOW_MS + i64::try_from(index).expect("index"),
        )
        .expect("retained export");
        if index == 1 {
            oldest_retained = retained
                .get("retainedExport")
                .and_then(serde_json::Value::as_str)
                .map(PathBuf::from);
        }
    }

    let retained_zip_count = fs::read_dir(&root)
        .expect("retention entries")
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "zip")
        })
        .count();
    let retained_manifest_count = fs::read_dir(&root)
        .expect("retention entries")
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "json")
        })
        .count();

    assert!(!expired_path.exists());
    assert!(!oldest_retained.expect("oldest retained path").exists());
    assert_eq!(retained_zip_count, MAX_RETAINED_EXPORTS);
    assert_eq!(retained_manifest_count, MAX_RETAINED_EXPORTS);
}

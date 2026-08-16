use super::*;
use pretty_assertions::assert_eq;
use std::io::Write;
use tempfile::tempdir;

// MARK: - The writer

#[test]
fn the_writer_redacts_what_reaches_the_file() {
    let home = tempdir().expect("temp dir");
    let path = home.path().join("whisply-tui.log");
    {
        let mut writer = RedactedLogWriter::open(&path, 1024 * 1024).expect("open log");
        writer
            .write_all(b"authorization: Bearer abc123def456\n")
            .expect("write");
        writer.flush().expect("flush");
    }

    let contents = std::fs::read_to_string(&path).expect("read log");
    assert!(!contents.contains("abc123def456"), "{contents}");
    assert!(contents.contains("Bearer <redacted>"), "{contents}");
}

/// tracing hands the sink whatever chunks its writer produced, which need not
/// align to lines. Scrubbing per chunk would let a credential split across two
/// writes through in halves that each look harmless.
#[test]
fn a_secret_split_across_two_writes_is_still_caught() {
    let home = tempdir().expect("temp dir");
    let path = home.path().join("whisply-tui.log");
    {
        let mut writer = RedactedLogWriter::open(&path, 1024 * 1024).expect("open log");
        writer
            .write_all(b"authorization: Bearer abc12")
            .expect("write");
        writer.write_all(b"3def456\n").expect("write");
        writer.flush().expect("flush");
    }

    let contents = std::fs::read_to_string(&path).expect("read log");
    assert!(!contents.contains("abc123def456"), "{contents}");
}

/// Enforcing the budget only when the file is opened bounds a log that grows
/// across launches, but leaves a single long session — the app's runtime is
/// one — appending without limit until it next starts.
#[test]
fn a_single_long_session_is_bounded_while_it_runs() {
    let home = tempdir().expect("temp dir");
    let path = home.path().join("whisply-tui.log");
    {
        let mut writer = RedactedLogWriter::open(&path, 256).expect("open log");
        for index in 0..200 {
            writer
                .write_all(format!("line {index} of an ordinary session\n").as_bytes())
                .expect("write");
        }
        writer.flush().expect("flush");
    }

    let current = std::fs::metadata(&path).expect("current log").len();
    assert!(current <= 256, "current log grew to {current} bytes");
}

#[test]
fn at_most_one_older_log_survives_the_rotation() {
    let home = tempdir().expect("temp dir");
    let path = home.path().join("whisply-tui.log");
    {
        let mut writer = RedactedLogWriter::open(&path, 128).expect("open log");
        for index in 0..500 {
            writer
                .write_all(format!("line {index} of an ordinary session\n").as_bytes())
                .expect("write");
        }
        writer.flush().expect("flush");
    }

    let logs: Vec<_> = std::fs::read_dir(home.path())
        .expect("read home")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(logs.len(), 2, "kept {logs:?}");
}

#[test]
fn an_unterminated_stream_cannot_grow_the_buffer_without_bound() {
    let home = tempdir().expect("temp dir");
    let path = home.path().join("whisply-tui.log");
    let mut writer = RedactedLogWriter::open(&path, 10 * 1024 * 1024).expect("open log");
    for _ in 0..200 {
        writer.write_all(&[b'x'; 1024]).expect("write");
    }

    // Nothing has been newline-terminated, so anything on disk can only have
    // arrived because the buffer refused to keep growing.
    let written = std::fs::metadata(&path).expect("log").len();
    assert!(written > 0, "the buffer grew without bound");
}

#[cfg(unix)]
#[test]
fn the_log_is_readable_only_by_its_owner() {
    use std::os::unix::fs::PermissionsExt;

    let home = tempdir().expect("temp dir");
    let path = home.path().join("whisply-tui.log");
    let writer = RedactedLogWriter::open(&path, 1024).expect("open log");
    drop(writer);

    let mode = std::fs::metadata(&path).expect("log").permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "log mode was {mode:o}");
}

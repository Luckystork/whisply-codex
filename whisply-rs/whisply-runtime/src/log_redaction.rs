//! A local log file that redacts what it is given and stays within a budget.
//!
//! The scrubbing itself lives in `whisply_utils_string`, because the file log is
//! not the only place the product records what it did: the app-server and the
//! TUI also keep a SQLite log store under the account home. One scrubber
//! serving both is what keeps the two answers from drifting apart.

use std::fs::File;
use std::fs::OpenOptions;
use std::io;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use whisply_utils_string::redact_log_line;

pub use whisply_utils_string::REDACTED;

/// A local log file that redacts what it is given and stays within a budget.
///
/// The budget is enforced while the session runs, not only when the file is
/// opened. Enforcing it at open time alone bounds a log that grows across
/// launches but leaves a single long-lived session — the app's runtime is one —
/// appending without limit until it next starts.
pub struct RedactedLogWriter {
    path: PathBuf,
    max_bytes: u64,
    written: u64,
    file: File,
    pending: Vec<u8>,
}

/// How much unterminated output to hold before writing it out anyway, so a
/// stream that never emits a newline cannot grow the buffer without bound.
const MAX_PENDING_BYTES: usize = 64 * 1024;

impl RedactedLogWriter {
    /// Opens the log for appending, applying the budget to what is already there.
    pub fn open(path: &Path, max_bytes: u64) -> io::Result<Self> {
        let file = open_log_file(path)?;
        let written = file.metadata().map(|metadata| metadata.len()).unwrap_or(0);
        let mut writer = Self {
            path: path.to_path_buf(),
            max_bytes,
            written,
            file,
            pending: Vec::new(),
        };
        if writer.written > writer.max_bytes {
            writer.rotate()?;
        }
        Ok(writer)
    }

    /// Retires the current log and starts a new one, keeping at most one older
    /// file so the pair is bounded rather than the sequence being unbounded.
    fn rotate(&mut self) -> io::Result<()> {
        self.file.flush()?;
        let previous = PathBuf::from(format!("{}.1", self.path.display()));
        let _ = std::fs::remove_file(&previous);
        std::fs::rename(&self.path, &previous)?;
        self.file = open_log_file(&self.path)?;
        self.written = 0;
        Ok(())
    }

    fn write_redacted_line(&mut self, line: &[u8]) -> io::Result<()> {
        // Invalid UTF-8 cannot be scanned for the shapes we redact, so it is
        // dropped rather than passed through unexamined.
        let Ok(text) = std::str::from_utf8(line) else {
            return Ok(());
        };
        let redacted = redact_log_line(text);
        let bytes = redacted.as_bytes();
        if self.written.saturating_add(bytes.len() as u64) > self.max_bytes {
            self.rotate()?;
        }
        self.file.write_all(bytes)?;
        self.written = self.written.saturating_add(bytes.len() as u64);
        Ok(())
    }

    fn drain_complete_lines(&mut self) -> io::Result<()> {
        while let Some(position) = self.pending.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=position).collect();
            self.write_redacted_line(&line)?;
        }
        if self.pending.len() > MAX_PENDING_BYTES {
            let line = std::mem::take(&mut self.pending);
            self.write_redacted_line(&line)?;
        }
        Ok(())
    }
}

fn open_log_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

impl Write for RedactedLogWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.pending.extend_from_slice(buf);
        self.drain_complete_lines()?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if !self.pending.is_empty() {
            let line = std::mem::take(&mut self.pending);
            self.write_redacted_line(&line)?;
        }
        self.file.flush()
    }
}

impl Drop for RedactedLogWriter {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

#[cfg(test)]
#[path = "log_redaction_tests.rs"]
mod tests;

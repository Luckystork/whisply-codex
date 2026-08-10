use std::ffi::OsString;
use std::path::Path;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::sync::OnceLock;
use std::sync::PoisonError;

use tempfile::tempdir;

/// Serializes tests that mutate process-wide WHISPLY_HOME.
///
/// Keep OAuth tests on this one guard instead of defining per-module helpers; otherwise
/// concurrently running test modules can point File/Secrets storage at different homes.
pub(super) struct TempCodexHome {
    _guard: MutexGuard<'static, ()>,
    _dir: tempfile::TempDir,
    previous_whisply_home: Option<OsString>,
}

impl TempCodexHome {
    pub(super) fn new() -> Self {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let guard = LOCK
            .get_or_init(Mutex::default)
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let dir = tempdir().expect("create WHISPLY_HOME temp dir");
        let previous_whisply_home = std::env::var_os("WHISPLY_HOME");
        unsafe {
            std::env::set_var("WHISPLY_HOME", dir.path());
        }
        Self {
            _guard: guard,
            _dir: dir,
            previous_whisply_home,
        }
    }

    pub(super) fn path(&self) -> &Path {
        self._dir.path()
    }
}

impl Drop for TempCodexHome {
    fn drop(&mut self) {
        unsafe {
            if let Some(previous_whisply_home) = &self.previous_whisply_home {
                std::env::set_var("WHISPLY_HOME", previous_whisply_home);
            } else {
                std::env::remove_var("WHISPLY_HOME");
            }
        }
    }
}

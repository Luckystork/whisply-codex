// Real model-response tests use the native managed gateway fixture, which is
// intentionally macOS-only. The server's protocol-only coverage stays in its
// cross-platform unit tests.
#[cfg(target_os = "macos")]
mod codex_tool;

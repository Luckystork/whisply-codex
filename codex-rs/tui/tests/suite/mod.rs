// Aggregates all former standalone integration tests as modules.
#[cfg(target_os = "macos")]
mod focus_palette;
mod resize_reflow;
mod status_indicator;
mod vt100_history;
mod vt100_live_commit;

//! Companion → GUI IPC bridge (D5).
//!
//! This module is GUI-only — the runtime continues to write inbox
//! markdown files via its built-in `StdoutNotifier`. The bridge
//! parses those files, watches the directory for new ones, and
//! delivers typed events to the React UI via Tauri 2 channels.

pub mod commands;
pub mod event;
pub mod scanner;
pub mod state;
pub mod watcher;

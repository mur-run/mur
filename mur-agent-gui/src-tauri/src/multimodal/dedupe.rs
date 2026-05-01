//! Drop-event deduper — Tauri issue #14134 workaround.
//!
//! `WebviewWindow::on_drag_drop_event` fires twice on macOS for a
//! single physical drop (sometimes Linux too). Dedupe by sorted paths
//! within a short time window so the pipeline runs once.

use std::path::PathBuf;
use std::time::{Duration, Instant};

const DEFAULT_WINDOW: Duration = Duration::from_millis(120);

pub struct DropDeduper {
    last: Option<(Vec<PathBuf>, Instant)>,
    window: Duration,
}

impl DropDeduper {
    pub fn new() -> Self {
        Self::with_window(DEFAULT_WINDOW)
    }

    pub fn with_window(window: Duration) -> Self {
        Self { last: None, window }
    }

    /// Returns `true` if this drop is novel and should be processed;
    /// `false` if it duplicates the previous one within the window.
    pub fn observe(&mut self, paths: &[PathBuf], at: Instant) -> bool {
        let mut sorted: Vec<PathBuf> = paths.to_vec();
        sorted.sort();
        if let Some((prev, t)) = &self.last
            && prev == &sorted
            && at.duration_since(*t) <= self.window
        {
            return false;
        }
        self.last = Some((sorted, at));
        true
    }
}

impl Default for DropDeduper {
    fn default() -> Self {
        Self::new()
    }
}

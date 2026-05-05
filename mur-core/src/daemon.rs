//! Thin inbox helpers shared between mur-core hooks and mur-daemon.
//! The canonical implementation lives in mur-daemon/src/inbox.rs;
//! this re-exports the same logic so hook.rs has no circular dependency.

use std::path::{Path, PathBuf};

pub fn inbox_path(session_id: &str) -> PathBuf {
    dirs::home_dir()
        .expect("no home dir")
        .join(".mur")
        .join("inbox")
        .join(format!("{session_id}.md"))
}

/// Read inbox content; returns None if missing or older than `max_age_secs`.
pub fn read_inbox(path: &Path, max_age_secs: u64) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?;
    let age = std::time::SystemTime::now().duration_since(modified).ok()?;
    if age.as_secs() >= max_age_secs {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

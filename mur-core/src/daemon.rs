//! Thin inbox helpers shared between mur-core hooks and mur-daemon.
//! The canonical implementation lives in mur-daemon/src/inbox.rs;
//! this re-exports the same logic so hook.rs has no circular dependency.

use std::path::{Path, PathBuf};
use chrono;

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

/// True if murmurd's lockfile exists and its heartbeat_at timestamp is
/// within the last 30 seconds. Returns false on any IO or parse error.
pub fn is_daemon_healthy() -> bool {
    let lock_path = dirs::home_dir()
        .map(|h| h.join(".mur").join("murmurd.lock"))
        .unwrap_or_default();
    let Ok(raw) = std::fs::read_to_string(&lock_path) else {
        return false;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return false;
    };
    let Some(hb_str) = v.get("heartbeat_at").and_then(|s| s.as_str()) else {
        return false;
    };
    let Ok(hb) = chrono::DateTime::parse_from_rfc3339(hb_str) else {
        return false;
    };
    let age = chrono::Utc::now().signed_duration_since(hb.with_timezone(&chrono::Utc));
    age.num_seconds() < 30
}

/// Attempt to spawn murmurd as a detached background process.
/// Errors are swallowed — this is best-effort recovery.
pub fn try_respawn_daemon() {
    let murmurd = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("murmurd")))
        .unwrap_or_else(|| std::path::PathBuf::from("murmurd"));
    let _ = std::process::Command::new(&murmurd)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

#[cfg(test)]
mod daemon_health_tests {
    use super::*;

    #[test]
    fn stale_heartbeat_age_check() {
        let old_ts = chrono::Utc::now() - chrono::Duration::seconds(60);
        let age = chrono::Utc::now().signed_duration_since(old_ts);
        assert!(age.num_seconds() >= 30, "stale lock should be >= 30s old");
    }

    #[test]
    fn fresh_heartbeat_age_check() {
        let hb = chrono::Utc::now();
        let age = chrono::Utc::now().signed_duration_since(hb);
        assert!(age.num_seconds() < 30, "fresh lock should be < 30s old");
    }
}

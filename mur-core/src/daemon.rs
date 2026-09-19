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

/// True if murmurd is running: its lock names a pid that is alive, OR the
/// heartbeat is fresh (< 30 s).
///
/// The pid check comes first and is the one that matters. A daemon that is
/// alive but momentarily starved — a long store-health scan, SIGSTOP, a
/// laptop waking from sleep with a minutes-old heartbeat — publishes a stale
/// timestamp while running perfectly well. Judging it dead on the timestamp
/// alone is what made every `mur` hook spawn another murmurd, prompt after
/// prompt, until Activity Monitor showed five of them (issue 014).
///
/// Returns false on any IO or parse error.
pub fn is_daemon_healthy() -> bool {
    let lock_path = dirs::home_dir()
        .map(|h| h.join(".mur").join("murmurd.lock"))
        .unwrap_or_default();
    daemon_healthy_for_lock(&lock_path)
}

fn daemon_healthy_for_lock(lock_path: &std::path::Path) -> bool {
    let Ok(raw) = std::fs::read_to_string(lock_path) else {
        return false;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return false;
    };
    // A live pid settles it — no timestamp can overrule a running process.
    if let Some(pid) = v.get("pid").and_then(|p| p.as_u64())
        && mur_common::lock_file::pid_alive(pid as u32)
    {
        return true;
    }
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
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

#[cfg(test)]
mod daemon_health_tests {
    use super::*;
    use std::io::Write;

    /// A pid that is reliably not a running process. Picking an arbitrary
    /// large number (the old 9999) is a coin flip now that liveness counts:
    /// if the machine happens to have that pid, the "stale" tests invert.
    fn dead_pid() -> u32 {
        // Spawn something trivial, reap it, reuse its pid: guaranteed to have
        // existed and guaranteed to be gone.
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn probe");
        let pid = child.id();
        let _ = child.wait();
        pid
    }

    fn write_lock_with_pid(path: &std::path::Path, pid: u32, heartbeat_at: &str) {
        let state = serde_json::json!({
            "pid": pid,
            "started_at": heartbeat_at,
            "heartbeat_at": heartbeat_at,
        });
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(serde_json::to_string(&state).unwrap().as_bytes())
            .unwrap();
    }

    fn write_lock(path: &std::path::Path, heartbeat_at: &str) {
        write_lock_with_pid(path, dead_pid(), heartbeat_at);
    }

    #[test]
    fn stale_lock_returns_false() {
        let dir = tempfile::TempDir::new().unwrap();
        let lock = dir.path().join("murmurd.lock");
        let old_ts = chrono::Utc::now() - chrono::Duration::seconds(60);
        write_lock(&lock, &old_ts.to_rfc3339());
        assert!(
            !daemon_healthy_for_lock(&lock),
            "stale lock should return false"
        );
    }

    /// The 014 regression, in one test: heartbeat minutes old, process very
    /// much alive. Anything that reports this as unhealthy spawns a duplicate.
    #[test]
    fn live_pid_with_stale_heartbeat_is_healthy() {
        let dir = tempfile::TempDir::new().unwrap();
        let lock = dir.path().join("murmurd.lock");
        let old_ts = chrono::Utc::now() - chrono::Duration::seconds(600);
        write_lock_with_pid(&lock, std::process::id(), &old_ts.to_rfc3339());
        assert!(
            daemon_healthy_for_lock(&lock),
            "a live pid must outrank a stale heartbeat — this is what spawned duplicates"
        );
    }

    /// The other direction still has to work: a dead daemon whose last
    /// heartbeat was recent (crashed within the window) must NOT block a
    /// respawn once its pid is gone… but a fresh heartbeat is still accepted,
    /// because a daemon that wrote one 5 s ago is almost certainly mid-start.
    #[test]
    fn dead_pid_with_fresh_heartbeat_still_healthy() {
        let dir = tempfile::TempDir::new().unwrap();
        let lock = dir.path().join("murmurd.lock");
        let now_ts = chrono::Utc::now();
        write_lock_with_pid(&lock, dead_pid(), &now_ts.to_rfc3339());
        assert!(daemon_healthy_for_lock(&lock));
    }

    #[test]
    fn dead_pid_and_stale_heartbeat_is_unhealthy() {
        let dir = tempfile::TempDir::new().unwrap();
        let lock = dir.path().join("murmurd.lock");
        let old_ts = chrono::Utc::now() - chrono::Duration::seconds(600);
        write_lock_with_pid(&lock, dead_pid(), &old_ts.to_rfc3339());
        assert!(
            !daemon_healthy_for_lock(&lock),
            "genuinely dead daemon must be respawnable"
        );
    }

    #[test]
    fn fresh_lock_returns_true() {
        let dir = tempfile::TempDir::new().unwrap();
        let lock = dir.path().join("murmurd.lock");
        let now_ts = chrono::Utc::now();
        write_lock(&lock, &now_ts.to_rfc3339());
        assert!(
            daemon_healthy_for_lock(&lock),
            "fresh lock should return true"
        );
    }

    #[test]
    fn missing_lock_returns_false() {
        let dir = tempfile::TempDir::new().unwrap();
        let lock = dir.path().join("nonexistent.lock");
        assert!(
            !daemon_healthy_for_lock(&lock),
            "missing lock should return false"
        );
    }

    #[test]
    fn malformed_lock_returns_false() {
        let dir = tempfile::TempDir::new().unwrap();
        let lock = dir.path().join("murmurd.lock");
        std::fs::write(&lock, b"not json").unwrap();
        assert!(
            !daemon_healthy_for_lock(&lock),
            "malformed lock should return false"
        );
    }
}

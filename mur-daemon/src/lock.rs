use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockState {
    pub pid: u32,
    pub started_at: DateTime<Utc>,
    pub heartbeat_at: DateTime<Utc>,
}

pub fn lock_path() -> PathBuf {
    dirs::home_dir()
        .expect("no home dir")
        .join(".mur")
        .join("murmurd.lock")
}

pub fn write_lock(path: &Path, state: &LockState) -> Result<()> {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(state)?)?;
    Ok(())
}

pub fn read_lock(path: &Path) -> Result<Option<LockState>> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(serde_json::from_str(&s)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Returns true if the lock is fresh (heartbeat < 30 s ago).
pub fn is_healthy(state: &LockState) -> bool {
    let age = Utc::now()
        .signed_duration_since(state.heartbeat_at)
        .num_seconds();
    age < 30
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_and_read_lock_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.lock");
        let state = LockState {
            pid: std::process::id(),
            started_at: Utc::now(),
            heartbeat_at: Utc::now(),
        };
        write_lock(&path, &state).unwrap();
        let loaded = read_lock(&path).unwrap().unwrap();
        assert_eq!(loaded.pid, state.pid);
    }

    #[test]
    fn read_lock_missing_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.lock");
        assert!(read_lock(&path).unwrap().is_none());
    }

    #[test]
    fn is_healthy_true_for_fresh_heartbeat() {
        let state = LockState {
            pid: 1,
            started_at: Utc::now(),
            heartbeat_at: Utc::now(),
        };
        assert!(is_healthy(&state));
    }

    #[test]
    fn is_healthy_false_for_stale_heartbeat() {
        use chrono::TimeDelta;
        let state = LockState {
            pid: 1,
            started_at: Utc::now(),
            heartbeat_at: Utc::now() - TimeDelta::seconds(60),
        };
        assert!(!is_healthy(&state));
    }
}

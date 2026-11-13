//! Per-project attempt history: last attempt, failure count, backoff.
//!
//! Lives at `<mur_home>/`[`STATE_FILE`], beside `indexes/codebase/` rather
//! than inside it, so index discovery never mistakes it for an index, and
//! under a tree no agent profile can write. Persisted so a failing project's
//! backoff survives a daemon restart.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use mur_common::config::CodebaseReindexConfig;
use serde::{Deserialize, Serialize};

/// Daemon state file, relative to the MUR home.
pub(super) const STATE_FILE: &str = "indexes/reindex-state.json";

#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct ReindexState {
    #[serde(default)]
    projects: BTreeMap<String, ProjectState>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct ProjectState {
    /// Unix seconds of the last attempt, successful or not.
    #[serde(default)]
    pub last_attempt: i64,
    /// Consecutive failures since the last success.
    #[serde(default)]
    pub failures: u32,
    /// Unix seconds before which a failed project is not retried.
    #[serde(default)]
    pub retry_after: i64,
}

impl ReindexState {
    /// Missing file → empty state. An unreadable or unparsable file is also
    /// treated as empty: the worst case is one early retry per project.
    pub fn load(path: &Path) -> Self {
        let Ok(data) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        serde_json::from_str(&data).unwrap_or_else(|e| {
            tracing::warn!(path = %path.display(), error = %e, "reindex state unparsable; starting fresh");
            Self::default()
        })
    }

    /// Temp file + rename, so a crash never leaves a torn document.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(format!(".tmp-{}", std::process::id()));
        let tmp = PathBuf::from(tmp);
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, path).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
    }

    #[cfg(test)]
    pub fn get(&self, name: &str) -> Option<&ProjectState> {
        self.projects.get(name)
    }

    /// `None` when `name` may run at `now`; otherwise the unix second it
    /// becomes eligible (minimum interval or failure backoff, whichever is
    /// later).
    pub fn blocked_until(&self, name: &str, now: i64, cfg: &CodebaseReindexConfig) -> Option<i64> {
        let p = self.projects.get(name)?;
        let min_gap = i64::try_from(cfg.min_interval_secs).unwrap_or(i64::MAX);
        let until = p.last_attempt.saturating_add(min_gap).max(p.retry_after);
        (until > now).then_some(until)
    }

    pub fn record_success(&mut self, name: &str, now: i64) {
        self.projects.insert(
            name.to_string(),
            ProjectState {
                last_attempt: now,
                failures: 0,
                retry_after: 0,
            },
        );
    }

    pub fn record_failure(&mut self, name: &str, now: i64, cfg: &CodebaseReindexConfig) {
        let p = self.projects.entry(name.to_string()).or_default();
        p.failures = p.failures.saturating_add(1);
        let delay = backoff_secs(p.failures, cfg.min_interval_secs, cfg.max_backoff_secs);
        p.last_attempt = now;
        p.retry_after = now.saturating_add(i64::try_from(delay).unwrap_or(i64::MAX));
    }
}

/// `min · 2^(failures−1)`, capped at `max`. `failures` is at least 1 here.
pub(super) fn backoff_secs(failures: u32, min: u64, max: u64) -> u64 {
    let shift = failures.saturating_sub(1).min(63);
    min.saturating_mul(1u64 << shift).min(max)
}

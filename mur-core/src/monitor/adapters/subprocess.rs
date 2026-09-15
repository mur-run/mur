//! `source.type: codex | claude_code` — an agentic subprocess the launcher
//! registered as `<mur_home>/monitor/procs/<id>.json`. The record id is the
//! durable identity; the pid inside it is just one field (spec §MVP Adapter
//! → Codex／Claude Code: "不以 PID 單獨作 durable identity"). No launcher
//! writes these yet (plan-2 wires them); `write_record` is the contract.
//!
//! Liveness reads: exit file → terminal; pid alive → pending with the log
//! length as the progress token; pid gone with no exit file → `unknown`.
//! Losing the stdout pipe is never a failure verdict.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use mur_monitor::adapter::{Observation, SourceAdapter};
use mur_monitor::spec::SourceType;
use mur_monitor::state::Outcome;
use serde::{Deserialize, Serialize};

const MAX_ID_LEN: usize = 96;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessRecord {
    pub pid: u32,
    pub started_at: DateTime<Utc>,
    pub log_path: PathBuf,
    /// Written by the launcher's wait loop with the exit code, once.
    pub exit_path: PathBuf,
}

pub fn procs_dir(mur_home: &Path) -> PathBuf {
    mur_home.join("monitor").join("procs")
}

fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_ID_LEN
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Temp file + rename, like every other MUR YAML/JSON write.
pub fn write_record(mur_home: &Path, id: &str, rec: &ProcessRecord) -> Result<()> {
    if !valid_id(id) {
        anyhow::bail!(
            "process record id: letters, digits, `-` and `_` only, at most {MAX_ID_LEN} chars"
        );
    }
    let dir = procs_dir(mur_home);
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let path = dir.join(format!("{id}.json"));
    let tmp = dir.join(format!(".{id}.json.tmp"));
    std::fs::write(&tmp, serde_json::to_vec_pretty(rec)?)?;
    std::fs::rename(&tmp, &path).with_context(|| format!("rename {}", path.display()))?;
    Ok(())
}

pub fn load_record(mur_home: &Path, id: &str) -> Result<Option<ProcessRecord>> {
    if !valid_id(id) {
        return Ok(None);
    }
    let path = procs_dir(mur_home).join(format!("{id}.json"));
    match std::fs::read(&path) {
        Ok(bytes) => Ok(Some(
            serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?,
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
    }
}

pub fn observe_record(rec: &ProcessRecord) -> Observation {
    if let Ok(raw) = std::fs::read_to_string(&rec.exit_path) {
        return match raw.trim().parse::<i32>() {
            Ok(0) => Observation::terminal(Outcome::Succeeded, "exit 0"),
            Ok(n) => Observation::terminal(Outcome::Failed, format!("exit {n}")),
            Err(_) => Observation::unknown(format!("exit record is not a code: {}", raw.trim())),
        };
    }
    if mur_common::lock_file::pid_alive(rec.pid) {
        let log_len = std::fs::metadata(&rec.log_path)
            .map(|m| m.len())
            .unwrap_or(0);
        return Observation::pending(
            format!("log:{log_len}"),
            format!("pid {} alive, {log_len} bytes of output", rec.pid),
        );
    }
    Observation::unknown(format!(
        "pid {} is gone and there is no exit record",
        rec.pid
    ))
}

pub struct SubprocessAdapter {
    mur_home: PathBuf,
    kind: SourceType,
}

impl SubprocessAdapter {
    pub fn new(mur_home: &Path, kind: SourceType) -> Self {
        Self {
            mur_home: mur_home.to_path_buf(),
            kind,
        }
    }
}

impl SourceAdapter for SubprocessAdapter {
    fn source_type(&self) -> SourceType {
        self.kind
    }

    fn validate_reference(&self, reference: &str) -> Result<(), String> {
        if valid_id(reference) {
            Ok(())
        } else {
            Err(format!(
                "process record id: letters, digits, `-` and `_` only, at most {MAX_ID_LEN} chars"
            ))
        }
    }

    fn observe(&self, reference: &str, _credential_ref: Option<&str>) -> Observation {
        match load_record(&self.mur_home, reference) {
            Ok(Some(rec)) => observe_record(&rec),
            Ok(None) => Observation::unknown(format!(
                "no process record `{reference}` under {}",
                procs_dir(&self.mur_home).display()
            )),
            Err(e) => Observation::unknown(format!("process record unreadable: {e:#}")),
        }
        .redacted()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn rec(dir: &Path, pid: u32) -> ProcessRecord {
        ProcessRecord {
            pid,
            started_at: Utc::now(),
            log_path: dir.join("out.log"),
            exit_path: dir.join("exit"),
        }
    }

    #[cfg(unix)]
    fn dead_pid() -> u32 {
        let mut c = std::process::Command::new("true")
            .spawn()
            .expect("spawn true");
        let pid = c.id();
        c.wait().unwrap();
        pid
    }

    #[test]
    fn alive_process_is_pending_and_log_growth_is_progress() {
        let d = tempfile::tempdir().unwrap();
        let r = rec(d.path(), std::process::id());
        std::fs::write(&r.log_path, "abc").unwrap();
        let a = observe_record(&r);
        std::fs::write(&r.log_path, "abcdef").unwrap();
        let b = observe_record(&r);
        assert_eq!(a.outcome, Outcome::Pending);
        assert_ne!(a.progress_token, b.progress_token);
        assert_eq!(
            observe_record(&r).progress_token,
            b.progress_token,
            "no new output, no progress"
        );
    }

    #[test]
    fn exit_record_decides_terminal_outcome() {
        let d = tempfile::tempdir().unwrap();
        // The exit file is read BEFORE the pid is consulted, so this test
        // does not need a dead pid — the test process's own (certainly
        // alive) pid is fine and keeps this portable to Windows, where
        // `true` does not exist.
        let r = rec(d.path(), std::process::id());
        std::fs::write(&r.exit_path, "0\n").unwrap();
        assert_eq!(observe_record(&r).outcome, Outcome::Succeeded);
        std::fs::write(&r.exit_path, "3").unwrap();
        assert_eq!(observe_record(&r).outcome, Outcome::Failed);
        std::fs::write(&r.exit_path, "garbage").unwrap();
        assert_eq!(observe_record(&r).outcome, Outcome::Unknown);
    }

    #[cfg(unix)]
    #[test]
    fn gone_without_exit_record_is_unknown_not_failed() {
        let d = tempfile::tempdir().unwrap();
        let o = observe_record(&rec(d.path(), dead_pid()));
        assert_eq!(o.outcome, Outcome::Unknown);
        assert!(o.adapter_error.unwrap().contains("no exit record"));
    }

    #[test]
    fn record_round_trips_and_missing_is_unknown() {
        let d = tempfile::tempdir().unwrap();
        let a = SubprocessAdapter::new(d.path(), SourceType::Codex);
        assert_eq!(a.source_type(), SourceType::Codex);
        assert_eq!(a.observe("sess-1", None).outcome, Outcome::Unknown);
        let r = rec(d.path(), std::process::id());
        write_record(d.path(), "sess-1", &r).unwrap();
        assert_eq!(load_record(d.path(), "sess-1").unwrap().unwrap().pid, r.pid);
        assert_eq!(a.observe("sess-1", None).outcome, Outcome::Pending);
        assert!(a.validate_reference("sess-1").is_ok());
        assert!(a.validate_reference("../etc").is_err());
    }
}

//! Atomic read/write of `~/.mur/runs/<run_id>/run.json`. Pure I/O — no policy.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::RunState;

/// `<mur_home>/runs`.
pub fn runs_dir(mur_home: &Path) -> PathBuf {
    mur_home.join("runs")
}

/// `<mur_home>/runs/<run_id>/run.json`.
pub fn run_path(mur_home: &Path, run_id: &str) -> PathBuf {
    runs_dir(mur_home).join(run_id).join("run.json")
}

/// Write `run` atomically: serialize to `run.json.tmp`, then rename over
/// `run.json`. A reader therefore never observes a half-written record — the
/// same temp-file-plus-rename discipline `store/yaml.rs` uses.
pub fn save(mur_home: &Path, run: &RunState) -> Result<()> {
    let dir = runs_dir(mur_home).join(&run.run_id);
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let final_path = dir.join("run.json");
    let tmp_path = dir.join("run.json.tmp");
    let body = serde_json::to_vec_pretty(run).context("serialize run state")?;
    {
        let mut f = std::fs::File::create(&tmp_path)
            .with_context(|| format!("create {}", tmp_path.display()))?;
        f.write_all(&body)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp_path, &final_path)
        .with_context(|| format!("rename into {}", final_path.display()))?;
    Ok(())
}

/// Read one run record. `Ok(None)` when the file does not exist — a run that
/// was never recorded is not an error, it is a rebuild candidate.
pub fn load(mur_home: &Path, run_id: &str) -> Result<Option<RunState>> {
    let path = run_path(mur_home, run_id);
    match std::fs::read(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
        Ok(bytes) => Ok(Some(
            serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?,
        )),
    }
}

/// Every run id that has a directory under `runs/`. Missing `runs/` yields an
/// empty list: no runs have happened yet, which is not a failure.
pub fn list_ids(mur_home: &Path) -> Result<Vec<String>> {
    let dir = runs_dir(mur_home);
    let entries = match std::fs::read_dir(&dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e).with_context(|| format!("read_dir {}", dir.display())),
        Ok(entries) => entries,
    };
    let mut ids = Vec::new();
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_dir()
            && let Some(name) = entry.file_name().to_str()
        {
            ids.push(name.to_string());
        }
    }
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_status::{RUN_SCHEMA, RunKind, RunState, State};

    fn sample(run_id: &str) -> RunState {
        RunState {
            schema: RUN_SCHEMA,
            run_id: run_id.to_string(),
            channel_id: Some("chan-1".into()),
            kind: RunKind::Job,
            label: "fan out 3 jobs".into(),
            pid: std::process::id(),
            started_at: chrono::Utc::now(),
            last_heartbeat_at: None,
            state: State::Running,
            steps: vec![],
            blocked_on: None,
            binary_version: "0.0.0-test".into(),
            build_sha: "deadbee".into(),
        }
    }

    #[test]
    fn save_then_load_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let run = sample("run-a");
        save(tmp.path(), &run).unwrap();
        let back = load(tmp.path(), "run-a").unwrap().expect("run.json exists");
        assert_eq!(back.run_id, "run-a");
        assert_eq!(back.state, State::Running);
        assert_eq!(back.kind, RunKind::Job);
    }

    #[test]
    fn load_missing_run_is_none_not_error() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(load(tmp.path(), "nope").unwrap().is_none());
    }

    #[test]
    fn save_leaves_no_temp_file_behind() {
        let tmp = tempfile::tempdir().unwrap();
        save(tmp.path(), &sample("run-b")).unwrap();
        let entries: Vec<_> = std::fs::read_dir(runs_dir(tmp.path()).join("run-b"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            entries,
            vec!["run.json".to_string()],
            "temp file left behind"
        );
    }

    #[test]
    fn list_ids_returns_every_saved_run() {
        let tmp = tempfile::tempdir().unwrap();
        save(tmp.path(), &sample("run-a")).unwrap();
        save(tmp.path(), &sample("run-b")).unwrap();
        let mut ids = list_ids(tmp.path()).unwrap();
        ids.sort();
        assert_eq!(ids, vec!["run-a".to_string(), "run-b".to_string()]);
    }

    #[test]
    fn list_ids_on_missing_runs_dir_is_empty_not_error() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(list_ids(tmp.path()).unwrap().is_empty());
    }
}

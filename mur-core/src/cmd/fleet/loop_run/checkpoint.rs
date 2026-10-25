//! The checkpoint a paused fleet run leaves behind (pause/resume design §5.3).
//!
//! `~/.mur/runs/<run_id>/checkpoint.json`, beside `run.json`, written temp +
//! rename like it. The loop can write there; it cannot write `fleets/`.
//!
//! The checkpoint is written BY THE RUN, so resume never trusts it to widen
//! anything (bounds are `min`-ed with `fleet.yaml`, counters `max`-ed with the
//! signed `run_paused` event). This module only stores and loads it.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use mur_common::skill::manifest::ProcedureStep;
use serde::{Deserialize, Serialize};

/// Bumped when the shape changes; a resume refuses any other value.
pub const CHECKPOINT_SCHEMA: u32 = 1;

/// File name beside `run.json`.
pub const CHECKPOINT_FILE: &str = "checkpoint.json";

/// The largest planned procedure a checkpoint will carry. Over it the PAUSE is
/// refused and the run keeps going: the procedure is never truncated and the
/// iteration never silently re-planned, since re-planning repeats paid,
/// side-effecting delegate work. Initial value to be validated against real
/// router output.
pub const CHECKPOINT_PROCEDURE_MAX_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    pub schema: u32,
    /// The outer run id.
    pub run_id: String,
    /// `"{run_id}-{iteration}"`: the id on the job and the cursor key.
    pub iteration_run_id: String,
    pub fleet: String,
    /// The iteration to re-enter (0-based).
    pub iteration: u32,
    pub spent_usd: f64,
    /// Wall time spent running, pauses excluded.
    pub active_elapsed_ms: u64,
    /// Time since the last agent progress, at the pause.
    pub stuck_elapsed_ms: u64,
    /// Channel seq the loop's marker scan starts at.
    pub start_seq: u64,
    /// The job this iteration claimed, if any.
    pub job_id: Option<String>,
    /// Run-scoped `--goal`, if any.
    pub goal_override: Option<String>,
    /// Bounds resolved at the original start.
    pub deadline_ms: u64,
    pub stuck_ms: Option<u64>,
    pub cost_usd: Option<f64>,
    /// `defer` | `wait`: the HITL policy resolved at the original start.
    pub hitl_unanswered: String,
    pub target_cwd: Option<String>,
    /// The planned steps of the paused iteration, stored whole.
    pub procedure: Vec<ProcedureStep>,
    pub paused_at: String,
    pub binary_version: String,
}

/// `<mur_home>/runs/<run_id>/checkpoint.json`.
pub fn checkpoint_path(mur_home: &Path, run_id: &str) -> PathBuf {
    crate::run_status::store::runs_dir(mur_home)
        .join(run_id)
        .join(CHECKPOINT_FILE)
}

/// Serialized size of a planned procedure, as the checkpoint would store it.
pub fn procedure_bytes(steps: &[ProcedureStep]) -> Result<usize> {
    Ok(serde_json::to_vec(steps)
        .context("serialize the checkpoint procedure")?
        .len())
}

/// Would a checkpoint carry this procedure? `false` when it is over the cap or
/// cannot be serialized: either way the iteration cannot be paused.
pub fn procedure_fits(steps: &[ProcedureStep]) -> bool {
    procedure_bytes(steps).is_ok_and(|n| n <= CHECKPOINT_PROCEDURE_MAX_BYTES)
}

/// Write `cp` atomically. Refuses (without writing) when the serialized
/// procedure is over [`CHECKPOINT_PROCEDURE_MAX_BYTES`].
pub fn save(mur_home: &Path, cp: &Checkpoint) -> Result<()> {
    let procedure_bytes = procedure_bytes(&cp.procedure)?;
    if procedure_bytes > CHECKPOINT_PROCEDURE_MAX_BYTES {
        bail!(
            "the planned procedure is {procedure_bytes} bytes, over the {CHECKPOINT_PROCEDURE_MAX_BYTES}-byte checkpoint limit"
        );
    }
    let path = checkpoint_path(mur_home, &cp.run_id);
    let dir = path.parent().context("checkpoint path has no parent")?;
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let tmp = path.with_extension("json.tmp");
    let body = serde_json::to_vec_pretty(cp).context("serialize the checkpoint")?;
    std::fs::write(&tmp, body).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("rename to {}", path.display()))?;
    Ok(())
}

/// Load a run's checkpoint. `Ok(None)` when there is none; an error when it is
/// there but unreadable or from another schema (resume says "use `stop`").
pub fn load(mur_home: &Path, run_id: &str) -> Result<Option<Checkpoint>> {
    let path = checkpoint_path(mur_home, run_id);
    let body = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
    };
    let cp: Checkpoint = serde_json::from_slice(&body)
        .with_context(|| format!("{} is not a readable checkpoint", path.display()))?;
    if cp.schema != CHECKPOINT_SCHEMA {
        bail!(
            "checkpoint schema {} is not the {CHECKPOINT_SCHEMA} this binary reads",
            cp.schema
        );
    }
    Ok(Some(cp))
}

/// `6m52s`, `30m`, `1h05m`: a frozen run budget as people read it.
fn human_ms(ms: u64) -> String {
    let secs = ms / 1000;
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    match (h, m, s) {
        (0, 0, s) => format!("{s}s"),
        (0, m, 0) => format!("{m}m"),
        (0, m, s) => format!("{m}m{s:02}s"),
        (h, m, _) => format!("{h}h{m:02}m"),
    }
}

/// The frozen spend a paused run carries, as `mur job status` shows it
/// (design §5.5): `paused at <t> · $1.84 of $5.00 · 6m52s of 30m active`.
/// An unbounded cost reads `$1.84 spent` — never a made-up ceiling.
pub fn summary_line(cp: &Checkpoint) -> String {
    let cost = match cp.cost_usd {
        Some(cap) => format!("${:.2} of ${cap:.2}", cp.spent_usd),
        None => format!("${:.2} spent", cp.spent_usd),
    };
    format!(
        "paused at {} · {cost} · {} of {} active",
        cp.paused_at,
        human_ms(cp.active_elapsed_ms),
        human_ms(cp.deadline_ms)
    )
}

/// Marker the run leaves when it cannot honour a pause (design §4: "the pause
/// is refused and the run keeps going"). The run cannot touch `fleets/`, where
/// the `.paused` sentinel lives, so it says so here and `mur fleet pause`,
/// which can, removes the sentinel and reports the reason.
pub const PAUSE_REFUSED_FILE: &str = "pause_refused";

fn refusal_path(mur_home: &Path, run_id: &str) -> PathBuf {
    checkpoint_path(mur_home, run_id).with_file_name(PAUSE_REFUSED_FILE)
}

/// Record that this run cannot be paused right now, and why. Best-effort.
pub fn refuse_pause(mur_home: &Path, run_id: &str, reason: &str) {
    let path = refusal_path(mur_home, run_id);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, reason);
}

/// Why this run said it cannot be paused, if it did.
pub fn pause_refusal(mur_home: &Path, run_id: &str) -> Option<String> {
    std::fs::read_to_string(refusal_path(mur_home, run_id)).ok()
}

/// Withdraw the refusal (the run can be paused again). Idempotent.
pub fn clear_pause_refusal(mur_home: &Path, run_id: &str) {
    let _ = std::fs::remove_file(refusal_path(mur_home, run_id));
}

/// Remove a run's checkpoint (idempotent). Used by `resume` once the run is
/// running again, and by `stop` of a paused run.
pub fn remove(mur_home: &Path, run_id: &str) {
    let _ = std::fs::remove_file(checkpoint_path(mur_home, run_id));
}

/// A filled-in checkpoint for tests in other modules.
#[cfg(test)]
pub(crate) fn sample_for_test(run_id: &str) -> Checkpoint {
    Checkpoint {
        schema: CHECKPOINT_SCHEMA,
        run_id: run_id.into(),
        iteration_run_id: format!("{run_id}-3"),
        fleet: "dev".into(),
        iteration: 3,
        spent_usd: 1.84,
        active_elapsed_ms: 412_000,
        stuck_elapsed_ms: 30_000,
        start_seq: 1288,
        job_id: Some("job-1".into()),
        goal_override: None,
        deadline_ms: 1_800_000,
        stuck_ms: Some(300_000),
        cost_usd: Some(5.0),
        hitl_unanswered: "defer".into(),
        target_cwd: None,
        procedure: vec![ProcedureStep {
            id: Some("s0".into()),
            description: "do the thing".into(),
            ..Default::default()
        }],
        paused_at: "2026-10-22T00:00:00Z".into(),
        binary_version: "0.0.0-test".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(run_id: &str) -> Checkpoint {
        sample_for_test(run_id)
    }

    #[test]
    fn summary_line_shows_frozen_spend_against_its_bounds() {
        let cp = sample("r1");
        assert_eq!(
            summary_line(&cp),
            "paused at 2026-10-22T00:00:00Z · $1.84 of $5.00 · 6m52s of 30m active"
        );
        let unbounded = Checkpoint {
            cost_usd: None,
            active_elapsed_ms: 3_900_000,
            deadline_ms: 7_200_000,
            ..cp
        };
        assert_eq!(
            summary_line(&unbounded),
            "paused at 2026-10-22T00:00:00Z · $1.84 spent · 1h05m of 2h00m active"
        );
        assert_eq!(human_ms(42_000), "42s");
    }

    #[test]
    fn a_checkpoint_round_trips_and_leaves_no_temp_file() {
        let home = tempfile::tempdir().unwrap();
        let cp = sample("r1");
        save(home.path(), &cp).unwrap();
        let back = load(home.path(), "r1")
            .unwrap()
            .expect("checkpoint present");
        assert_eq!(
            serde_json::to_value(&back).unwrap(),
            serde_json::to_value(&cp).unwrap()
        );
        assert!(
            !checkpoint_path(home.path(), "r1")
                .with_extension("json.tmp")
                .exists()
        );
        remove(home.path(), "r1");
        assert!(load(home.path(), "r1").unwrap().is_none());
    }

    #[test]
    fn an_oversized_procedure_is_refused_and_nothing_is_written() {
        let home = tempfile::tempdir().unwrap();
        let mut cp = sample("r2");
        cp.procedure = vec![ProcedureStep {
            description: "x".repeat(CHECKPOINT_PROCEDURE_MAX_BYTES + 1),
            ..Default::default()
        }];
        let err = save(home.path(), &cp).expect_err("over the cap");
        assert!(format!("{err:#}").contains("checkpoint limit"), "{err:#}");
        assert!(!checkpoint_path(home.path(), "r2").exists());
    }

    #[test]
    fn a_foreign_schema_is_refused_not_misread() {
        let home = tempfile::tempdir().unwrap();
        let mut cp = sample("r3");
        cp.schema = CHECKPOINT_SCHEMA + 1;
        save(home.path(), &cp).unwrap();
        assert!(load(home.path(), "r3").is_err());
    }
}

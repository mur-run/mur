//! `mur fleet pause <name>` (pause/resume design §4).
//!
//! The command writes the `.paused` sentinel naming the live run, then waits
//! for the run to answer: either its process exits having written a
//! checkpoint (success), or it says it cannot be paused (refusal, sentinel
//! removed, the run keeps going). It never returns success without a
//! checkpoint on disk.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};

use super::loop_run::{checkpoint, run_lock};
use super::{control, progress, store};

/// How long `pause` waits for the run to reach a safe point. A member turn can
/// take minutes, but the DAG polls the halt signal every second and cancels
/// it, so this only has to cover that cancel plus the checkpoint write.
const PAUSE_WAIT: Duration = Duration::from_secs(120);
const PAUSE_POLL: Duration = Duration::from_millis(250);

/// What `pause` found, decided before anything is written.
#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    /// A live run to target.
    Pause {
        run_id: String,
    },
    /// Already paused with a checkpoint: nothing to do.
    AlreadyPaused,
    Refuse(String),
}

fn verdict(mur_home: &Path, name: &str) -> Verdict {
    if control::is_stopped(mur_home, name) {
        return Verdict::Refuse(format!("fleet '{name}' is stopped; stop wins over pause"));
    }
    let progress = progress::load(mur_home, name).map(|(p, _)| p);
    let held = run_lock::is_held(mur_home, name);
    if let Some(run_id) = control::paused_run_id(mur_home, name) {
        // The run named by the sentinel has exited with a checkpoint: paused.
        if !held && checkpoint::load(mur_home, &run_id).ok().flatten().is_some() {
            return Verdict::AlreadyPaused;
        }
    }
    if !held {
        return Verdict::Refuse(format!("fleet '{name}' has no running run to pause"));
    }
    let Some(p) = progress else {
        return Verdict::Refuse(format!("fleet '{name}' has no running run to pause"));
    };
    if p.finished_at.is_some() || p.paused_at.is_some() {
        return Verdict::Refuse(format!("fleet '{name}' has no running run to pause"));
    }
    Verdict::Pause { run_id: p.run_id }
}

/// `mur fleet pause <name>`.
pub fn cmd_fleet_pause(mur_home: &Path, name: &str) -> Result<()> {
    let _ = store::load_fleet(mur_home, name)?;
    match verdict(mur_home, name) {
        Verdict::Refuse(msg) => bail!("{msg}"),
        Verdict::AlreadyPaused => {
            println!(
                "Fleet '{name}' is already paused. Continue it with `mur fleet resume {name}`."
            );
            Ok(())
        }
        Verdict::Pause { run_id } => request_pause(mur_home, name, &run_id),
    }
}

fn request_pause(mur_home: &Path, name: &str, run_id: &str) -> Result<()> {
    checkpoint::clear_pause_refusal(mur_home, run_id);
    let sentinel = control::paused_path(mur_home, name);
    let body = format!(
        "run_id: {run_id}\nrequested_at: {}\n",
        chrono::Utc::now().to_rfc3339()
    );
    let tmp = sentinel.with_extension("tmp");
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, &sentinel)?;
    println!("Pausing fleet '{name}' (run {run_id})…");

    let deadline = Instant::now() + PAUSE_WAIT;
    loop {
        match poll(mur_home, name, run_id, &mut || {
            run_lock::is_held(mur_home, name)
        }) {
            Poll::Paused => {
                println!(
                    "Fleet '{name}' paused. Continue with `mur fleet resume {name}`, or discard with `mur fleet stop {name}`."
                );
                return Ok(());
            }
            Poll::Refused(why) => {
                let _ = std::fs::remove_file(&sentinel);
                bail!("fleet '{name}' cannot be paused, the run keeps going: {why}");
            }
            Poll::Stopped => {
                let _ = std::fs::remove_file(&sentinel);
                bail!("fleet '{name}' was stopped while pausing; stop wins over pause");
            }
            Poll::Finished => {
                // The run ended without a checkpoint (it finished first, or died).
                let _ = std::fs::remove_file(&sentinel);
                bail!(
                    "fleet '{name}': the run finished before the pause took effect — nothing to resume"
                );
            }
            Poll::Waiting => {}
        }
        if Instant::now() >= deadline {
            // Leave the sentinel: the run may still reach a safe point, and
            // a second `pause` is idempotent. Say what is true.
            bail!(
                "fleet '{name}': the run has not reached a safe point after {}s; the pause is still requested — check `mur fleet status {name}`",
                PAUSE_WAIT.as_secs()
            );
        }
        std::thread::sleep(PAUSE_POLL);
    }
}

/// What one look at the run found while `pause` waits.
#[derive(Debug, PartialEq, Eq)]
enum Poll {
    Paused,
    Refused(String),
    Stopped,
    Finished,
    Waiting,
}

/// One poll of the pausing run. `is_held` is the run-lock probe, injected so a
/// test can release the lock at an exact point.
fn poll(mur_home: &Path, name: &str, run_id: &str, is_held: &mut dyn FnMut() -> bool) -> Poll {
    // The checkpoint exists only once the loop wrote it, and the run
    // lock is released only once the process has exited: both together
    // mean the run is halted and resumable.
    if !is_held() && checkpoint::load(mur_home, run_id).ok().flatten().is_some() {
        return Poll::Paused;
    }
    if let Some(why) = checkpoint::pause_refusal(mur_home, run_id) {
        return Poll::Refused(why);
    }
    if control::is_stopped(mur_home, name) {
        return Poll::Stopped;
    }
    if !is_held() {
        // Re-read before concluding (design §8.5): the loop may have
        // checkpointed and released the lock between the first look and
        // this one. That run paused; it did not finish.
        if checkpoint::load(mur_home, run_id).ok().flatten().is_some() {
            return Poll::Paused;
        }
        return Poll::Finished;
    }
    Poll::Waiting
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::fleet::Fleet;

    fn save_dev(home: &Path) {
        store::save_fleet(
            home,
            &Fleet {
                name: "dev".into(),
                display_name: String::new(),
                goal: "g".into(),
                router: None,
                members: vec!["pm".into()],
                channel_id: "fleet-dev".into(),
                procedure: vec![],
                team_id: None,
                rules: vec![],
                skills: vec![],
                loop_cfg: None,
                parallel: None,
                hitl: None,
                requires_programs: vec![],
                limits: None,
                needs: vec![],
            },
        )
        .unwrap();
    }

    #[test]
    fn pause_with_no_running_run_is_refused_and_writes_nothing() {
        let home = tempfile::tempdir().unwrap();
        save_dev(home.path());
        let err = cmd_fleet_pause(home.path(), "dev").expect_err("nothing to pause");
        assert_eq!(format!("{err}"), "fleet 'dev' has no running run to pause");
        assert!(!control::paused_path(home.path(), "dev").exists());
    }

    #[test]
    fn pause_on_a_stopped_fleet_is_refused() {
        let home = tempfile::tempdir().unwrap();
        save_dev(home.path());
        control::cmd_fleet_stop(home.path(), "dev").unwrap();
        let err = cmd_fleet_pause(home.path(), "dev").expect_err("stopped");
        assert_eq!(
            format!("{err}"),
            "fleet 'dev' is stopped; stop wins over pause"
        );
        assert!(!control::paused_path(home.path(), "dev").exists());
    }

    fn checkpoint_for(run_id: &str) -> checkpoint::Checkpoint {
        checkpoint::Checkpoint {
            schema: checkpoint::CHECKPOINT_SCHEMA,
            run_id: run_id.into(),
            iteration_run_id: format!("{run_id}-0"),
            fleet: "dev".into(),
            iteration: 0,
            spent_usd: 0.0,
            active_elapsed_ms: 0,
            stuck_elapsed_ms: 0,
            start_seq: 0,
            job_id: None,
            goal_override: None,
            deadline_ms: 60_000,
            stuck_ms: None,
            cost_usd: None,
            hitl_unanswered: "defer".into(),
            target_cwd: None,
            procedure: vec![],
            paused_at: "t".into(),
            binary_version: "test".into(),
        }
    }

    /// Design §8.5 / §10 "Stale sentinel": the lock is held on the first
    /// look, then the loop checkpoints and releases it before the second.
    /// That run paused; `pause` must say so, not "finished".
    #[test]
    fn a_run_that_checkpoints_while_pause_looks_is_reported_paused() {
        let home = tempfile::tempdir().unwrap();
        save_dev(home.path());
        let mut calls = 0;
        let mut is_held = || {
            calls += 1;
            if calls == 1 {
                return true;
            }
            checkpoint::save(home.path(), &checkpoint_for("r1")).unwrap();
            false
        };
        assert_eq!(poll(home.path(), "dev", "r1", &mut is_held), Poll::Paused);
    }

    /// Negative control: the lock goes away with no checkpoint — finished.
    #[test]
    fn a_run_that_exits_without_a_checkpoint_is_reported_finished() {
        let home = tempfile::tempdir().unwrap();
        save_dev(home.path());
        let mut calls = 0;
        let mut is_held = || {
            calls += 1;
            calls == 1
        };
        assert_eq!(poll(home.path(), "dev", "r1", &mut is_held), Poll::Finished);
    }

    #[test]
    fn pause_unknown_fleet_errors() {
        let home = tempfile::tempdir().unwrap();
        assert!(cmd_fleet_pause(home.path(), "nope").is_err());
    }
}

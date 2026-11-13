//! Run one `mur project index-worker` child for a stale index, under the
//! same lock protocol the CLI background path uses, and read the outcome
//! from the worker's progress file (its exit code is 0 either way).

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use mur_core::codebase::{CodebaseIndex, IndexProgress, IndexStatus};
use tokio::sync::watch;

use super::reconcile::Job;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Outcome {
    /// Worker reported `Done`.
    Success,
    /// Worker reported `Error`, crashed, timed out, or could not be spawned.
    Failure(String),
    /// Another live process holds the index lock; try again later.
    Busy,
    /// The daemon is shutting down; the child was killed. Not a failure.
    Cancelled,
}

/// What a job needs from its surroundings.
#[derive(Debug, Clone)]
pub(super) struct ExecEnv {
    pub mur_home: PathBuf,
    pub indexes_dir: PathBuf,
    /// `None` when [`mur_bin`] could not place the CLI; every job then fails
    /// (and backs off) instead of running some other `mur`.
    pub mur_bin: Option<PathBuf>,
    pub timeout: Duration,
}

/// Arguments after the `mur` binary. Both opt-outs are always present: a
/// daemon rebuild must not pop a desktop notification per agent commit, and
/// must not let agent-writable `core.hooksPath` choose where it writes.
pub(super) fn worker_args(job: &Job) -> Vec<OsString> {
    vec![
        "project".into(),
        "index-worker".into(),
        job.name.clone().into(),
        job.root.clone().into_os_string(),
        "--no-notify".into(),
        "--no-hook".into(),
    ]
}

/// Reported when the daemon cannot tell where the `mur` CLI is.
pub(super) const NO_MUR_BIN: &str =
    "cannot locate the mur binary beside the daemon (set MUR_BIN to its path)";

/// The `mur` CLI shipped beside this daemon; `MUR_BIN` overrides for
/// development installs. `None` when the daemon cannot find its own path.
/// There is no bare-`mur` fallback: the daemon runs unsandboxed and must not
/// pick up whatever `mur` its `PATH` offers.
pub(super) fn mur_bin() -> Option<PathBuf> {
    resolve_mur_bin(std::env::var_os("MUR_BIN"), std::env::current_exe().ok())
}

pub(super) fn resolve_mur_bin(
    overridden: Option<OsString>,
    exe: Option<PathBuf>,
) -> Option<PathBuf> {
    overridden
        .map(PathBuf::from)
        .or_else(|| exe?.parent().map(|d| d.join("mur")))
}

pub(super) async fn run(env: &ExecEnv, job: &Job, mut shutdown: watch::Receiver<bool>) -> Outcome {
    let index = CodebaseIndex::in_dir(&env.indexes_dir, &job.name, &job.root);
    match index.try_acquire_lock() {
        Ok(true) => {}
        Ok(false) => return Outcome::Busy,
        Err(e) => return Outcome::Failure(format!("acquiring index lock: {e:#}")),
    }
    let me = std::process::id();
    // A crashed worker must not leave a stale "done" behind to be misread.
    let _ = index.write_progress(&progress(IndexStatus::Running, None));

    let Some(mur_bin) = env.mur_bin.as_deref() else {
        index.release_lock_if_held_by(me);
        return fail(&index, NO_MUR_BIN.into());
    };
    let mut cmd = tokio::process::Command::new(mur_bin);
    cmd.args(worker_args(job))
        .env(mur_common::home::MUR_HOME_ENV, &env.mur_home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        // Same process group as the daemon on purpose: launchd / systemd
        // tearing the daemon down reaps the child with it.
        .kill_on_drop(true);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            index.release_lock_if_held_by(me);
            return fail(&index, format!("spawning {}: {e}", mur_bin.display()));
        }
    };
    let Some(pid) = child.id() else {
        index.release_lock_if_held_by(me);
        return fail(&index, "worker exited before it could be tracked".into());
    };
    if let Err(e) = index.hand_lock_to(pid) {
        tracing::warn!(index = %job.name, error = %e, "could not hand index lock to worker");
    }

    let outcome = tokio::select! {
        status = child.wait() => match status {
            Ok(_) => outcome_from_progress(index.read_progress()),
            Err(e) => Outcome::Failure(format!("waiting for worker: {e}")),
        },
        () = tokio::time::sleep(env.timeout) => {
            let _ = child.kill().await;
            Outcome::Failure(format!("timed out after {}s", env.timeout.as_secs()))
        },
        () = stopped(&mut shutdown) => {
            let _ = child.kill().await;
            Outcome::Cancelled
        },
    };

    index.release_lock_if_held_by(pid);
    index.release_lock_if_held_by(me);
    match &outcome {
        Outcome::Failure(msg) if !matches!(index.read_progress(), Some(p) if p.status == IndexStatus::Error) =>
        {
            let _ = index.write_progress(&progress(IndexStatus::Error, Some(msg.clone())));
        }
        Outcome::Cancelled => {
            let msg = "daemon stopped before the rebuild finished".to_string();
            let _ = index.write_progress(&progress(IndexStatus::Error, Some(msg)));
        }
        _ => {}
    }
    outcome
}

/// Resolves once `shutdown` reads `true` (or its sender is gone). Drops the
/// borrowed `watch::Ref` before returning, so the caller's future stays `Send`.
pub(super) async fn stopped(shutdown: &mut watch::Receiver<bool>) {
    let _ = shutdown.wait_for(|stop| *stop).await;
}

fn fail(index: &CodebaseIndex, msg: String) -> Outcome {
    let _ = index.write_progress(&progress(IndexStatus::Error, Some(msg.clone())));
    Outcome::Failure(msg)
}

/// Only an explicit `Done` counts. `Running` after exit means the worker
/// died mid-build; a missing or unparsable file means it never got going.
pub(super) fn outcome_from_progress(p: Option<IndexProgress>) -> Outcome {
    match p {
        Some(p) if p.status == IndexStatus::Done => Outcome::Success,
        Some(p) if p.status == IndexStatus::Error => Outcome::Failure(
            p.error_message
                .unwrap_or_else(|| "worker reported an error".into()),
        ),
        Some(_) => Outcome::Failure("worker exited mid-build".into()),
        None => Outcome::Failure("worker left no progress".into()),
    }
}

fn progress(status: IndexStatus, error_message: Option<String>) -> IndexProgress {
    let now = chrono::Utc::now().to_rfc3339();
    let finished = (status != IndexStatus::Running).then(|| now.clone());
    IndexProgress {
        errors: usize::from(status == IndexStatus::Error),
        status,
        total_chunks: 0,
        done_chunks: 0,
        started_at: now,
        finished_at: finished,
        error_message,
    }
}

/// The meta for `name` under `indexes_dir`.
#[cfg(test)]
pub(super) fn meta_path(indexes_dir: &std::path::Path, name: &str) -> PathBuf {
    indexes_dir.join(format!("{name}.meta.json"))
}

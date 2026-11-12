//! Rebuild codebase indexes that sandboxed agents could not refresh.
//!
//! A sealed agent's post-commit hook cannot write `indexes/codebase/`, so it
//! drops an empty wake-up into `<mur_home>/`[`REINDEX_REQUEST_DIR`]. This
//! task, running outside every sandbox, drains that inbox (`intake`), decides
//! on its own which indexes are stale (`reconcile`), and runs the index
//! worker for each under the usual lock (`exec`). Wake-ups only say "look";
//! what gets rebuilt is decided from files no agent can write. Spec:
//! `docs/superpowers/specs/2026-11-12-sandboxed-codebase-reindex-design.md`.

mod exec;
mod intake;
mod reconcile;
mod state;

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use mur_common::config::{CodebaseReindexConfig, Config};
use mur_common::reindex_request::REINDEX_REQUEST_DIR;
use tokio::sync::watch;
use tokio::task::{JoinHandle, JoinSet};

use exec::{ExecEnv, Outcome};
use reconcile::Job;
use state::{ReindexState, STATE_FILE};

/// Start the reindex task. `None` (and no task at all) when
/// `codebase_reindex.enabled` is off. The task returns once `shutdown`
/// flips to `true` and every in-flight worker has been killed and its lock
/// released.
pub fn spawn(mur_dir: PathBuf, shutdown: watch::Receiver<bool>) -> Option<JoinHandle<()>> {
    let cfg = Config::load_or_default(&mur_dir.join("config.yaml")).codebase_reindex;
    if !cfg.enabled {
        tracing::info!("codebase reindex disabled (codebase_reindex.enabled: false)");
        return None;
    }
    let env = ExecEnv {
        indexes_dir: mur_core::codebase::indexes_dir(&mur_dir),
        mur_home: mur_dir,
        mur_bin: exec::mur_bin(),
        timeout: Duration::from_secs(cfg.job_timeout_secs),
    };
    Some(tokio::spawn(run(Scheduler::new(env, cfg), shutdown)))
}

async fn run(mut s: Scheduler, mut shutdown: watch::Receiver<bool>) {
    let mut tick = tokio::time::interval(Duration::from_secs(s.cfg.poll_secs));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut jobs: JoinSet<(String, Outcome)> = JoinSet::new();
    loop {
        tokio::select! {
            _ = tick.tick() => {
                for job in s.plan(unix_now(), Instant::now(), SystemTime::now()) {
                    let env = s.env.clone();
                    let rx = shutdown.clone();
                    jobs.spawn(async move {
                        let outcome = exec::run(&env, &job, rx).await;
                        (job.name, outcome)
                    });
                }
            }
            Some(done) = jobs.join_next() => match done {
                Ok((name, outcome)) => s.finish(&name, &outcome, unix_now()),
                Err(e) => tracing::error!(error = %e, "reindex job panicked"),
            },
            () = exec::stopped(&mut shutdown) => break,
        }
    }
    // Every job sees the same shutdown signal, kills its child and releases
    // its lock; wait for them so the caller can exit afterwards.
    while let Some(done) = jobs.join_next().await {
        if let Ok((name, outcome)) = done {
            s.finish(&name, &outcome, unix_now());
        }
    }
}

/// Scheduling state between sweeps. Synchronous so tests can drive it.
struct Scheduler {
    env: ExecEnv,
    cfg: CodebaseReindexConfig,
    state: ReindexState,
    state_path: PathBuf,
    inbox: PathBuf,
    running: HashSet<String>,
    /// Reconcile on the next sweep regardless of wake-ups. Starts `true`:
    /// commits made while the daemon was down are picked up at startup.
    pending: bool,
    last_reconcile: Option<Instant>,
    /// Earliest unix second a held-back stale project becomes eligible.
    retry_at: Option<i64>,
}

impl Scheduler {
    fn new(env: ExecEnv, cfg: CodebaseReindexConfig) -> Self {
        let state_path = env.mur_home.join(STATE_FILE);
        Self {
            state: ReindexState::load(&state_path),
            state_path,
            inbox: env.mur_home.join(REINDEX_REQUEST_DIR),
            env,
            cfg,
            running: HashSet::new(),
            pending: true,
            last_reconcile: None,
            retry_at: None,
        }
    }

    /// One sweep: drain the inbox, reconcile if anything asks for it, and
    /// return the jobs to start now (already marked running).
    fn plan(&mut self, now: i64, now_mono: Instant, now_wall: SystemTime) -> Vec<Job> {
        let swept = intake::sweep(&self.inbox, self.cfg.max_intake, now_wall);
        let reconcile_every = Duration::from_secs(self.cfg.reconcile_secs);
        let due = self.pending
            || swept.wakeups > 0
            || self.retry_at.is_some_and(|t| t <= now)
            || self
                .last_reconcile
                .is_none_or(|t| now_mono.duration_since(t) >= reconcile_every);
        if !due {
            return Vec::new();
        }
        let found = reconcile::reconcile(&self.env.indexes_dir);
        self.last_reconcile = Some(now_mono);
        self.pending = found.deferred;
        self.retry_at = None;

        let mut start = Vec::new();
        for job in found.stale {
            if self.running.contains(&job.name) {
                continue;
            }
            if let Some(until) = self.state.blocked_until(&job.name, now, &self.cfg) {
                self.retry_at = Some(self.retry_at.map_or(until, |t| t.min(until)));
                continue;
            }
            if self.running.len() >= self.cfg.max_concurrent {
                self.pending = true;
                continue;
            }
            self.running.insert(job.name.clone());
            start.push(job);
        }
        start
    }

    fn finish(&mut self, name: &str, outcome: &Outcome, now: i64) {
        self.running.remove(name);
        match outcome {
            Outcome::Success => {
                tracing::info!(index = name, "codebase index rebuilt");
                self.state.record_success(name, now);
            }
            Outcome::Failure(why) => {
                tracing::warn!(index = name, error = %why, "codebase reindex failed; backing off");
                self.state.record_failure(name, now, &self.cfg);
            }
            // Someone else is indexing it: look again next sweep.
            Outcome::Busy => {
                self.pending = true;
                return;
            }
            Outcome::Cancelled => return,
        }
        // A slot freed up: anything held back by concurrency can start.
        self.pending = true;
        if let Err(e) = self.state.save(&self.state_path) {
            tracing::warn!(path = %self.state_path.display(), error = %e, "could not save reindex state");
        }
    }
}

fn unix_now() -> i64 {
    chrono::Utc::now().timestamp()
}

#[cfg(test)]
mod tests;

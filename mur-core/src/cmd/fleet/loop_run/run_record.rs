//! The outer loop's run record and heartbeat (spec §3.6).
//!
//! The loop is one run with a handle: recorded when it starts so
//! `mur_job_status <run_id>` answers while it runs, beaten every interval, and
//! closed with the state the stop implies. Best-effort like progress.json — a
//! ledger failure never stops the loop. No sidecar: the parent has no single
//! first channel seq; `mur fleet status` keeps finding the per-iteration runs
//! through theirs.

use std::path::Path;

use mur_common::fleet::Fleet;

use crate::run_status::heartbeat::Heartbeat;

/// Write a `Running` record for `run_id` and start beating it. `None` when the
/// record could not be written (the loop runs on regardless).
///
/// A resumed run reopens the record it paused under: it keeps that record's
/// `started_at`, so the run's age spans the pause instead of restarting at the
/// resume. A fresh run has a fresh id and no record, so it starts now.
pub(super) fn open(mur_home: &Path, fleet: &Fleet, name: &str, run_id: &str) -> Option<Heartbeat> {
    let runs_cfg = mur_common::config::Config::load_or_default(&mur_home.join("config.yaml")).runs;
    let now = chrono::Utc::now();
    let started_at = crate::run_status::store::load(mur_home, run_id)
        .ok()
        .flatten()
        .map_or(now, |prev| prev.started_at);
    let record = crate::run_status::RunState {
        schema: crate::run_status::RUN_SCHEMA,
        run_id: run_id.to_string(),
        channel_id: Some(fleet.channel_id.clone()),
        kind: crate::run_status::RunKind::Fleet,
        label: format!("fleet {name} loop"),
        pid: std::process::id(),
        started_at,
        last_heartbeat_at: Some(now),
        state: crate::run_status::State::Running,
        steps: vec![],
        blocked_on: None,
        binary_version: env!("CARGO_PKG_VERSION").to_string(),
        build_sha: mur_common::build::SHORT_SHA.to_string(),
    };
    match crate::run_status::store::save(mur_home, &record) {
        Ok(()) => Some(Heartbeat::spawn(
            mur_home.to_path_buf(),
            run_id.to_string(),
            std::time::Duration::from_secs(runs_cfg.heartbeat_interval_secs),
        )),
        Err(error) => {
            tracing::warn!(run_id = %run_id, %error, "fleet loop: run record not written; mur_job_status will not see this loop");
            None
        }
    }
}

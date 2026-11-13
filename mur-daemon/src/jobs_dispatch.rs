//! Sweep `~/.mur/inbox/jobs-dispatch-requests/` (#2379): launch `parallel_jobs`
//! runs that a sealed agent asked for but could not sign.
//!
//! A `parallel_jobs` run writes as the router (`mur`), and its write-grant
//! gate parks a `HitlRequest` that is only answerable when signed. A sealed
//! agent cannot read `keys/mur/`, so it drops a request signed with its OWN
//! key instead. This process runs outside every sandbox: the
//! `mur job run-request` child it launches reads the router key from disk,
//! exactly like a terminal run.
//!
//! The request file is writable by every sandboxed agent, so nothing in it is
//! trusted until the signature verifies against the requester's on-disk
//! pubkey, and every gate is then applied again from the user's own config
//! (`mur_core::executor::jobs_request`). The child repeats all of it.

use std::path::{Path, PathBuf};

use anyhow::Context;
use mur_common::config::Config;
use mur_core::executor::jobs_request::{authorize, read_request, spend_run_id, verify_request};
use mur_track::jobs_dispatch_request::{
    JOBS_DISPATCH_ADMITTED_FILE, JOBS_DISPATCH_HOSTED_ENV, JOBS_DISPATCH_RECORD_FILE,
    JOBS_DISPATCH_REQUEST_DIR, JOBS_RUN_LOG_FILE, JobsDispatchRecord, JobsDispatchRequest,
    JobsDispatchStatus,
};

/// Same loop shape and config knobs as the fleet sweep (spec Q2): both
/// describe the daemon handoff, not the kind of run. The heartbeat is written
/// by the fleet sweep alone; this loop shares its tick period.
pub fn spawn(mur_dir: PathBuf) {
    tokio::spawn(async move {
        let dir = mur_dir.join(JOBS_DISPATCH_REQUEST_DIR);
        loop {
            // Re-read every pass: the allowlists are the user's to change at
            // any time, and a revoked agent must stop launching immediately.
            let cfg = Config::try_load(&mur_dir.join("config.yaml"));
            let period = cfg
                .as_ref()
                .map(|c| c.fleet_dispatch.poll_secs)
                .unwrap_or_else(|_| mur_common::config::FleetDispatchConfig::default().poll_secs);
            match &cfg {
                Ok(cfg) => {
                    if let Err(e) = sweep(&mur_dir, &dir, cfg, &crate::fleet_dispatch::mur_bin()) {
                        tracing::warn!(error = %e, "jobs-dispatch sweep failed");
                    }
                }
                // Unreadable config = no allowlist = nothing may launch. The
                // requests stay queued and expire on their freshness window.
                Err(e) => {
                    tracing::warn!(error = %e, "jobs-dispatch: config unreadable; not sweeping")
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(period.max(1))).await;
        }
    });
}

/// One pass over the request inbox. `pub` for the end-to-end test.
pub fn sweep(mur_dir: &Path, dir: &Path, cfg: &Config, mur_bin: &Path) -> anyhow::Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "yaml"))
        .collect();
    paths.sort();
    for path in paths {
        // Single-shot either way: a rejected request must not be retried
        // forever. A launched one lives on as the daemon's own serialized
        // copy, so the inbox file is never needed again.
        let verdict = process_one(mur_dir, &path, cfg, mur_bin);
        let _ = std::fs::remove_file(&path);
        if let Err(e) = verdict {
            tracing::warn!(request = %path.display(), error = %format!("{e:#}"), "jobs dispatch request rejected");
        }
    }
    Ok(())
}

fn process_one(mur_dir: &Path, path: &Path, cfg: &Config, mur_bin: &Path) -> anyhow::Result<()> {
    // Read ONCE. Every later step uses this value, never the inbox path: the
    // inbox is writable by every sandboxed agent, so a reread could execute
    // bytes other than the ones that passed the gates below.
    let req = read_request(path)?;
    admit(mur_dir, &req, cfg, mur_bin)
}

/// Every gate, then the launch, on one already-read request. `pub(crate)`
/// for the substitution test.
pub(crate) fn admit(
    mur_dir: &Path,
    req: &JobsDispatchRequest,
    cfg: &Config,
    mur_bin: &Path,
) -> anyhow::Result<()> {
    // Shape, freshness, signature. From here on `requested_by` is a verified
    // fact; before it, nothing in the file may be written anywhere.
    verify_request(mur_dir, req, Some(cfg.fleet_dispatch.request_max_age_secs))?;
    // Atomically creates the run's directory in the daemon's own tree; an
    // existing one is a replay. Nothing is written before this succeeds, so
    // a replay cannot overwrite the original verdict either.
    let run_dir = spend_run_id(mur_dir, req)?;
    let record_path = run_dir.join(JOBS_DISPATCH_RECORD_FILE);

    let launched = authorize(mur_dir, req, cfg).and_then(|()| launch(&run_dir, req, mur_bin));
    match launched {
        Ok(pid) => {
            write_record(
                &record_path,
                req,
                JobsDispatchStatus::Launched,
                None,
                Some(pid),
            )?;
            tracing::info!(run_id = %req.dispatch.run_id, dispatched_by = %req.dispatch.requested_by, "parallel_jobs dispatched by daemon");
            Ok(())
        }
        Err(e) => {
            // Verified requester: leave the reason where its run handle
            // points, so `mur_job_status <run_id>` does not just stay silent.
            let _ = write_record(
                &record_path,
                req,
                JobsDispatchStatus::Rejected,
                Some(format!("{e:#}")),
                None,
            );
            Err(e)
        }
    }
}

/// Spawn `mur job run-request` on the admitted request, serialized from the
/// verified value into the daemon's own `run_dir`, so the child re-verifies
/// exactly what passed admission. Returns the child's pid.
fn launch(run_dir: &Path, req: &JobsDispatchRequest, mur_bin: &Path) -> anyhow::Result<u32> {
    let kept = run_dir.join(JOBS_DISPATCH_ADMITTED_FILE);
    // `create_new` (O_EXCL): never follows or reuses whatever sits at the
    // name. `spend_run_id` just made the directory and the sandbox denies
    // agents this tree, so this is the backstop if a backend fails to.
    write_new(&kept, serde_yaml_ng::to_string(req)?.as_bytes())
        .context("keep the admitted request")?;
    let log = create_new(&run_dir.join(JOBS_RUN_LOG_FILE)).context("create run log")?;
    let log_err = log.try_clone()?;
    // No signing handoff: this process is unsealed, so the child loads the
    // router's key from disk — the whole point of launching it here. The
    // hosted marker records the run in this daemon-owned tree, never in the
    // requester's home, which the requester can write (and plant links in).
    let mut child = std::process::Command::new(mur_bin)
        .args(["job", "run-request"])
        .arg(&kept)
        .env(JOBS_DISPATCH_HOSTED_ENV, "1")
        .env_remove(mur_common::paths::RUN_OWNER_ENV)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(log))
        .stderr(std::process::Stdio::from(log_err))
        .spawn()
        .with_context(|| format!("spawn {}", mur_bin.display()))?;
    let pid = child.id();
    // Detached; reap on a thread so it never lingers as a zombie.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(pid)
}

fn write_record(
    path: &Path,
    req: &JobsDispatchRequest,
    status: JobsDispatchStatus,
    reason: Option<String>,
    pid: Option<u32>,
) -> anyhow::Result<()> {
    let record = JobsDispatchRecord {
        run_id: req.dispatch.run_id.clone(),
        channel_id: req.dispatch.channel_id.clone(),
        dispatched_by: req.dispatch.requested_by.clone(),
        jobs: req.dispatch.jobs.len(),
        status,
        reason,
        pid,
        decided_at: chrono::Utc::now(),
    };
    let tmp = path.with_extension("json.tmp");
    write_new(&tmp, &serde_json::to_vec_pretty(&record)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Create `path`, refusing anything already there (a symlink included).
fn create_new(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
}

fn write_new(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    create_new(path)?.write_all(bytes)
}

#[cfg(test)]
#[path = "jobs_dispatch_tests.rs"]
mod tests;

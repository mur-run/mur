//! Verify and authorize a signed `parallel_jobs` dispatch request (#2379).
//!
//! Two readers, one rule set: `mur-daemon` decides whether to launch, and the
//! `mur job run-request` child it launches decides again before it runs, so
//! the child never trusts the daemon's verdict. The request file is writable
//! by every sandboxed agent; nothing in it counts until the signature
//! verifies against the requester's own on-disk pubkey, and every grant is
//! then re-derived from the user's config. A request can only ask.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use mur_common::config::{Config, ParallelJobsConfig};
use mur_common::identity::AgentIdentity;
use mur_track::jobs_dispatch_request::{JOBS_DISPATCH_RECORD_FILE, JobsDispatchRequest};

use crate::a2a_dial::canonicalize_agent_name;

/// Parse a request file without trusting anything in it yet.
pub fn read_request(path: &Path) -> Result<JobsDispatchRequest> {
    let text = std::fs::read_to_string(path).context("read request")?;
    serde_yaml_ng::from_str(&text).context("parse request")
}

/// Shape, freshness, and signature. After `Ok`, `requested_by` is a verified
/// fact. Freshness is skipped by the child (`max_age_secs: None`): the daemon
/// already admitted it, and a slow launch must not strand a run whose handle
/// was returned.
pub fn verify_request(
    mur_home: &Path,
    req: &JobsDispatchRequest,
    max_age_secs: Option<u64>,
) -> Result<()> {
    let d = &req.dispatch;
    // Shape first: these strings become path segments below.
    d.check_shape().map_err(anyhow::Error::msg)?;
    if let Some(max) = max_age_secs {
        ensure!(
            req.is_fresh(chrono::Utc::now(), max),
            "outside freshness window"
        );
    }
    // Trust anchor: the identity in the requester's own home, which no other
    // agent can write.
    let identity = AgentIdentity::load(&mur_home.join("agents").join(&d.requested_by))
        .map_err(|e| anyhow::anyhow!("load agent identity: {e}"))?;
    ensure!(
        req.verify(&identity.verifying_key_bytes()),
        "signature verification failed"
    );
    Ok(())
}

/// May `agent` ask for a `parallel_jobs` run at all? `requesters` empty means
/// any agent (the tool's presence was the only gate before #2379, so existing
/// setups keep working); otherwise exact canonical match.
pub fn requester_allowed(mur_home: &Path, cfg: &ParallelJobsConfig, agent: &str) -> bool {
    cfg.requesters.is_empty()
        || cfg
            .requesters
            .iter()
            .any(|r| canonicalize_agent_name(mur_home, r) == agent)
}

/// Re-apply every gate from the user's config. The request's own claims about
/// what is allowed are never consulted.
pub fn authorize(mur_home: &Path, req: &JobsDispatchRequest, cfg: &Config) -> Result<()> {
    let d = &req.dispatch;
    ensure!(
        requester_allowed(mur_home, &cfg.parallel_jobs, &d.requested_by),
        "agent '{}' may not request parallel_jobs (parallel_jobs.requesters)",
        d.requested_by
    );
    let allow: HashSet<String> = cfg
        .parallel_jobs
        .targets
        .iter()
        .map(|t| canonicalize_agent_name(mur_home, t))
        .collect();
    for j in &d.jobs {
        let target = canonicalize_agent_name(mur_home, &j.agent);
        ensure!(
            allow.contains(&target),
            "target '{target}' is not in parallel_jobs.targets"
        );
        // An agent dispatching to itself through the daemon would run with
        // the router's signature on work it authored: same rule as fleets.
        ensure!(
            !target.eq_ignore_ascii_case(&d.requested_by),
            "agent '{}' may not dispatch a job to itself",
            d.requested_by
        );
    }
    Ok(())
}

/// Where the requester's run lives: its own `agents/<agent>/runs/<run_id>/`
/// (#2320), so `mur_job_status` finds the verdict and the run in one place.
pub fn run_dir(mur_home: &Path, req: &JobsDispatchRequest) -> PathBuf {
    mur_common::paths::agent_runs_dir(mur_home, &req.dispatch.requested_by)
        .join(&req.dispatch.run_id)
}

/// `bail!` unless no verdict was recorded for this run id yet: a record means
/// the id is spent, and a replay must not overwrite the original verdict.
pub fn ensure_unspent(mur_home: &Path, req: &JobsDispatchRequest) -> Result<()> {
    if run_dir(mur_home, req)
        .join(JOBS_DISPATCH_RECORD_FILE)
        .exists()
    {
        bail!("run id already used (replay)");
    }
    Ok(())
}

#[cfg(test)]
#[path = "jobs_request_tests.rs"]
mod tests;

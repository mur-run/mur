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
use mur_track::jobs_dispatch_request::{
    JOBS_DISPATCH_STATE_DIR, JobsDispatchRequest, jobs_dispatch_run_dir,
};

use crate::a2a_dial::canonicalize_agent_name;

/// Parse a request file without trusting anything in it yet.
#[allow(dead_code)] // consumed by mur-daemon's jobs sweep; the `mur` bin compiles this module too
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

/// Claim `run_id` in the daemon's own tree: atomically create
/// `<mur_home>/jobs-dispatch/<run_id>/` and return it. An existing entry
/// (directory, file or link) means the id was already decided — a replay.
///
/// The tree is write-denied to every sandboxed agent, so nothing in it was
/// planted; `create_dir` (not `_all`) also refuses to follow a final-component
/// symlink, and the parent is refused if it is itself a link.
#[allow(dead_code)] // consumed by mur-daemon's jobs sweep; the `mur` bin compiles this module too
pub fn spend_run_id(mur_home: &Path, req: &JobsDispatchRequest) -> Result<PathBuf> {
    let root = mur_home.join(JOBS_DISPATCH_STATE_DIR);
    std::fs::create_dir_all(&root).context("create jobs-dispatch dir")?;
    ensure!(
        !std::fs::symlink_metadata(&root)?.file_type().is_symlink(),
        "{} is a symlink; refusing to write through it",
        root.display()
    );
    let dir = jobs_dispatch_run_dir(mur_home, &req.dispatch.run_id);
    match std::fs::create_dir(&dir) {
        Ok(()) => Ok(dir),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            bail!("run id already used (replay)")
        }
        Err(e) => Err(e).with_context(|| format!("create {}", dir.display())),
    }
}

#[cfg(test)]
#[path = "jobs_request_tests.rs"]
mod tests;

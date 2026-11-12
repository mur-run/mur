//! Signed `parallel_jobs` dispatch request (#2379) — how a sealed agent asks
//! the daemon to run a fan-out it cannot sign.
//!
//! A `parallel_jobs` run writes its channel events as the router (`mur`), and
//! the write-grant gate parks a `HitlRequest` that `hitl::authority` answers
//! only when it is signed. A sealed runtime cannot read `keys/mur/` (#850), so
//! it drops one of these (YAML, tmp+rename) into
//! `<mur_home>/inbox/jobs-dispatch-requests/` instead. `mur-daemon`, outside
//! every sandbox, verifies it against the requester's on-disk pubkey and
//! launches `mur job run-request`, which reads the router key from disk like a
//! terminal run. No agent ever holds another agent's private key.
//!
//! There is deliberately no `yes` field: a sandboxed agent may not approve its
//! own write grant (spec Q3). Approval always goes to the human.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use mur_common::identity::{AgentIdentity, verify_bytes};

/// File-drop directory, relative to the MUR home.
pub const JOBS_DISPATCH_REQUEST_DIR: &str = "inbox/jobs-dispatch-requests";

/// The daemon's verdict, beside the run's own record in the requester's
/// `agents/<agent>/runs/<run_id>/`. Its existence also marks the id as spent.
pub const JOBS_DISPATCH_RECORD_FILE: &str = "jobs-dispatch.json";

/// The launched child's stdout+stderr, beside [`JOBS_DISPATCH_RECORD_FILE`].
pub const JOBS_RUN_LOG_FILE: &str = "jobs_run.log";

/// Most jobs one request may carry. Same guardrail as the MCP tool.
pub const MAX_JOBS: usize = 32;

/// Domain tag: a signature over one request type must never verify as another.
const DOMAIN: &str = "mur-jobs-dispatch-request-v1\n";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestedJob {
    pub description: String,
    /// Canonical assignee name, already resolved by the requester. The daemon
    /// re-checks it against `parallel_jobs.targets`; it never trusts it.
    pub agent: String,
}

/// What the requester asks for. Everything here is signed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobsDispatch {
    /// Canonical name of the agent asking; the key that must verify.
    pub requested_by: String,
    pub jobs: Vec<RequestedJob>,
    pub max_concurrency: u32,
    /// Absolute target directory, when the requester had one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default)]
    pub cwd_inferred: bool,
    /// `run-<uuidv7>`, minted by the requester and already handed back to its
    /// caller, so the handle it returned is the one the run is polled under.
    pub run_id: String,
    /// A UUID, minted by the requester for the same reason. The channel itself
    /// is created by the daemon's child, outside the sandbox.
    pub channel_id: String,
}

impl JobsDispatch {
    /// Shape checks every reader applies before trusting any field as a path
    /// segment or a count. Signature and authorization are separate.
    pub fn check_shape(&self) -> Result<(), String> {
        let name_ok = |n: &str| mur_common::agent_name::validate_agent_name(n).is_ok();
        if !name_ok(&self.requested_by) {
            return Err("invalid requesting agent name".into());
        }
        if !(self.run_id.starts_with("run-") && mur_common::paths::is_single_segment(&self.run_id))
        {
            return Err("invalid run id".into());
        }
        if uuid::Uuid::parse_str(&self.channel_id).is_err() {
            return Err("invalid channel id".into());
        }
        if self.jobs.is_empty() || self.jobs.len() > MAX_JOBS {
            return Err(format!(
                "a request carries 1..={MAX_JOBS} jobs (got {})",
                self.jobs.len()
            ));
        }
        if let Some(j) = self.jobs.iter().find(|j| !name_ok(&j.agent)) {
            return Err(format!("invalid job assignee '{}'", j.agent));
        }
        if self.jobs.iter().any(|j| j.description.trim().is_empty()) {
            return Err("a job has an empty description".into());
        }
        if self.max_concurrency == 0 || self.max_concurrency as usize > MAX_JOBS {
            return Err(format!("max_concurrency must be 1..={MAX_JOBS}"));
        }
        if let Some(dir) = &self.cwd
            && !std::path::Path::new(dir).is_absolute()
        {
            return Err("cwd must be absolute".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobsDispatchRequest {
    #[serde(flatten)]
    pub dispatch: JobsDispatch,
    pub requested_at: DateTime<Utc>,
    /// Key-rotation version; recorded, verified against the CURRENT pubkey.
    #[serde(default)]
    pub key_version: u32,
    /// Multibase (Base58Btc) Ed25519 signature over the canonical sign-input.
    pub sig: String,
}

impl JobsDispatchRequest {
    pub fn create(
        dispatch: JobsDispatch,
        identity: &AgentIdentity,
        key_version: u32,
        now: DateTime<Utc>,
    ) -> Self {
        let input = crate::signed_request::sign_input(DOMAIN, &dispatch, &now, key_version);
        Self {
            sig: identity.sign_multibase(&input),
            dispatch,
            requested_at: now,
            key_version,
        }
    }

    /// Fail-closed signature check against `pubkey`.
    pub fn verify(&self, pubkey: &[u8; 32]) -> bool {
        let input = crate::signed_request::sign_input(
            DOMAIN,
            &self.dispatch,
            &self.requested_at,
            self.key_version,
        );
        verify_bytes(pubkey, &input, &self.sig)
    }

    pub fn is_fresh(&self, now: DateTime<Utc>, max_age_secs: u64) -> bool {
        crate::signed_request::is_fresh(self.requested_at, now, max_age_secs)
    }
}

/// The daemon's verdict. Where a run's provenance lives: who asked.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobsDispatchRecord {
    pub run_id: String,
    pub channel_id: String,
    /// The agent whose verified signature requested this run.
    pub dispatched_by: String,
    pub jobs: usize,
    pub status: JobsDispatchStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The launched child's pid, so a reader can tell "launched, not recorded
    /// yet" from "launched, then died before recording". `None` on rejection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    pub decided_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobsDispatchStatus {
    Launched,
    Rejected,
}

#[cfg(test)]
#[path = "jobs_dispatch_request_tests.rs"]
mod tests;

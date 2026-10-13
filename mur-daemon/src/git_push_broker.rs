//! Daemon side of the git push broker (plan T12, spec §4 F14, §6).
//!
//! This first slice holds the parts that do not depend on HITL authority:
//! the broker-wide switch, the parser sandbox shape, per-agent notification
//! aggregation and the enrollment doctor. The approval fold
//! (`fold_approvals` / `HumanApprovalVerifier`) and the inbox sweeper land
//! once #1764 is closed, so they call `mur_core::hitl::authority` directly
//! instead of a local copy of the predicate (spec §4).

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use mur_common::config::Config;
use mur_git_broker::constants::ALLOWED_REF_PREFIX;
use mur_git_broker::policy::RemotePolicy;

/// The only environment the parser child may see. `git.rs` clears the rest;
/// anything credential-shaped must never be added here.
pub const PARSER_ENV_ALLOW: [&str; 4] =
    ["PATH", "LC_ALL", "GIT_CONFIG_NOSYSTEM", "GIT_CONFIG_GLOBAL"];

/// `config.yaml` `git_push.enabled`. Default (and unreadable config) is off.
pub fn sweeper_enabled(mur_dir: &Path) -> bool {
    Config::load_or_default(&mur_dir.join("config.yaml"))
        .git_push
        .enabled
}

/// What the parser sandbox grants, kept free of the runtime's `SandboxPolicy`
/// so it can be asserted on directly. The launcher maps it onto the real one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxPolicyView {
    pub fs_write: Vec<PathBuf>,
    pub network_allowed: bool,
    pub env_allow: Vec<String>,
}

/// The parser writes only its own output dir, has no network and no
/// credential env.
pub fn parser_policy(out_dir: &Path) -> SandboxPolicyView {
    SandboxPolicyView {
        fs_write: vec![out_dir.to_path_buf()],
        network_allowed: false,
        env_allow: PARSER_ENV_ALLOW.iter().map(|s| s.to_string()).collect(),
    }
}

/// One aggregated notification: every request an agent made pending since
/// the last drain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Batch {
    pub agent: String,
    pub request_ids: Vec<String>,
}

/// Notify once per `(agent, request_id)`, and aggregate per agent so a burst
/// of requests is one message, not a flood (spec §4 F16).
#[derive(Debug, Default)]
pub struct Notifier {
    seen: HashSet<(String, String)>,
    queued: BTreeMap<String, Vec<String>>,
}

impl Notifier {
    /// True the first time this request is seen; it is then queued for the
    /// agent's next batch. A repeat is never queued again, even after a drain.
    pub fn should_notify(&mut self, agent: &str, request_id: &str) -> bool {
        if !self
            .seen
            .insert((agent.to_string(), request_id.to_string()))
        {
            return false;
        }
        self.queued
            .entry(agent.to_string())
            .or_default()
            .push(request_id.to_string());
        true
    }

    /// Take the queued batches, one per agent, in agent-name order.
    pub fn drain_batches(&mut self) -> Vec<Batch> {
        std::mem::take(&mut self.queued)
            .into_iter()
            .map(|(agent, request_ids)| Batch { agent, request_ids })
            .collect()
    }
}

/// A reason the doctor refuses a remote's enrollment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnrollmentProblem {
    /// A `creation_base_refs` entry that is not a full `refs/heads/<name>`.
    BaseRefNotFullHead(String),
    /// `ref_prefix` reaches outside the agent namespace.
    RefPrefixTooWide(String),
    /// An agent write grant overlaps the broker-owned directory (Linux only;
    /// the macOS seal denies it regardless).
    GrantCoversBrokerDir(PathBuf),
}

/// Enrollment doctor. Reuses `RemotePolicy::validate_enrollment` per field so
/// the doctor and the broker cannot disagree on what is valid.
pub fn doctor_check_enrollment(p: &RemotePolicy, fs_grants: &[PathBuf]) -> Vec<EnrollmentProblem> {
    let mut out = Vec::new();
    let prefix_only = RemotePolicy {
        creation_base_refs: Vec::new(),
        ..p.clone()
    };
    if prefix_only.validate_enrollment().is_err() {
        out.push(EnrollmentProblem::RefPrefixTooWide(p.ref_prefix.clone()));
    }
    for r in &p.creation_base_refs {
        let one = RemotePolicy {
            creation_base_refs: vec![r.clone()],
            ref_prefix: ALLOWED_REF_PREFIX.to_string(),
            ..p.clone()
        };
        if one.validate_enrollment().is_err() {
            out.push(EnrollmentProblem::BaseRefNotFullHead(r.clone()));
        }
    }
    out.extend(grant_problems(fs_grants));
    out
}

#[cfg(target_os = "linux")]
fn grant_problems(fs_grants: &[PathBuf]) -> Vec<EnrollmentProblem> {
    let Some(home) = mur_common::home::try_mur_home() else {
        return Vec::new();
    };
    let broker = mur_common::git_push::broker_dir(&home);
    fs_grants
        .iter()
        .filter(|g| broker.starts_with(g) || g.starts_with(&broker))
        .map(|g| EnrollmentProblem::GrantCoversBrokerDir(g.clone()))
        .collect()
}

/// macOS: SBPL denies the broker dir under any grant, so this is not a
/// doctor concern there.
#[cfg(not(target_os = "linux"))]
fn grant_problems(_fs_grants: &[PathBuf]) -> Vec<EnrollmentProblem> {
    Vec::new()
}

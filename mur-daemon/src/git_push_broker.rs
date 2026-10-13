//! Daemon side of the git push broker (plan T12, spec §4 F14, §6).
//!
//! The broker-wide switch, the parser sandbox shape, per-agent notification
//! aggregation, the enrollment doctor, and the approval fold. The fold is the
//! only place the broker meets HITL authority: it calls
//! `mur_core::hitl::authority::is_human_authority` and the gate's own
//! response binding, never a local copy of either (spec §4).

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use mur_common::channel::{ChannelEvent, EventKind};
use mur_common::config::Config;
use mur_common::hitl::HitlResponse;
use mur_core::hitl::authority::is_human_authority;
use mur_core::hitl::binding::Requests;
use mur_git_broker::approval::ApprovalProof;
use mur_git_broker::constants::ALLOWED_REF_PREFIX;
use mur_git_broker::pending::RequestKey;
use mur_git_broker::policy::RemotePolicy;
use sha2::{Digest, Sha256};

mod intake;
pub use intake::{
    CODE_NOT_ALLOWLISTED, CODE_NOT_ENROLLED, SWEEP_INTERVAL_SECS, spawn, sweep_inboxes,
};

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

/// A human approval bound to one pending broker row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folded {
    pub proof: ApprovalProof,
    pub key: RequestKey,
}

/// Fold channel events into approvals for the rows that are pending now.
///
/// A row is released only by a `HitlResponse` that is the human's
/// (`is_human_authority`: router-signed, `Human` actor), answers the
/// router-signed `HitlRequest` named by its `hitl_id` with the same
/// `action_hash` in time (the gate's `Requests::bind`), and whose
/// `hitl_id` and hash are exactly the row's. The newest signed decision
/// wins and a deny wins a tie, as in the gate; a deny yields nothing.
///
/// Only rows in `pending` are considered, so an old allow sitting in the log
/// releases nothing once its row is gone. The proof's `event_id` is derived
/// from the signature, so folding the same log twice gives the same id and
/// the broker's tombstone makes the replay inert.
pub fn fold_approvals(
    mur_home: &Path,
    channel_id: &str,
    events: &[ChannelEvent],
    pending: &[(RequestKey, String)],
) -> Vec<Folded> {
    let now = chrono::Utc::now();
    let requests = Requests::collect(mur_home, channel_id, events);
    let mut out = Vec::new();
    for (key, hash) in pending {
        // (signed answer time, allow, event id) of the newest human decision.
        let mut best: Option<(chrono::DateTime<chrono::Utc>, bool, String)> = None;
        for e in events.iter().filter(|e| e.kind == EventKind::HitlResponse) {
            let Ok(r) = serde_json::from_value::<HitlResponse>(e.payload.clone()) else {
                continue;
            };
            if r.hitl_id != key.request_id || r.action_hash != *hash {
                continue;
            }
            if !is_human_authority(mur_home, channel_id, e) {
                continue;
            }
            let Ok(at) = requests.bind(&r, now) else {
                continue;
            };
            let Some(id) = event_id(e) else { continue };
            let newer = best
                .as_ref()
                .is_none_or(|(t, allow, _)| at > *t || (at == *t && *allow && !r.allow));
            if newer {
                best = Some((at, r.allow, id));
            }
        }
        if let Some((_, true, event_id)) = best {
            out.push(Folded {
                proof: ApprovalProof {
                    event_id,
                    request_id: key.request_id.clone(),
                    action_hash: hash.clone(),
                },
                key: key.clone(),
            });
        }
    }
    out
}

/// Stable id for an approval event: a digest of its signature, which covers
/// the channel id and the payload. `seq` and `ts` are store-assigned and
/// unsigned, so a copied line must not become a fresh approval.
fn event_id(e: &ChannelEvent) -> Option<String> {
    let sig = e.sig.as_deref()?;
    Some(hex::encode(Sha256::digest(sig.as_bytes())))
}

/// The adapter the broker's approval path goes through: one channel, one
/// `mur_home`, and [`fold_approvals`] as its only judgement.
#[derive(Debug, Clone)]
pub struct HumanApprovalVerifier {
    mur_home: PathBuf,
    channel_id: String,
}

impl HumanApprovalVerifier {
    pub fn new(mur_home: impl Into<PathBuf>, channel_id: impl Into<String>) -> Self {
        Self {
            mur_home: mur_home.into(),
            channel_id: channel_id.into(),
        }
    }

    /// Proofs for the rows in `pending` that a human approved in this channel.
    pub fn verify(&self, events: &[ChannelEvent], pending: &[(RequestKey, String)]) -> Vec<Folded> {
        fold_approvals(&self.mur_home, &self.channel_id, events, pending)
    }
}

//! `status`: the one reader of "did this SSH grant get used" (SSH setup spec,
//! "Signing mark", reader). Called by the CLI, murmur and Hub.
//!
//! Reads files only, so it works with the runtime stopped. Never writes: a
//! channel the agent has not created yet reads as empty, not created.
//!
//! A pair is "signed" only when a signing mark on the start channel is signed
//! by this agent and names the `(action_hash, issued_at)` of a live allow in
//! the agent's verified decision log. The request file and the agent's own
//! reports are agent-writable and count for nothing here.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::Result;
use chrono::{DateTime, Utc};
use mur_agent_runtime::hitl::integrity;
use mur_agent_runtime::hitl::remembered::{LiveDecision, live_decisions, signed_allows};
use mur_agent_runtime::hitl::ssh_action_hash;
use mur_channel::ChannelService;
use mur_common::agent::{HITL_CHANNEL_FILE, START_CHANNEL_FILE, SshEntitlement};
use mur_common::channel::{ChannelActor, ChannelEvent, EventKind};
use mur_common::identity::AgentIdentity;
use mur_ssh_request::{PendingRequest, SigningMark};

/// Where one granted `(key, host key)` pair stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairState {
    /// A signature left the proxy under the live approval, at `at`.
    Signed { at: DateTime<Utc> },
    /// A live allow exists; no signing seen under it yet.
    Approved,
    /// No live allow. Never "failed": the absence of a mark proves nothing.
    NotApproved,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairStatus {
    pub key_fp: String,
    pub host_key_fp: String,
    /// The `known_hosts` names confirmed for this host key.
    pub host_names: Vec<String>,
    pub action_hash: String,
    pub state: PairState,
    /// Newest mark made under an approval that is no longer live. History,
    /// shown dimmed; it never changes `state`.
    pub last_expired_mark: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestStatus {
    pub request: PendingRequest,
    /// Indices into [`SshStatus::pairs`] whose host entry names this
    /// request's `host:port`. Empty for a host not granted yet.
    pub pairs: Vec<usize>,
}

impl RequestStatus {
    /// A mark on one of its pairs clears the request.
    pub fn cleared(&self, pairs: &[PairStatus]) -> bool {
        self.pairs
            .iter()
            .any(|&i| matches!(pairs[i].state, PairState::Signed { .. }))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SshStatus {
    pub pairs: Vec<PairStatus>,
    pub requests: Vec<RequestStatus>,
    /// Why no decision was trusted, when the log could not be checked. Every
    /// pair then reads `NotApproved`.
    pub decisions_unverified: Option<String>,
}

/// The status of every pair granted in `ssh`, and of every pending request.
pub fn status(mur_home: &Path, agent: &str, ssh: &SshEntitlement) -> SshStatus {
    status_at(mur_home, agent, ssh, Utc::now())
}

pub(crate) fn status_at(
    mur_home: &Path,
    agent: &str,
    ssh: &SshEntitlement,
    now: DateTime<Utc>,
) -> SshStatus {
    let agent_home = mur_home.join("agents").join(agent);
    let (folds, decisions_unverified) = match decisions(mur_home, agent, &agent_home, now) {
        Ok(f) => (f, None),
        Err(e) => (Folds::default(), Some(format!("{e:#}"))),
    };
    let marks = marks(mur_home, agent, &agent_home);

    let mut pairs = Vec::new();
    for k in &ssh.keys {
        for h in &k.allow_hosts {
            let hash = ssh_action_hash(agent, &k.fingerprint, &h.host_key);
            let live = folds.live.get(&hash).filter(|d| d.allow);
            let mine: Vec<&Mark> = marks.iter().filter(|m| m.action_hash == hash).collect();
            let signed = live.and_then(|d| {
                mine.iter()
                    .filter(|m| m.issued_at == d.issued_at)
                    .map(|m| m.at)
                    .max()
            });
            let state = match (live, signed) {
                (Some(_), Some(at)) => PairState::Signed { at },
                (Some(_), None) => PairState::Approved,
                (None, _) => PairState::NotApproved,
            };
            let last_expired_mark = mine
                .iter()
                .filter(|m| live.is_none_or(|d| d.issued_at != m.issued_at))
                .filter(|m| folds.history.contains(&(hash.clone(), m.issued_at)))
                .map(|m| m.at)
                .max();
            pairs.push(PairStatus {
                key_fp: k.fingerprint.clone(),
                host_key_fp: h.host_key.clone(),
                host_names: h.known_hosts.iter().map(|kh| kh.host.clone()).collect(),
                action_hash: hash,
                state,
                last_expired_mark,
            });
        }
    }

    let requests = mur_ssh_request::read_pending(&agent_home, now)
        .into_iter()
        .map(|request| {
            let name = request.target.known_hosts_name();
            let pairs = pairs
                .iter()
                .enumerate()
                .filter(|(_, p)| p.host_names.contains(&name))
                .map(|(i, _)| i)
                .collect();
            RequestStatus { request, pairs }
        })
        .collect();

    SshStatus {
        pairs,
        requests,
        decisions_unverified,
    }
}

#[derive(Default)]
struct Folds {
    live: HashMap<String, LiveDecision>,
    history: HashSet<(String, DateTime<Utc>)>,
}

/// The channel a marker file names, if it still loads. Never creates one.
fn remembered_channel(svc: &ChannelService, agent_home: &Path, file: &str) -> Option<String> {
    let raw = std::fs::read_to_string(agent_home.join(file)).ok()?;
    let id = raw.trim();
    (!id.is_empty() && svc.exists(id)).then(|| id.to_string())
}

/// Both folds over the decision log, under the same lock and integrity check
/// the gate uses. A log that fails the check yields nothing.
fn decisions(mur_home: &Path, agent: &str, agent_home: &Path, now: DateTime<Utc>) -> Result<Folds> {
    let svc = ChannelService::open(mur_home)?;
    let Some(channel_id) = remembered_channel(&svc, agent_home, HITL_CHANNEL_FILE) else {
        return Ok(Folds::default());
    };
    let pubkey = AgentIdentity::load_pubkey(agent_home)?;
    let _guard = integrity::read_lock(mur_home, &channel_id);
    integrity::verify(mur_home, agent, &channel_id, &pubkey)
        .map_err(|e| anyhow::anyhow!("decision log failed its integrity check: {}", e.reason))?;
    let events = svc.load_events(&channel_id)?;
    Ok(Folds {
        live: live_decisions(&channel_id, &events, &pubkey, now),
        history: signed_allows(&channel_id, &events, &pubkey),
    })
}

struct Mark {
    action_hash: String,
    issued_at: DateTime<Utc>,
    at: DateTime<Utc>,
}

/// Signing marks on the start channel that this agent signed. Matching them
/// to a decision is the caller's job.
fn marks(mur_home: &Path, agent: &str, agent_home: &Path) -> Vec<Mark> {
    let Ok(svc) = ChannelService::open(mur_home) else {
        return Vec::new();
    };
    let Some(channel_id) = remembered_channel(&svc, agent_home, START_CHANNEL_FILE) else {
        return Vec::new();
    };
    let events = svc.load_events(&channel_id).unwrap_or_default();
    events
        .iter()
        .filter_map(|e| mark_of(mur_home, agent, &channel_id, e))
        .collect()
}

fn mark_of(mur_home: &Path, agent: &str, channel_id: &str, e: &ChannelEvent) -> Option<Mark> {
    if e.kind != EventKind::Note {
        return None;
    }
    if !matches!(&e.actor, ChannelActor::Agent { id } if id == agent) {
        return None;
    }
    let m = SigningMark::from_payload(&e.payload)?;
    if !crate::channel_verify::verify_event(mur_home, channel_id, e, true) {
        return None;
    }
    Some(Mark {
        action_hash: m.action_hash,
        issued_at: m.issued_at,
        // The store's time, outside the signature: display only. Editing it
        // moves a time on screen; it cannot create or match a mark.
        at: e.ts,
    })
}

#[cfg(test)]
#[path = "status_tests.rs"]
mod tests;

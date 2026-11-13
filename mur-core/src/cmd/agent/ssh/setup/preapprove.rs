//! Preapproval (SSH setup spec, "Preapproval"): one remembered sign
//! decision per granted pair, written with the runtime stopped and the
//! `running.sentinel` flock held, so the test connection raises no second
//! prompt. Fallible by design: a failure never stops the grant.

use std::path::Path;
use std::sync::Arc;

use chrono::Utc;
use mur_agent_runtime::hitl::ssh::ssh_decision_label;
use mur_agent_runtime::hitl::ssh_action_hash;
use mur_agent_runtime::hitl::store::{ChannelDecisionStore, RecordOutcome};
use mur_agent_runtime::sandbox::policy::{ExecBackend, SandboxPolicy};
use mur_common::agent::{Entitlements, SshEntitlement};
use mur_common::hitl::HitlResponse;
use mur_common::identity::AgentIdentity;

/// The `surface` a preapproval is recorded under, so `hitl list` shows
/// where it came from.
pub const PREAPPROVAL_SURFACE: &str = "ssh-setup";
const PREAPPROVAL_REASON: &str = "approved in SSH setup";

/// One granted pair, with its display labels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pair {
    pub key_fp: String,
    pub key_label: Option<String>,
    pub host_key_fp: String,
    pub host_label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Preapproval {
    /// No restart through setup, so none was written.
    NotAttempted,
    Recorded {
        pairs: usize,
        /// The decision log failed its integrity check and was replaced:
        /// every earlier remembered approval is gone.
        reset_channel: Option<String>,
    },
    /// The seal may not enforce, so a recorded decision would prove nothing.
    Skipped {
        why: String,
    },
    Failed {
        error: String,
    },
}

impl Preapproval {
    /// The card / CLI line. `None` when there is nothing to say.
    pub fn line(&self) -> Option<String> {
        match self {
            Self::NotAttempted => None,
            Self::Recorded {
                reset_channel: None,
                ..
            } => Some("The first signature will not ask again; you approved it here.".into()),
            Self::Recorded {
                reset_channel: Some(_),
                ..
            } => Some("Earlier remembered approvals were reset; the agent will ask again.".into()),
            Self::Skipped { why } => Some(format!(
                "Granted. The first signature will ask you once, because {why}."
            )),
            Self::Failed { error } => Some(format!(
                "Granted. The first signature will ask you once, because the approval could \
                 not be recorded: {error}."
            )),
        }
    }
}

/// Why a preapproval would prove nothing, if it would. It stands in for a
/// human yes only where the agent's own programs cannot forge one: the
/// runtime must refuse to start unsealed, and the seal must keep them from
/// reading the signing key. Landlock cannot carve `identity.key` out of the
/// own home it force-grants, so Linux is skipped until it can.
pub fn skip_reason(ent: &Entitlements) -> Option<String> {
    skip_reason_on(ent, ExecBackend::current())
}

pub fn skip_reason_on(ent: &Entitlements, backend: Option<ExecBackend>) -> Option<String> {
    let Some(b) = backend else {
        return Some("this platform has no kernel sandbox".into());
    };
    if !SandboxPolicy::signing_key_isolated(Some(b)) {
        return Some(
            "this platform's sandbox cannot keep the agent's programs from reading its signing key"
                .into(),
        );
    }
    if !ent.fail_closed_on_sandbox_error {
        return Some(
            "the agent may run without its sandbox (fail_closed_on_sandbox_error is off)".into(),
        );
    }
    None
}

/// Only pairs `ssh` (the profile just written) allows: a preapproval can
/// never widen a grant.
pub fn allowed_pairs(ssh: &SshEntitlement, pairs: &[Pair]) -> Vec<Pair> {
    pairs
        .iter()
        .filter(|p| {
            ssh.keys.iter().any(|k| {
                k.fingerprint == p.key_fp
                    && k.allow_hosts.iter().any(|h| h.host_key == p.host_key_fp)
            })
        })
        .cloned()
        .collect()
}

/// Record the decisions. The caller holds the sentinel flock (`Free`), so
/// no runtime can create or replace the decision channel meanwhile.
pub fn record(
    mur_home: &Path,
    agent: &str,
    key_version: u32,
    ent: &Entitlements,
    pairs: &[Pair],
) -> Preapproval {
    if let Some(why) = skip_reason(ent) {
        return Preapproval::Skipped { why };
    }
    let pairs = allowed_pairs(&ent.ssh, pairs);
    if pairs.is_empty() {
        return Preapproval::Failed {
            error: "no granted pair was found in the profile".into(),
        };
    }
    let agent_home = mur_home.join("agents").join(agent);
    let identity = match AgentIdentity::load(&agent_home) {
        Ok(id) => Arc::new(id),
        Err(e) => {
            return Preapproval::Failed {
                error: format!("the agent's identity could not be loaded ({e})"),
            };
        }
    };
    let store =
        ChannelDecisionStore::new(mur_home.to_path_buf(), agent.into(), identity, key_version);
    let now = Utc::now();
    let mut reset_channel = None;
    for p in &pairs {
        let resp = HitlResponse {
            hitl_id: format!("{PREAPPROVAL_SURFACE}-{}", uuid::Uuid::now_v7()),
            action_hash: ssh_action_hash(agent, &p.key_fp, &p.host_key_fp),
            allow: true,
            reason: PREAPPROVAL_REASON.into(),
            surface: PREAPPROVAL_SURFACE.into(),
            issued_at: Some(now),
            label: Some(ssh_decision_label(
                &p.key_fp,
                p.key_label.as_deref(),
                &p.host_key_fp,
                p.host_label.as_deref(),
            )),
        };
        match store.record_checked(&resp) {
            Ok(RecordOutcome::Recorded) => {}
            Ok(RecordOutcome::RecordedOnFreshChannel { damaged_channel }) => {
                reset_channel = Some(damaged_channel);
            }
            Err(e) => {
                return Preapproval::Failed {
                    error: format!("{e:#}"),
                };
            }
        }
    }
    Preapproval::Recorded {
        pairs: pairs.len(),
        reset_channel,
    }
}

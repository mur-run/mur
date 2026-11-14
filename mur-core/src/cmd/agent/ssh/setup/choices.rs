//! The one rule that turns what a user picked on the card into the
//! `Approval` `apply_and_restart` takes, or says why Approve is unavailable
//! (SSH setup spec, "The card"). murmur and Hub both call it; neither holds
//! a copy of the verdict table.

use mur_ssh_request::validate_user;
use serde::{Deserialize, Serialize};

use super::super::edit::HostGrant;
use super::verdict::{self, HostChoice};
use super::{Approval, SetupPlan, SpawnChange};

/// What the user picked. Every field may be left out; a missing `key_fp`
/// reads as `NotReady::NoKey`, and an empty `user` as "not given".
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Choices {
    pub key_fp: Option<String>,
    #[serde(default)]
    pub pasted: Option<String>,
    #[serde(default)]
    pub ticked: Vec<String>,
    #[serde(default)]
    pub spawn_consented: bool,
    /// Only for the test connection; empty means "not given".
    #[serde(default)]
    pub user: String,
}

/// Why Approve is unavailable. Checked in this order, so a client shows the
/// first thing to fix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub enum NotReady {
    NoKey,
    KeyNotLoaded,
    /// The verdict table refused the host choice; the text says why.
    Host(String),
    SpawnConsent,
    User(String),
}

pub fn build_approval(plan: &SetupPlan, c: &Choices) -> Result<Approval, NotReady> {
    let fp = c.key_fp.as_deref().ok_or(NotReady::NoKey)?;
    let key = match plan.keys_loaded.iter().find(|k| k.fingerprint == fp) {
        Some(k) => k,
        None if plan.keys_unloaded.iter().any(|k| k.fingerprint == fp) => {
            return Err(NotReady::KeyNotLoaded);
        }
        None => return Err(NotReady::NoKey),
    };
    let choice = HostChoice {
        pasted: c.pasted.clone(),
        ticked: c.ticked.clone(),
    };
    let keep = verdict::select(
        plan.host.host(),
        &plan.scanned,
        &plan.verdict,
        &plan.matched,
        &choice,
    )
    .map_err(|e| NotReady::Host(e.to_string()))?;
    if plan.spawn == Some(SpawnChange::NoneToStrict) && !c.spawn_consented {
        return Err(NotReady::SpawnConsent);
    }
    if !c.user.is_empty() {
        validate_user(&c.user).map_err(|e| NotReady::User(e.to_string()))?;
    }
    let label = Some(plan.host.canonical());
    Ok(Approval {
        key_fp: key.fingerprint.clone(),
        key_label: (!key.comment.is_empty()).then(|| key.comment.clone()),
        hosts: keep
            .into_iter()
            .map(|k| HostGrant {
                host_key_fp: k.fingerprint,
                label: label.clone(),
                known_host: Some(k.entry),
            })
            .collect(),
        spawn: plan.spawn,
        spawn_consented: c.spawn_consented,
        restart: true,
    })
}

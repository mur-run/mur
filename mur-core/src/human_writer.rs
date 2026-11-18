//! The one way `mur-core` writes an event as the human (at-mention phase 2,
//! C2, D-4).
//!
//! The event is signed with the human key, the head is sealed by the router
//! (H1), and the write is always anchored. A channel with no live owner
//! record is enrolled first (C1), gated on A0 and on an intact log.
//!
//! There is no unsigned fallback. Inside an agent's sandbox `keys/.human/`
//! is unreadable, so a human write from there fails with the "your own
//! terminal" hint instead of landing as a keyless `Human` line that every
//! reader would demote anyway.

use std::path::Path;

use anyhow::{Context, Result};
use mur_channel::ChannelService;
use mur_channel::service::{ChannelOwner, EnrollGate, LegacySealer};
use mur_common::channel::{ChannelActor, ChannelEvent, EventKind};
use mur_common::identity::AgentIdentity;

use crate::channel_owner::{HumanOwner, identity_protected};
use crate::channel_writer::read_key_version;
use crate::cmd::channel_integrity::{self, IntegrityReport};

/// Append `payload` as the human to `channel_id`.
pub fn append_as_human(
    svc: &ChannelService,
    mur_home: &Path,
    channel_id: &str,
    kind: EventKind,
    payload: serde_json::Value,
    idempotency_key: Option<String>,
) -> Result<ChannelEvent> {
    let owner = HumanOwner::load(mur_home)?;
    append_with_owner(
        svc,
        mur_home,
        channel_id,
        &owner,
        kind,
        payload,
        idempotency_key,
    )
}

/// [`append_as_human`] with the owner already loaded (a caller that also
/// created the channel with it).
pub fn append_with_owner(
    svc: &ChannelService,
    mur_home: &Path,
    channel_id: &str,
    owner: &HumanOwner,
    kind: EventKind,
    payload: serde_json::Value,
    idempotency_key: Option<String>,
) -> Result<ChannelEvent> {
    let ChannelOwner::Human { signer, router } = owner.owner() else {
        unreachable!("HumanOwner::owner is always Human")
    };
    // The agent whose key sealed a pre-phase-2 head: the router takes it
    // over once, at enrollment. Read before the lock; the enrollment itself
    // re-checks the head under it.
    let sealer = legacy_sealer(mur_home, channel_id)?;
    let gate = EnrollGate {
        identity_protected: identity_protected(mur_home),
        log_intact: &|| log_intact(mur_home, channel_id),
        legacy_sealer: sealer.as_ref().map(|s| LegacySealer {
            identity: &s.identity,
            kv: s.kv,
            key_home: &s.home,
        }),
    };
    svc.append_human(
        channel_id,
        signer,
        router,
        &gate,
        kind,
        payload,
        idempotency_key,
    )
    .with_context(|| format!("write as the human to channel {channel_id}"))
}

/// The keyed `Human` actor for the local human key, for a payload field that
/// names who acted (`repaired_by`). Keyless when no human is enrolled.
pub fn human_actor(mur_home: &Path) -> ChannelActor {
    match mur_common::identity::human::load_pubkey(mur_home) {
        Ok(pk) => {
            ChannelActor::local_human_with_key(mur_common::identity::encode_pubkey_bytes(&pk))
        }
        Err(_) => ChannelActor::local_human(),
    }
}

/// The log is intact for enrollment (N5): no damage of any kind except
/// lines whose signature fails, which alone are not damage (D-7) — every
/// pre-phase-2 human line signed by a session agent is one.
fn log_intact(mur_home: &Path, channel_id: &str) -> Result<bool> {
    Ok(intact_ignoring_signatures(&channel_integrity::check(
        mur_home, channel_id,
    )?))
}

fn intact_ignoring_signatures(report: &IntegrityReport) -> bool {
    IntegrityReport {
        unverified_seqs: Vec::new(),
        ..report.clone()
    }
    .is_intact()
}

struct Sealer {
    identity: AgentIdentity,
    kv: u32,
    home: std::path::PathBuf,
}

/// The agent that sealed the channel's head, when integrity says it was not
/// the router. `None` on a channel with no head or a router-sealed one.
fn legacy_sealer(mur_home: &Path, channel_id: &str) -> Result<Option<Sealer>> {
    let report = match channel_integrity::check(mur_home, channel_id) {
        Ok(r) => r,
        // No log yet (a channel created a moment ago): nothing sealed it.
        Err(_) => return Ok(None),
    };
    let Some(agent) = report.legacy_sealer else {
        return Ok(None);
    };
    let home = mur_home.join("agents").join(&agent);
    let identity = AgentIdentity::load(&home).with_context(|| {
        format!(
            "channel {channel_id}'s head was sealed by agent `{agent}` before the router took \
             over; enrolling the owner reads that agent's key, which this process cannot"
        )
    })?;
    Ok(Some(Sealer {
        kv: read_key_version(&home),
        identity,
        home,
    }))
}

#[cfg(test)]
#[path = "human_writer_tests.rs"]
mod tests;

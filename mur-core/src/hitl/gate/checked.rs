//! The gate's read of its channel log, checked against the chain and the
//! signed head in `channel-heads/` (#1935).
//!
//! `scan_prior` can release a gate without writing anything, so it is the one
//! read where a tampered log turns straight into an authorization: delete a
//! newer deny (and its request) and the older allow is all that is left. The
//! anchored writes after it would notice the hole, but only after the allow
//! was already returned. So the read itself asks the checked loader, and a
//! damaged log yields no prior decisions at all — the gate asks again.
//!
//! What counts as damage here is the STRUCTURAL part of the report: lines
//! that do not parse, hash-chain breaks, a head that does not hold, and a
//! dirty marker. `DamageReport::unverified` is deliberately not used: the
//! loader checks every event against one key, while a gate channel carries
//! events from several actors (agents sign their own), so it would flag
//! honest lines. Authority is already decided per event by
//! `authority::is_router_signed` / `is_human_authority`.
//!
//! Gate B (the agent runtime's chat gate) keeps `<agent_home>/hitl-heads/`
//! as its only anchor and does not read `channel-heads/`, deliberately: its
//! decision channel is written by the agent process itself, inside the
//! sandbox, which can never write `channel-heads/` (that is the point of the
//! directory), so those channels never get a router head to check against.
//! The runtime also must not depend on `mur-core`. Its own self-signed head
//! already makes it ignore remembered decisions when a record is removed
//! (#1859), which is the same behavior this module gives gate A.

use std::path::Path;

use anyhow::Result;
use mur_channel::ChannelStore;
use mur_channel::store::DamageReport;
use mur_common::channel::{ChannelActor, ChannelEvent};

/// Stand-in for the loader's bare-key parameter when there is no signed head:
/// with no head nothing is checked against it, and per-event verdicts are
/// ignored here (see the module doc). Not a key.
const NO_HEAD_INERT_KEY: [u8; 32] = [0u8; 32];

/// The channel's events plus, when the log cannot be trusted, why.
pub(super) struct CheckedLog {
    pub events: Vec<ChannelEvent>,
    /// `Some` = do not derive prior decisions from `events`.
    pub damage: Option<String>,
}

/// Load `channel_id` through `load_events_with_damage`. A head that exists
/// but cannot be read, or cannot be checked because the router key is gone,
/// is damage too: an unverifiable anchor anchors nothing.
pub(super) fn load(mur_home: &Path, channel_id: &str) -> Result<CheckedLog> {
    let store = ChannelStore::new(mur_home);
    let unchecked = |why: String| -> Result<CheckedLog> {
        Ok(CheckedLog {
            events: store.load_events(channel_id)?,
            damage: Some(why),
        })
    };
    let head = match mur_channel::head_io::read_head(
        &mur_channel::anchor::heads_dir(mur_home),
        channel_id,
    ) {
        Ok(h) => h,
        Err(e) => return unchecked(format!("signed head unreadable: {e:#}")),
    };
    let key = match &head {
        None => NO_HEAD_INERT_KEY,
        // The head is router-signed; resolve the key version it claims so a
        // rekey does not read as tampering.
        Some(h) => match crate::channel_verify::actor_pubkey(
            mur_home,
            &ChannelActor::System,
            h.key_version,
        ) {
            Some(k) => k,
            None => {
                return unchecked("router key unreadable; signed head cannot be checked".into());
            }
        },
    };
    let (events, report) = store.load_events_with_damage(channel_id, &key, false)?;
    Ok(CheckedLog {
        events,
        damage: structural_damage(&report),
    })
}

/// The findings that make the log's history untrustworthy, or `None`.
fn structural_damage(r: &DamageReport) -> Option<String> {
    let mut found = Vec::new();
    if !r.unparseable_lines.is_empty() {
        found.push(format!("unparseable lines {:?}", r.unparseable_lines));
    }
    if !r.chain_breaks.is_empty() {
        found.push(format!("hash chain broken at seq {:?}", r.chain_breaks));
    }
    if let Some(m) = &r.anchor_mismatch {
        found.push(format!(
            "signed head does not hold ({:?}: head covers {} lines, log has {})",
            m.kind, m.head.lines, m.log_lines
        ));
    }
    if r.anchor_dirty {
        found.push("signed head is stale (dirty marker)".into());
    }
    (!found.is_empty()).then(|| found.join("; "))
}

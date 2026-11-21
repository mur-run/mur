//! Human-owner trust on a channel (at-mention phase 2, B2).
//!
//! Two layers, kept apart on purpose:
//!
//! - **Provenance** ([`resolve_human_pubkey`], reached through
//!   [`super::actor_pubkey`]): a `Human` actor that names its key is checked
//!   against the local human rotation chain. No channel context.
//! - **Trust** ([`HumanTrust`]): with the channel's log and signed head in
//!   hand, select the live owner record (D-3, D-7), check its `human_cut`
//!   (D-6), and decide whether one `Human` event is trusted.
//!
//! Nothing here writes. Until a channel carries an `owner-key-enrolled`
//! record (PR 2), [`HumanTrust::is_trusted_human`] is today's rule for
//! keyless humans and refuses keyed ones.

use std::path::Path;

use mur_common::channel::{ChannelActor, ChannelEvent, EventKind, OwnerKeyEnrolledPayload};
use mur_common::identity::decode_pubkey;
use mur_common::identity::human::rotation_chain;

use crate::head::Head;

/// The human key `key` names, if it is on the local human chain at exactly
/// `key_version`.
///
/// Strict, unlike [`crate::sign::resolve_writer_pubkey`]: the key comes only
/// from the verified chain in `identity/` (never a disk fallback), and the
/// event's claimed key must be the key that version attests. `key_version`
/// `None` means the chain head. Any mismatch is `None`, which a signed event
/// turns into a verification failure.
pub fn resolve_human_pubkey(
    mur_home: &Path,
    key: &str,
    key_version: Option<u32>,
) -> Option<[u8; 32]> {
    let claimed = decode_pubkey(key).ok()?;
    let chain = rotation_chain(mur_home).ok()?;
    let attested = match key_version {
        None => chain.head_pubkey(),
        Some(kv) if kv == chain.head_key_version() => chain.head_pubkey(),
        Some(kv) => {
            let att = chain.entries().iter().find(|a| a.new_key_version == kv)?;
            decode_pubkey(&att.new_pubkey).ok()?
        }
    };
    (attested == claimed).then_some(attested)
}

/// Owner-state damage the fold and the integrity report must surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerDamage {
    /// The live record is not covered by the signed head: no head, or
    /// `record.seq > head.seq` (D-3).
    Unanchored,
    /// `human_cut.seq` lies past the signed head (D-6).
    CutBeyondHead,
    /// The line at `human_cut.seq` is missing or does not hash to
    /// `human_cut.line_hash` (D-6).
    CutLineHashMismatch,
}

/// The selected owner record.
#[derive(Debug, Clone)]
pub struct LiveRecord {
    pub seq: u64,
    pub payload: OwnerKeyEnrolledPayload,
}

/// A channel's human-owner trust state, computed from one load.
#[derive(Debug, Clone, Default)]
pub struct HumanTrust {
    live: Option<LiveRecord>,
    /// `seq` of every record that did not qualify (foreign key, retired key,
    /// bad signature). Reported, never damage (D-7).
    ignored: Vec<u64>,
    /// `seq` of qualifying records after the live one. Ignored with a warning.
    later: Vec<u64>,
    /// `seq` of records that verify on the local chain but are not live: a
    /// retired key's record that no head covers yet. Anchoring anything
    /// after one would make it live (and first), so C1 refuses (see
    /// [`HumanTrust::dormant_records`]).
    dormant: Vec<u64>,
    damage: Vec<OwnerDamage>,
}

impl HumanTrust {
    /// Build trust state from a channel's raw log and its signed head (as
    /// already verified by the caller's anchor check).
    ///
    /// Takes the raw log, not parsed events, because `human_cut.line_hash`
    /// hashes the raw line (D-6). Lines that do not parse are skipped here;
    /// the store's damage report already counts them.
    pub fn from_log(mur_home: &Path, channel_id: &str, log: &str, head: Option<&Head>) -> Self {
        let lines: Vec<(&str, ChannelEvent)> = log
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str(l).ok().map(|ev| (l, ev)))
            .collect();
        let mut trust = HumanTrust::default();
        for (_, ev) in lines
            .iter()
            .filter(|(_, e)| e.kind == EventKind::OwnerKeyEnrolled)
        {
            match qualifying_record(mur_home, channel_id, ev, head) {
                Some(payload) if trust.live.is_none() => {
                    trust.live = Some(LiveRecord {
                        seq: ev.seq,
                        payload,
                    });
                }
                Some(_) => {
                    tracing::warn!(
                        channel_id,
                        seq = ev.seq,
                        "a later owner-key-enrolled record is ignored; the first valid one is live"
                    );
                    trust.later.push(ev.seq);
                }
                None => {
                    tracing::warn!(
                        channel_id,
                        seq = ev.seq,
                        "SIGNATURE_INVALID: owner-key-enrolled record does not verify under the \
                         local human chain — ignored, grants nothing"
                    );
                    trust.ignored.push(ev.seq);
                    if record_on_chain(mur_home, channel_id, ev).is_some() {
                        trust.dormant.push(ev.seq);
                    }
                }
            }
        }
        if let Some(live) = &trust.live {
            trust.damage = owner_damage(live, head, &lines);
        }
        trust
    }

    pub fn live_record(&self) -> Option<&LiveRecord> {
        self.live.as_ref()
    }

    /// Ignored records that nonetheless verify under the local human chain
    /// (a retired key's, not yet covered by a head). A head sealed over one
    /// would turn it live, so enrolling must not proceed past it.
    pub fn dormant_records(&self) -> &[u64] {
        &self.dormant
    }

    pub fn ignored_records(&self) -> &[u64] {
        &self.ignored
    }

    pub fn later_records(&self) -> &[u64] {
        &self.later
    }

    pub fn damage(&self) -> &[OwnerDamage] {
        &self.damage
    }

    /// Whether `ev`, a `Human` event, is trusted on this channel. `false` for
    /// any other actor — this answers the human question only.
    ///
    /// - live record, keyed: provenance verifies (signature required).
    /// - live record, keyless: `seq <= human_cut.seq` and router-signed.
    ///   `require_sig` does not relax this: past the cut a keyless human line
    ///   is exactly what an agent could write.
    /// - no live record, keyed: fail (spec §2.3). Verifying on the local chain
    ///   is provenance, not enrollment on this channel.
    /// - no live record, keyless: today's rule, [`super::verify_event`].
    pub fn is_trusted_human(
        &self,
        mur_home: &Path,
        channel_id: &str,
        ev: &ChannelEvent,
        require_sig: bool,
    ) -> bool {
        let ChannelActor::Human { key, .. } = &ev.actor else {
            return false;
        };
        let Some(live) = &self.live else {
            return key.is_none() && super::verify_event(mur_home, channel_id, ev, require_sig);
        };
        match key {
            Some(_) => super::verify_event(mur_home, channel_id, ev, true),
            None => {
                live.payload
                    .human_cut
                    .as_ref()
                    .is_some_and(|cut| ev.seq <= cut.seq)
                    && ev.sig.is_some()
                    && super::control_plane_pubkey(mur_home, ev.key_version)
                        .is_some_and(|pk| crate::sign::verify_one(channel_id, ev, &pk, true))
            }
        }
    }
}

/// The record's payload if `ev` qualifies as a live-record candidate: a
/// keyed `Human` actor naming the same key as the payload, signed, verifying
/// strictly on the local chain.
///
/// Rev 4: a new candidate must verify under the chain **head** key. A record
/// signed by an older chain key still qualifies when the signed head already
/// covers it — it was selected before the rekey and is anchored, so it stays
/// live. An unanchored record under a retired key is ignored.
fn qualifying_record(
    mur_home: &Path,
    channel_id: &str,
    ev: &ChannelEvent,
    head: Option<&Head>,
) -> Option<OwnerKeyEnrolledPayload> {
    let payload = record_on_chain(mur_home, channel_id, ev)?;
    let head_kv = rotation_chain(mur_home).ok()?.head_key_version();
    let anchored = head.is_some_and(|h| ev.seq <= h.seq);
    (payload.key_version == head_kv || anchored).then_some(payload)
}

/// An `owner-key-enrolled` record that is well-formed and verifies under the
/// local human chain at its own key version — before asking whether it is
/// the head key or anchored.
fn record_on_chain(
    mur_home: &Path,
    channel_id: &str,
    ev: &ChannelEvent,
) -> Option<OwnerKeyEnrolledPayload> {
    let ChannelActor::Human { key: Some(key), .. } = &ev.actor else {
        return None;
    };
    let payload: OwnerKeyEnrolledPayload = serde_json::from_value(ev.payload.clone()).ok()?;
    if &payload.owner_key != key || ev.key_version != Some(payload.key_version) {
        return None;
    }
    let pk = resolve_human_pubkey(mur_home, key, Some(payload.key_version))?;
    crate::sign::verify_one(channel_id, ev, &pk, true).then_some(payload)
}

fn owner_damage(
    live: &LiveRecord,
    head: Option<&Head>,
    lines: &[(&str, ChannelEvent)],
) -> Vec<OwnerDamage> {
    let mut damage = Vec::new();
    if head.is_none_or(|h| live.seq > h.seq) {
        damage.push(OwnerDamage::Unanchored);
    }
    if let Some(cut) = &live.payload.human_cut {
        if head.is_none_or(|h| cut.seq > h.seq) {
            damage.push(OwnerDamage::CutBeyondHead);
        }
        let hashes = lines
            .iter()
            .find(|(_, e)| e.seq == cut.seq)
            .is_some_and(|(raw, _)| crate::store::line_hash(raw) == cut.line_hash);
        if !hashes {
            damage.push(OwnerDamage::CutLineHashMismatch);
        }
    }
    damage
}

#[cfg(test)]
#[path = "human_tests.rs"]
mod tests;

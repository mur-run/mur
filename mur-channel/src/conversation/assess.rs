//! The locked read of a channel conversation (§3.4, §7 order under the
//! lock): `REPAIRING` → regression → damage table → watermark → fold. One
//! log read — owner trust and every head check reuse its bytes — all inside
//! the caller's shared lock.
use anyhow::Result;
use mur_common::channel::{ChannelActor, ChannelEvent, EventKind};

use super::{Assessment, CacheState, ConversationDamage as D, Keys, Verdict};
use crate::store::{ChannelStore, DamageReport};
use crate::verify::human::{HumanTrust, OwnerDamage};

/// Run inside `store.with_shared_lock(channel_id, |s| assess_locked(s, ...))`.
pub fn assess_locked(
    store: &ChannelStore,
    channel_id: &str,
    keys: &Keys<'_>,
    cache: &CacheState,
    channel_seq: u64,
    turn_input: &str,
) -> Result<Verdict> {
    // require_sig = false: every pre-identity channel is unsigned; the HITL
    // gate makes the same choice (log_integrity.rs). ONE read of the log:
    // the owner state and every head check below use these same bytes.
    let (content, events, mut report) =
        store.load_raw_with_damage(channel_id, &keys.writer_pubkey, false)?;
    let head = crate::head_io::read_head(&crate::anchor::heads_dir(keys.mur_home), channel_id)?;
    let trust = HumanTrust::from_log(keys.mur_home, channel_id, &content, head.as_ref());
    let owned = trust.live_record().is_some();
    // H1: router first; the writer key (and its rotated keys) only while the
    // channel has no live owner record.
    report.anchor_mismatch = super::head::verdict(
        store,
        channel_id,
        keys,
        &content,
        head.as_ref(),
        report.anchor_mismatch.take(),
        owned,
    )?;
    // Lines signed before a rekey: vouched for by the same history.
    report
        .unverified
        .retain(|ev| !verifies_under_rotated_key(channel_id, keys, ev));
    report.unverified_newer_kind.retain(|ev| {
        !rotated_key(keys, ev.key_version)
            .is_some_and(|pk| crate::sign::verify_newer_kind(channel_id, ev, &pk, true))
    });
    // C4: on an owned channel a `Human` line is judged by owner trust, never
    // by the writer key; an untrusted claim is projected marked, not damage.
    // A record that did not qualify is ignored, never damage (D-7).
    let ignored = trust.ignored_records();
    report.unverified.retain(|ev| {
        !(owned && matches!(ev.actor, ChannelActor::Human { .. }))
            && !(ev.kind == EventKind::OwnerKeyEnrolled && ignored.contains(&ev.seq))
    });
    report
        .unverified_newer_kind
        .retain(|ev| !(owned && matches!(ev.actor, ChannelActor::Human { .. })));

    // §7: REPAIRING before anything else under the lock.
    if report.repair_pending {
        return Ok(Verdict::Repairing);
    }
    let log_last_seq = events.last().map(|e| e.seq);

    // The seq 0 repair record is verified alone under ITS actor's key and,
    // when it verifies, excluded from `unverified` before the table: under
    // this agent's writer key a router-signed record always fails.
    let repaired = events
        .first()
        .is_some_and(|first| first.seq == 0 && verified_repair_record(channel_id, keys, first));
    if repaired {
        report.unverified.retain(|e| e.seq != 0);
    }

    // Regression check BEFORE the damage table.
    let mut rebuilt_after_repair = None;
    if let Some(wm) = cache.watermark {
        match log_last_seq {
            Some(last) if wm <= last => {}
            Some(_) if repaired => rebuilt_after_repair = Some(wm),
            _ => return Ok(Verdict::Damaged(D::LogRegressed)),
        }
    }

    if let Some(d) = damage_table(report.clone()) {
        return Ok(Verdict::Damaged(d));
    }
    // Owner state before any event is classified: a record no head covers
    // proves nothing about the lines it would vouch for (D-3).
    if let Some(d) = owner_damage(&trust) {
        return Ok(Verdict::Damaged(d));
    }

    if let Some(d) = watermark(channel_id, keys, &events, channel_seq, turn_input) {
        return Ok(Verdict::Damaged(d));
    }
    // The user took this turn's message back (a stale replay raced the
    // retract): the retract lies after `channel_seq`, so scan the whole log.
    if super::retract::scan(&events, keys.agent, u64::MAX)
        .targets
        .contains(&channel_seq)
    {
        return Ok(Verdict::Damaged(D::Watermark {
            detail: format!("seq {channel_seq} was retracted"),
        }));
    }

    let retracts = super::retract::scan(&events, keys.agent, channel_seq);
    let mut projected = super::project::project(&events, keys.agent, channel_seq);
    mark_untrusted(channel_id, keys, &trust, &events, &mut projected);
    // `watermark` above proved the event exists.
    let chain = mur_common::identity::human::rotation_chain(keys.mur_home).ok();
    let turn_originator = events
        .iter()
        .find(|e| e.seq == channel_seq)
        .map(|ev| {
            super::originator::classify(keys.mur_home, channel_id, &trust, chain.as_ref(), ev)
        })
        .unwrap_or(super::originator::Originator::System);
    Ok(Verdict::Ok(Assessment {
        turn_originator,
        projected,
        retracted: retracts.targets.into_iter().collect(),
        ignored_retracts: retracts.ignored,
        log_last_seq: log_last_seq.unwrap_or(0),
        rebuilt_after_repair,
        anchor_dirty: report.anchor_dirty,
    }))
}

/// The per-field damage policy, in field order; `anchor_dirty` is allowed.
fn damage_table(report: DamageReport) -> Option<D> {
    if !report.unparseable_lines.is_empty() {
        return Some(D::Unparseable {
            lines: report.unparseable_lines,
        });
    }
    if !report.unverified.is_empty() || !report.unverified_newer_kind.is_empty() {
        return Some(D::Signature);
    }
    if !report.chain_breaks.is_empty() {
        return Some(D::Chain);
    }
    if report.anchor_mismatch.is_some() {
        return Some(D::Anchor);
    }
    None
}

/// `owner_unanchored` (D-3) is its own reason; a cut that does not hold
/// means a line inside the anchored prefix changed, which is what `anchor`
/// already names.
fn owner_damage(trust: &HumanTrust) -> Option<D> {
    let damage = trust.damage();
    if damage.contains(&OwnerDamage::Unanchored) {
        Some(D::OwnerUnanchored)
    } else if !damage.is_empty() {
        Some(D::Anchor)
    } else {
        None
    }
}

/// Flag every projected `Human` turn the owner trust does not vouch for.
/// `require_sig = true`: an unsigned human line is history, not a claim
/// anyone stands behind.
fn mark_untrusted(
    channel_id: &str,
    keys: &Keys<'_>,
    trust: &HumanTrust,
    events: &[ChannelEvent],
    projected: &mut [super::ProjectedMessage],
) {
    for p in projected.iter_mut() {
        let Some(ev) = events.iter().find(|e| e.seq == p.seq) else {
            continue;
        };
        p.untrusted = matches!(ev.actor, ChannelActor::Human { .. })
            && !trust.is_trusted_human(keys.mur_home, channel_id, ev, true);
    }
}

/// Murmur's own event at `channel_seq` must exist and be this turn's input.
fn watermark(
    channel_id: &str,
    keys: &Keys<'_>,
    events: &[ChannelEvent],
    channel_seq: u64,
    turn_input: &str,
) -> Option<D> {
    let Some(ev) = events.iter().find(|e| e.seq == channel_seq) else {
        let end = events.last().map_or_else(
            || "the log is empty".to_string(),
            |e| format!("the log ends at seq {}", e.seq),
        );
        return Some(D::Watermark {
            detail: format!("no event at seq {channel_seq} ({end})"),
        });
    };
    let text = ev
        .payload
        .get("text")
        .and_then(|t| t.as_str())
        .unwrap_or_default();
    match (&ev.actor, ev.kind) {
        (ChannelActor::Human { .. }, EventKind::Message) => {
            (text != super::normalize_user_text(turn_input)).then(|| D::Watermark {
                detail: format!("seq {channel_seq} text differs from the turn input"),
            })
        }
        // S1: a shell-started turn's card, signed by this agent's writer key;
        // no text comparison (the input block is derived from the card).
        (ChannelActor::System, EventKind::Note)
            if !crate::repair_record::is_repair_record(ev)
                && crate::sign::verify_one(channel_id, ev, &keys.writer_pubkey, true) =>
        {
            None
        }
        _ => Some(D::Watermark {
            detail: format!("seq {channel_seq} is not this turn's user event"),
        }),
    }
}

/// The seq 0 record is a repair record, its actor is `System` or
/// `Agent{self}`, and its signature verifies on its own under THAT actor's
/// key with require_sig = true.
fn verified_repair_record(channel_id: &str, keys: &Keys<'_>, ev: &ChannelEvent) -> bool {
    if !crate::repair_record::is_repair_record(ev) {
        return false;
    }
    let own = matches!(&ev.actor, ChannelActor::Agent { id } if id == keys.agent);
    if !own && !matches!(ev.actor, ChannelActor::System) {
        return false;
    }
    // key_of(System) = router key; key_of(Agent{self}) = this agent's key at
    // the record's key_version. `actor_pubkey` does exactly that lookup.
    match crate::verify::actor_pubkey(keys.mur_home, &ev.actor, ev.key_version) {
        Some(pk) => crate::sign::verify_one(channel_id, ev, &pk, true),
        None => false,
    }
}

/// This agent's key for `key_version` per its VERIFIED rotation history,
/// when that is not the key already tried. Strict: no fallback to the
/// current key, so a version the history does not vouch for resolves nothing.
pub(super) fn rotated_key(keys: &Keys<'_>, key_version: Option<u32>) -> Option<[u8; 32]> {
    let dir = crate::verify::actor_key_dir(
        keys.mur_home,
        &ChannelActor::Agent {
            id: keys.agent.to_string(),
        },
    )?;
    let key = crate::sign::resolve_writer_pubkey_strict(&dir, key_version?)?;
    (key != keys.writer_pubkey).then_some(key)
}

/// A line that failed under the current writer key but verifies under the
/// key this agent's rotation history gives its `key_version`.
fn verifies_under_rotated_key(channel_id: &str, keys: &Keys<'_>, ev: &ChannelEvent) -> bool {
    rotated_key(keys, ev.key_version)
        .is_some_and(|pk| crate::sign::verify_one(channel_id, ev, &pk, true))
}

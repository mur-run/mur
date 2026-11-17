//! Which `Retract` events a history reader honours (retract spec §4.1).
//!
//! A retract withdraws one earlier `Human` `Message` from conversation
//! history. It is **valid** when the target exists, is a `Human` `Message`
//! by the same actor as the retract, lies before the retract, and nothing
//! answered it: no `Agent{self}` `Message` and no non-repair `System` `Note`
//! between the two. Anything else is ignored, never damage.
//!
//! A valid retract the runtime **refused** (the cancelled turn had already
//! run tools, retract spec §4.3) is void: `Agent{self}` records the refusal
//! as a `Note` carrying [`RetractRefusal`] after the retract, and every
//! history reader then keeps the target, as the model does. Without that
//! record the UI would hide a message the model still receives.
use std::collections::{BTreeMap, BTreeSet};

use mur_common::channel::{ChannelActor, ChannelEvent, EventKind, RetractPayload, RetractRefusal};

/// The retracts among `events` with `seq < before`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RetractScan {
    /// Seqs of validly withdrawn messages.
    pub targets: BTreeSet<u64>,
    /// Seqs of `Retract` events that withdraw nothing, for audit.
    pub ignored: Vec<u64>,
    /// Targets of valid retracts the runtime refused (a refusal note below
    /// `before`); kept in history, not in `targets` or `ignored`.
    pub refused: BTreeSet<u64>,
}

/// Scan `events` (in seq order) for retracts below `before`.
pub fn scan(events: &[ChannelEvent], agent: &str, before: u64) -> RetractScan {
    let by_seq: BTreeMap<u64, &ChannelEvent> = events.iter().map(|e| (e.seq, e)).collect();
    let mut out = RetractScan::default();
    let mut first_retract: BTreeMap<u64, u64> = BTreeMap::new();
    for r in events
        .iter()
        .filter(|e| e.seq < before && e.kind == EventKind::Retract)
    {
        match valid_target(&by_seq, agent, r) {
            Some(t) => {
                first_retract.entry(t).or_insert(r.seq);
            }
            None => out.ignored.push(r.seq),
        }
    }
    // Once refused, every retract of that target is void: a later duplicate
    // must not hide what the model was told it keeps.
    for (t, first) in first_retract {
        if refused_after(events, agent, first, t, before) {
            out.refused.insert(t);
        } else {
            out.targets.insert(t);
        }
    }
    // A duplicate of an honoured (or refused) retract withdraws nothing new,
    // but it is not an invalid one either.
    out.ignored.retain(|seq| {
        !by_seq
            .get(seq)
            .and_then(|r| target_seq(r))
            .is_some_and(|t| out.targets.contains(&t) || out.refused.contains(&t))
    });
    out
}

/// `Agent{agent}` recorded refusing the retract of `target` somewhere after
/// its first valid retract (`retract_seq`) and below `before`.
fn refused_after(
    events: &[ChannelEvent],
    agent: &str,
    retract_seq: u64,
    target: u64,
    before: u64,
) -> bool {
    events.iter().any(|e| {
        e.seq > retract_seq
            && e.seq < before
            && e.kind == EventKind::Note
            && matches!(&e.actor, ChannelActor::Agent { id } if id == agent)
            && RetractRefusal::from_payload(&e.payload).is_some_and(|r| r.target_seq == target)
    })
}

fn target_seq(r: &ChannelEvent) -> Option<u64> {
    serde_json::from_value::<RetractPayload>(r.payload.clone())
        .ok()
        .map(|p| p.target_seq)
}

fn valid_target(
    by_seq: &BTreeMap<u64, &ChannelEvent>,
    agent: &str,
    r: &ChannelEvent,
) -> Option<u64> {
    let t = target_seq(r)?;
    if t >= r.seq {
        return None;
    }
    let target = by_seq.get(&t)?;
    if target.kind != EventKind::Message
        || !matches!(target.actor, ChannelActor::Human { .. })
        || target.actor != r.actor
    {
        return None;
    }
    let answered = by_seq.range(t + 1..r.seq).any(|(_, e)| answers(e, agent));
    (!answered).then_some(t)
}

/// An event that turns a user message into an answered one.
fn answers(e: &ChannelEvent, agent: &str) -> bool {
    match (&e.actor, e.kind) {
        (ChannelActor::Agent { id }, EventKind::Message) => id == agent,
        (ChannelActor::System, EventKind::Note) => !crate::repair_record::is_repair_record(e),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use mur_common::channel::{RetractReason, retract_idempotency_key};

    use super::*;

    const AGENT: &str = "mur";

    fn ev(
        seq: u64,
        actor: ChannelActor,
        kind: EventKind,
        payload: serde_json::Value,
    ) -> ChannelEvent {
        ChannelEvent {
            seq,
            ts: Utc::now(),
            actor,
            kind,
            payload,
            idempotency_key: None,
            sig: None,
            key_version: None,
            prev_hash: None,
        }
    }

    fn human(seq: u64) -> ChannelEvent {
        ev(
            seq,
            ChannelActor::local_human(),
            EventKind::Message,
            serde_json::json!({ "text": "foo" }),
        )
    }

    fn agent_msg(seq: u64) -> ChannelEvent {
        let a = ChannelActor::Agent { id: AGENT.into() };
        ev(
            seq,
            a,
            EventKind::Message,
            serde_json::json!({ "text": "hi" }),
        )
    }

    fn retract_by(seq: u64, actor: ChannelActor, target: u64) -> ChannelEvent {
        let p = RetractPayload {
            target_seq: target,
            reason: RetractReason::Cancelled,
        };
        let mut e = ev(
            seq,
            actor,
            EventKind::Retract,
            serde_json::to_value(p).unwrap(),
        );
        e.idempotency_key = Some(retract_idempotency_key(target));
        e
    }

    fn retract(seq: u64, target: u64) -> ChannelEvent {
        retract_by(seq, ChannelActor::local_human(), target)
    }

    fn targets(s: &RetractScan) -> Vec<u64> {
        s.targets.iter().copied().collect()
    }

    #[test]
    fn unanswered_own_message_is_withdrawn() {
        let evs = [human(0), agent_msg(1), human(2), retract(3, 2)];
        let s = scan(&evs, AGENT, u64::MAX);
        assert_eq!(targets(&s), vec![2]);
        assert!(s.ignored.is_empty());
    }

    #[test]
    fn retract_at_or_past_before_is_not_seen() {
        let evs = [human(0), retract(1, 0)];
        assert_eq!(scan(&evs, AGENT, 1), RetractScan::default());
    }

    #[test]
    fn answered_message_is_not_withdrawn() {
        let evs = [human(0), agent_msg(1), retract(2, 0)];
        let s = scan(&evs, AGENT, u64::MAX);
        assert!(s.targets.is_empty());
        assert_eq!(s.ignored, vec![2]);
    }

    #[test]
    fn shell_note_counts_as_an_answer() {
        let note = ev(
            1,
            ChannelActor::System,
            EventKind::Note,
            serde_json::json!({ "text": "$ ls" }),
        );
        let evs = [human(0), note, retract(2, 0)];
        assert_eq!(scan(&evs, AGENT, u64::MAX).ignored, vec![2]);
    }

    #[test]
    fn another_agents_message_is_not_an_answer() {
        let other = ev(
            1,
            ChannelActor::Agent { id: "qa".into() },
            EventKind::Message,
            serde_json::json!({ "text": "x" }),
        );
        let evs = [human(0), other, retract(2, 0)];
        assert_eq!(targets(&scan(&evs, AGENT, u64::MAX)), vec![0]);
    }

    #[test]
    fn retract_by_another_actor_is_ignored() {
        let other = ChannelActor::Human {
            name: "eve".into(),
            key: None,
        };
        let evs = [human(0), retract_by(1, other, 0)];
        let s = scan(&evs, AGENT, u64::MAX);
        assert!(s.targets.is_empty());
        assert_eq!(s.ignored, vec![1]);
    }

    #[test]
    fn retract_of_an_agent_message_is_ignored() {
        let a = ChannelActor::Agent { id: AGENT.into() };
        let evs = [agent_msg(0), retract_by(1, a, 0)];
        assert_eq!(scan(&evs, AGENT, u64::MAX).ignored, vec![1]);
    }

    #[test]
    fn forward_or_missing_target_is_ignored() {
        let evs = [human(0), retract(1, 5), retract(2, 2), human(5)];
        let s = scan(&evs, AGENT, u64::MAX);
        assert!(s.targets.is_empty());
        assert_eq!(s.ignored, vec![1, 2]);
    }

    #[test]
    fn duplicate_retract_is_idempotent() {
        let evs = [human(0), retract(1, 0), retract(2, 0)];
        let s = scan(&evs, AGENT, u64::MAX);
        assert_eq!(targets(&s), vec![0]);
        assert!(s.ignored.is_empty());
    }

    fn refusal(seq: u64, by: &str, target: u64) -> ChannelEvent {
        ev(
            seq,
            ChannelActor::Agent { id: by.into() },
            EventKind::Note,
            RetractRefusal { target_seq: target }.to_payload(),
        )
    }

    #[test]
    fn a_retract_the_runtime_refused_withdraws_nothing() {
        let evs = [human(0), retract(1, 0), refusal(2, AGENT, 0), retract(3, 0)];
        let s = scan(&evs, AGENT, u64::MAX);
        assert!(s.targets.is_empty(), "{s:?}");
        assert_eq!(s.refused.iter().copied().collect::<Vec<_>>(), vec![0]);
        assert!(s.ignored.is_empty(), "a duplicate is not invalid: {s:?}");
    }

    #[test]
    fn a_refusal_at_or_past_before_is_not_seen() {
        let evs = [human(0), retract(1, 0), refusal(2, AGENT, 0)];
        assert_eq!(targets(&scan(&evs, AGENT, 2)), vec![0]);
    }

    #[test]
    fn a_refusal_by_another_agent_or_before_the_retract_is_ignored() {
        let evs = [
            human(0),
            refusal(1, AGENT, 0),
            retract(2, 0),
            refusal(3, "qa", 0),
        ];
        assert_eq!(targets(&scan(&evs, AGENT, u64::MAX)), vec![0]);
    }

    #[test]
    fn unreadable_payload_is_ignored() {
        let bad = ev(
            1,
            ChannelActor::local_human(),
            EventKind::Retract,
            serde_json::json!({}),
        );
        assert_eq!(scan(&[human(0), bad], AGENT, u64::MAX).ignored, vec![1]);
    }
}

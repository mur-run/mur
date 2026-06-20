//! Replay-resistant fold of commander governance directives from a channel log.
//!
//! Trust = signature verifies against an accepted pubkey AND the directive is
//! ordered by its SIGNED `issued_at_ms` (NOT the store-assigned `seq`). A
//! replayed old directive keeps its old timestamp and cannot supersede a newer
//! one; verbatim replays are dropped by nonce-dedup. See the design spec §4.

use std::collections::HashSet;

use mur_common::channel::ChannelEvent;
use mur_common::commander::{COMMANDER_DIRECTIVE_KEY, CommanderDirective, GovernanceState};

use crate::sign::verify_event_sig;

/// Fold the channel's commander directives into governance state.
pub fn fold_governance(
    events: &[ChannelEvent],
    channel_id: &str,
    fleet_name: &str,
    accepted_pubkeys: &[[u8; 32]],
) -> GovernanceState {
    // 1. Candidate filter: parse the marker, match the fleet, verify the sig
    //    against SOME accepted pubkey.
    let mut candidates: Vec<CommanderDirective> = Vec::new();
    for e in events {
        let Some(raw) = e.payload.get(COMMANDER_DIRECTIVE_KEY) else {
            continue;
        };
        let Ok(d) = serde_json::from_value::<CommanderDirective>(raw.clone()) else {
            continue;
        };
        if d.fleet != fleet_name {
            continue;
        }
        let Some(sig) = e.sig.as_deref() else {
            continue;
        };
        let ok = accepted_pubkeys.iter().any(|pk| {
            verify_event_sig(
                channel_id,
                &e.actor,
                e.kind,
                &e.payload,
                e.idempotency_key.as_deref(),
                sig,
                pk,
            )
        });
        if ok {
            candidates.push(d);
        }
    }

    // 2. Nonce-dedup (drop verbatim replays; keep first occurrence).
    let mut seen: HashSet<String> = HashSet::new();
    candidates.retain(|d| seen.insert(d.nonce.clone()));

    // 3. Order by the SIGNED issued_at_ms (then nonce as a deterministic tiebreak).
    candidates.sort_by(|a, b| {
        a.issued_at_ms
            .cmp(&b.issued_at_ms)
            .then_with(|| a.nonce.cmp(&b.nonce))
    });

    // 4. Last-wins per kind in that order.
    let mut state = GovernanceState::default();
    for d in &candidates {
        match d.kind.as_str() {
            "kill" => state.killed = true,
            "resume" => state.killed = false,
            "budget_ceiling" => {
                if let Some(v) = d.budget_usd
                    && v.is_finite()
                    && v >= 0.0
                {
                    state.budget_ceiling = Some(v);
                }
            }
            _ => {}
        }
    }
    state
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::channel::{ChannelActor, ChannelEvent, EventKind};
    use mur_common::identity::AgentIdentity;

    const CID: &str = "fleet-dev";

    // Build a signed directive event (mirrors what append_signed produces).
    fn directive_event(
        id: &AgentIdentity,
        seq: u64,
        kind: &str,
        fleet: &str,
        budget: Option<f64>,
        nonce: &str,
        issued_at_ms: u64,
    ) -> ChannelEvent {
        let payload = serde_json::json!({ COMMANDER_DIRECTIVE_KEY: {
            "kind": kind, "fleet": fleet, "budget_usd": budget,
            "nonce": nonce, "issued_at_ms": issued_at_ms,
        }});
        let actor = ChannelActor::System;
        let sig = crate::sign::sign_event(id, CID, &actor, EventKind::Note, &payload, Some(nonce));
        ChannelEvent {
            seq,
            ts: chrono::Utc::now(),
            actor,
            kind: EventKind::Note,
            payload,
            idempotency_key: Some(nonce.to_string()),
            sig: Some(sig),
            key_version: None,
        }
    }

    #[test]
    fn kill_then_newer_resume_then_replayed_old_resume_stays_killed() {
        let cmd = AgentIdentity::generate();
        let pk = [cmd.verifying_key_bytes()];
        let resume_old = directive_event(&cmd, 1, "resume", "dev", None, "r1", 1000);
        let kill = directive_event(&cmd, 2, "kill", "dev", None, "k1", 2000);
        // attacker re-appends the OLD resume as a new, higher-seq row (verbatim):
        let mut resume_replay = resume_old.clone();
        resume_replay.seq = 3;
        let evs = vec![resume_old, kill, resume_replay];
        // Despite the replay being last by seq, issued_at order + nonce-dedup keep the kill.
        assert!(fold_governance(&evs, CID, "dev", &pk).killed);
    }

    #[test]
    fn newer_resume_clears_a_kill() {
        let cmd = AgentIdentity::generate();
        let pk = [cmd.verifying_key_bytes()];
        let evs = vec![
            directive_event(&cmd, 1, "kill", "dev", None, "k1", 1000),
            directive_event(&cmd, 2, "resume", "dev", None, "r1", 2000),
        ];
        assert!(!fold_governance(&evs, CID, "dev", &pk).killed);
    }

    #[test]
    fn wrong_key_directive_is_ignored() {
        let cmd = AgentIdentity::generate();
        let attacker = AgentIdentity::generate();
        let pk = [cmd.verifying_key_bytes()];
        // kill signed by attacker's key → not a candidate → not killed.
        let evs = vec![directive_event(
            &attacker, 1, "kill", "dev", None, "k1", 1000,
        )];
        assert!(!fold_governance(&evs, CID, "dev", &pk).killed);
    }

    #[test]
    fn wrong_fleet_directive_is_ignored() {
        let cmd = AgentIdentity::generate();
        let pk = [cmd.verifying_key_bytes()];
        let evs = vec![directive_event(&cmd, 1, "kill", "other", None, "k1", 1000)];
        assert!(!fold_governance(&evs, CID, "dev", &pk).killed);
    }

    #[test]
    fn budget_ceiling_applied_zero_honored_negative_ignored() {
        let cmd = AgentIdentity::generate();
        let pk = [cmd.verifying_key_bytes()];
        assert_eq!(
            fold_governance(
                &[directive_event(
                    &cmd,
                    1,
                    "budget_ceiling",
                    "dev",
                    Some(5.0),
                    "b1",
                    1000
                )],
                CID,
                "dev",
                &pk
            )
            .budget_ceiling,
            Some(5.0)
        );
        assert_eq!(
            fold_governance(
                &[directive_event(
                    &cmd,
                    1,
                    "budget_ceiling",
                    "dev",
                    Some(0.0),
                    "b1",
                    1000
                )],
                CID,
                "dev",
                &pk
            )
            .budget_ceiling,
            Some(0.0)
        );
        assert_eq!(
            fold_governance(
                &[directive_event(
                    &cmd,
                    1,
                    "budget_ceiling",
                    "dev",
                    Some(-1.0),
                    "b1",
                    1000
                )],
                CID,
                "dev",
                &pk
            )
            .budget_ceiling,
            None
        );
    }

    #[test]
    fn previous_key_still_verifies() {
        let prev = AgentIdentity::generate();
        let cur = AgentIdentity::generate();
        let accepted = [cur.verifying_key_bytes(), prev.verifying_key_bytes()];
        let evs = vec![directive_event(&prev, 1, "kill", "dev", None, "k1", 1000)];
        assert!(fold_governance(&evs, CID, "dev", &accepted).killed);
    }
}

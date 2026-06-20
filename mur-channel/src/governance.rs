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

    // 3. Order by the SIGNED issued_at_ms. On an EQUAL timestamp, bias toward the
    //    safe state — a kill outranks a resume (kind_rank) — then nonce as a final
    //    deterministic tiebreak. This is fail-safe: a same-ms kill/resume pair can
    //    never resolve to "resumed".
    fn kind_rank(kind: &str) -> u8 {
        match kind {
            "resume" => 0,
            "kill" => 2,
            _ => 1,
        }
    }
    candidates.sort_by(|a, b| {
        a.issued_at_ms
            .cmp(&b.issued_at_ms)
            .then_with(|| kind_rank(&a.kind).cmp(&kind_rank(&b.kind)))
            .then_with(|| a.nonce.cmp(&b.nonce))
    });

    // 4. Last-wins per kind in that order. Bind each decision to the nonce of the
    //    directive that produced it, so the audit can attest exactly which signed
    //    directive was honored.
    let mut state = GovernanceState::default();
    for d in &candidates {
        match d.kind.as_str() {
            "kill" => {
                state.killed = true;
                state.kill_nonce = Some(d.nonce.clone());
            }
            "resume" => {
                state.killed = false;
                state.kill_nonce = None;
            }
            "budget_ceiling" => {
                if let Some(v) = d.budget_usd
                    && v.is_finite()
                    && v >= 0.0
                {
                    state.budget_ceiling = Some(v);
                    state.budget_nonce = Some(d.nonce.clone());
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

    #[test]
    fn ordering_is_by_issued_at_not_log_position() {
        let cmd = AgentIdentity::generate();
        let pk = [cmd.verifying_key_bytes()];
        // kill is NEWER (issued 2000) but appears FIRST in the log; resume is
        // OLDER (1000) but LAST. Distinct nonces ⇒ dedup keeps both. Only the
        // issued_at sort keeps the kill; delete the sort and this flips to false.
        let evs = vec![
            directive_event(&cmd, 5, "kill", "dev", None, "k1", 2000),
            directive_event(&cmd, 6, "resume", "dev", None, "r1", 1000),
        ];
        assert!(fold_governance(&evs, CID, "dev", &pk).killed);
    }

    #[test]
    fn equal_timestamp_kill_beats_resume() {
        let cmd = AgentIdentity::generate();
        let pk = [cmd.verifying_key_bytes()];
        // Same issued_at: the safe state (kill) must win regardless of log order.
        let evs = vec![
            directive_event(&cmd, 1, "resume", "dev", None, "r1", 5000),
            directive_event(&cmd, 2, "kill", "dev", None, "k1", 5000),
        ];
        assert!(fold_governance(&evs, CID, "dev", &pk).killed);
        let evs2 = vec![
            directive_event(&cmd, 1, "kill", "dev", None, "k2", 5000),
            directive_event(&cmd, 2, "resume", "dev", None, "r2", 5000),
        ];
        assert!(fold_governance(&evs2, CID, "dev", &pk).killed);
    }

    #[test]
    fn nonces_bind_to_deciding_directives() {
        let cmd = AgentIdentity::generate();
        let pk = [cmd.verifying_key_bytes()];
        let g = fold_governance(
            &[directive_event(&cmd, 1, "kill", "dev", None, "k9", 1000)],
            CID,
            "dev",
            &pk,
        );
        assert_eq!(g.kill_nonce.as_deref(), Some("k9"));
        let g2 = fold_governance(
            &[directive_event(
                &cmd,
                1,
                "budget_ceiling",
                "dev",
                Some(3.0),
                "b9",
                1000,
            )],
            CID,
            "dev",
            &pk,
        );
        assert_eq!(g2.budget_nonce.as_deref(), Some("b9"));
    }
}

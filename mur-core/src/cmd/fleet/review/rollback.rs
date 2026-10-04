//! §8.2: replay-with-damage and the rollback math on Continue. Bridges
//! `mur-channel`'s `load_events_with_damage` (P4) — which reports damage at
//! the `ChannelEvent`/signature level — with the pure `ledger` fold, which
//! only understands [`ReviewPayload`](super::schema::ReviewPayload)s. This module decides round
//! boundaries, classifies damage as Partial/Fatal (P1: never mid-round),
//! and computes the monotonic clock/cost adopted on Continue (AC11d).

use std::collections::HashSet;

use mur_common::channel::{ChannelEvent, EventKind};

use super::ledger::{FoldError, Ledger};
use super::schema::{Cumulative, NoteClassification, classify_note_payload, payload_round};

/// Outcome of replaying a session's events with damage detection (§8.2).
#[derive(Debug, Clone, PartialEq)]
pub enum ReplayOutcome {
    /// No damage at all — `ledger` is the full, current state.
    Clean(Ledger),
    /// Partial — damage after at least one complete round (P1: rolled back
    /// to the last COMPLETE round, never a mid-round event). `ledger` is
    /// rebuilt up to `last_good_round`, with its cumulative fields already
    /// raised to the AC11d lower bound from every readable post-N line.
    Partial {
        ledger: Ledger,
        last_good_round: u32,
        /// 1-based line number of the first damaged line/event.
        damage_line: usize,
        damage_reason: String,
    },
    /// Fatal — the first review event is damaged, or no complete round is
    /// valid. Resume is impossible.
    Fatal {
        damage_line: usize,
        damage_reason: String,
    },
}

/// One slot in file order: either a successfully-parsed `ChannelEvent` (with
/// its 1-based source line) or a line that failed to parse as one at all.
enum Slot<'a> {
    Parsed {
        line: usize,
        event: &'a ChannelEvent,
    },
    Unparseable {
        line: usize,
    },
}

fn merge_in_line_order<'a>(
    events: &'a [ChannelEvent],
    event_lines: &[usize],
    unparseable_lines: &[usize],
) -> Vec<Slot<'a>> {
    let mut slots: Vec<Slot<'a>> = Vec::with_capacity(events.len() + unparseable_lines.len());
    for (event, &line) in events.iter().zip(event_lines.iter()) {
        slots.push(Slot::Parsed { line, event });
    }
    for &line in unparseable_lines {
        slots.push(Slot::Unparseable { line });
    }
    slots.sort_by_key(|s| match s {
        Slot::Parsed { line, .. } => *line,
        Slot::Unparseable { line } => *line,
    });
    slots
}

/// Replay a session's events, classifying damage per §8.2.
///
/// `events`/`event_lines`/`unparseable_lines` come straight from
/// [`mur_channel::store::ChannelStore::load_events_with_damage`] (P4).
/// `unverified_seqs` is the set of `seq` values present in that call's
/// `DamageReport.unverified` (bad or missing-when-required signature).
/// `raw_lines` is the channel's `events.jsonl` split into lines (1-indexed
/// via `raw_lines[line - 1]`), used ONLY for the AC11d lenient numeric scan
/// — never to decide parse/signature/schema validity, which all come from
/// the structured inputs above.
pub fn replay_with_damage(
    events: &[ChannelEvent],
    event_lines: &[usize],
    unparseable_lines: &[usize],
    unverified_seqs: &HashSet<u64>,
    raw_lines: &[&str],
) -> ReplayOutcome {
    let slots = merge_in_line_order(events, event_lines, unparseable_lines);

    let mut ledger = Ledger::default();
    let mut round_in_progress: u32 = 0;
    let mut checkpoint: Option<(Ledger, u32, usize)> = None; // (ledger, round, line)

    for slot in &slots {
        match slot {
            Slot::Unparseable { line } => {
                return seal_or_fatal(
                    checkpoint,
                    *line,
                    "line is unparseable or truncated".to_string(),
                    raw_lines,
                );
            }
            Slot::Parsed { line, event } => {
                if event.kind != EventKind::Note {
                    continue; // not a review carrier at all; ignore (§4).
                }
                match classify_note_payload(&event.payload) {
                    NoteClassification::NotReview => continue,
                    NoteClassification::Malformed => {
                        return seal_or_fatal(
                            checkpoint,
                            *line,
                            "review payload failed schema validation".to_string(),
                            raw_lines,
                        );
                    }
                    NoteClassification::Review(env) => {
                        if let Some(r) = payload_round(&env.payload)
                            && r > round_in_progress
                        {
                            if round_in_progress > 0 {
                                ledger.note_round_complete();
                                checkpoint = Some((ledger.clone(), round_in_progress, *line));
                            }
                            round_in_progress = r;
                        }
                        if unverified_seqs.contains(&event.seq) {
                            return seal_or_fatal(
                                checkpoint,
                                *line,
                                "signature verification failed".to_string(),
                                raw_lines,
                            );
                        }
                        if let Err(e) = ledger.apply(&env.payload) {
                            return seal_or_fatal(
                                checkpoint,
                                *line,
                                fold_error_reason(&e),
                                raw_lines,
                            );
                        }
                    }
                }
            }
        }
    }
    ReplayOutcome::Clean(ledger)
}

fn fold_error_reason(e: &FoldError) -> String {
    format!("illegal state transition: {e}")
}

/// Build the final outcome once damage is hit: `Partial` from the last
/// sealed checkpoint (if any), raising its cumulative fields to the AC11d
/// lower bound; `Fatal` when nothing was ever sealed.
fn seal_or_fatal(
    checkpoint: Option<(Ledger, u32, usize)>,
    damage_line: usize,
    damage_reason: String,
    raw_lines: &[&str],
) -> ReplayOutcome {
    match checkpoint {
        Some((mut ledger, round, checkpoint_line)) => {
            let lower_bound = scan_readable_cumulative_lower_bound(raw_lines, checkpoint_line);
            ledger.adopt_cumulative_lower_bound(&lower_bound);
            ReplayOutcome::Partial {
                ledger,
                last_good_round: round,
                damage_line,
                damage_reason,
            }
        }
        None => ReplayOutcome::Fatal {
            damage_line,
            damage_reason,
        },
    }
}

/// §8.2 "Limits on rollback": scan every raw line AFTER `checkpoint_line`
/// (1-based; i.e. lines `checkpoint_line+1..=len`) for `exec_time_ms`/
/// `cost_usd_micros` values, taking the max of whatever is READABLE —
/// regardless of whether the line's JSON parses fully, its signature
/// verifies, or its review payload validates. "A line too truncated to
/// yield the number contributes nothing."
fn scan_readable_cumulative_lower_bound(raw_lines: &[&str], checkpoint_line: usize) -> Cumulative {
    let mut best = Cumulative {
        exec_time_ms: 0,
        cost_usd_micros: 0,
    };
    for line in raw_lines.iter().skip(checkpoint_line) {
        if let Some(v) = extract_readable_u64(line, "\"exec_time_ms\":") {
            best.exec_time_ms = best.exec_time_ms.max(v);
        }
        if let Some(v) = extract_readable_u64(line, "\"cost_usd_micros\":") {
            best.cost_usd_micros = best.cost_usd_micros.max(v);
        }
    }
    best
}

/// Find `key` in `line` and parse the run of ASCII digits immediately
/// following it (skipping ASCII whitespace). `None` when the key is absent,
/// or present but not followed by a parseable digit run (truncated mid-key
/// or mid-number) — "contributes nothing" per §8.2.
fn extract_readable_u64(line: &str, key: &str) -> Option<u64> {
    let idx = line.find(key)?;
    let rest = &line[idx + key.len()..];
    let rest = rest.trim_start();
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse::<u64>().ok()
}

impl Ledger {
    /// §8.2 "Limits on rollback — clock and cost are monotonic (fail-closed)":
    /// raise (never lower) the ledger's cumulative totals to at least
    /// `lower_bound`. Public on `Ledger` because both the normal fold
    /// (`adopt_cumulative`, per-event) and this rollback step
    /// (whole-of-remaining-file) need the same "max, never decrease" rule.
    pub fn adopt_cumulative_lower_bound(&mut self, lower_bound: &Cumulative) {
        self.exec_time_ms = self.exec_time_ms.max(lower_bound.exec_time_ms);
        self.cost_usd_micros = self.cost_usd_micros.max(lower_bound.cost_usd_micros);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::fleet::review::schema::{
        Mode, PriorUpdateDto, ReviewPayload, Role, Severity, VerdictKind, to_note_payload,
    };
    use mur_common::channel::ChannelActor;

    fn note_event(seq: u64, payload: serde_json::Value) -> ChannelEvent {
        ChannelEvent {
            seq,
            ts: chrono::Utc::now(),
            actor: ChannelActor::System,
            kind: EventKind::Note,
            payload,
            idempotency_key: None,
            sig: None,
            key_version: None,
        }
    }

    fn raw_line_for(ev: &ChannelEvent) -> String {
        serde_json::to_string(ev).unwrap()
    }

    /// Build a clean multi-round log: round 1..=up_to, each a
    /// `TurnSent` + `Verdict(Revise)` pair carrying increasing cumulative
    /// values, so later tests have known-good checkpoints to roll back to.
    fn build_rounds(up_to: u32) -> (Vec<ChannelEvent>, Vec<String>) {
        let mut events = Vec::new();
        let mut raw = Vec::new();
        let mut seq = 1;
        for round in 1..=up_to {
            let turn = note_event(
                seq,
                to_note_payload(&ReviewPayload::TurnSent {
                    round,
                    to: Role::Main,
                    restart_note: None,
                }),
            );
            raw.push(raw_line_for(&turn));
            events.push(turn);
            seq += 1;
            let verdict = note_event(
                seq,
                to_note_payload(&ReviewPayload::Verdict {
                    round,
                    kind: VerdictKind::Revise,
                    cumulative: Cumulative {
                        exec_time_ms: (round as u64) * 1000,
                        cost_usd_micros: (round as u64) * 100,
                    },
                }),
            );
            raw.push(raw_line_for(&verdict));
            events.push(verdict);
            seq += 1;
        }
        (events, raw)
    }

    fn line_numbers(n: usize) -> Vec<usize> {
        (1..=n).collect()
    }

    /// AC11a(i): damage = an unparseable/truncated line in round 4, after a
    /// valid log for rounds 1..3.
    #[test]
    fn partial_replay_on_an_unparseable_line_in_round_4() {
        let (mut events, mut raw) = build_rounds(3);
        // Round 4 starts, then the line after it is unparseable.
        let turn4 = note_event(
            7,
            to_note_payload(&ReviewPayload::TurnSent {
                round: 4,
                to: Role::Main,
                restart_note: None,
            }),
        );
        raw.push(raw_line_for(&turn4));
        events.push(turn4);
        raw.push("{not json at all".to_string());

        let event_lines = line_numbers(events.len());
        let unparseable = vec![events.len() + 1];
        let raw_refs: Vec<&str> = raw.iter().map(String::as_str).collect();

        let outcome = replay_with_damage(
            &events,
            &event_lines,
            &unparseable,
            &HashSet::new(),
            &raw_refs,
        );
        match outcome {
            ReplayOutcome::Partial {
                ledger,
                last_good_round,
                damage_line,
                ..
            } => {
                assert_eq!(last_good_round, 3, "rounds 1..3 are complete");
                assert_eq!(ledger.round, 3);
                assert_eq!(damage_line, events.len() + 1);
                // Round-4's TurnSent must NOT have been applied — no event
                // from the partial round is applied.
                assert_eq!(ledger.exec_time_ms, 3000);
            }
            other => panic!("expected Partial, got {other:?}"),
        }
    }

    /// AC11a(ii): damage = a bad signature in round 4.
    #[test]
    fn partial_replay_on_a_bad_signature_in_round_4() {
        let (mut events, mut raw) = build_rounds(3);
        let bad_verdict = note_event(
            8,
            to_note_payload(&ReviewPayload::Verdict {
                round: 4,
                kind: VerdictKind::Approve,
                cumulative: Cumulative {
                    exec_time_ms: 9000,
                    cost_usd_micros: 900,
                },
            }),
        );
        raw.push(raw_line_for(&bad_verdict));
        events.push(bad_verdict.clone());

        let event_lines = line_numbers(events.len());
        let mut unverified = HashSet::new();
        unverified.insert(bad_verdict.seq);
        let raw_refs: Vec<&str> = raw.iter().map(String::as_str).collect();

        let outcome = replay_with_damage(&events, &event_lines, &[], &unverified, &raw_refs);
        match outcome {
            ReplayOutcome::Partial {
                last_good_round,
                ledger,
                ..
            } => {
                assert_eq!(last_good_round, 3);
                assert_eq!(ledger.round, 3);
            }
            other => panic!("expected Partial, got {other:?}"),
        }
    }

    /// AC11a(iii): damage = a schema-invalid review payload in round 4.
    #[test]
    fn partial_replay_on_a_schema_invalid_payload_in_round_4() {
        let (mut events, mut raw) = build_rounds(3);
        // First a clean round-4 marker to advance round_in_progress, then a
        // malformed review payload.
        let turn4 = note_event(
            7,
            to_note_payload(&ReviewPayload::TurnSent {
                round: 4,
                to: Role::Main,
                restart_note: None,
            }),
        );
        raw.push(raw_line_for(&turn4));
        events.push(turn4);
        let malformed = note_event(
            8,
            serde_json::json!({"review": {"v": 1, "type": "not_a_real_kind"}}),
        );
        raw.push(raw_line_for(&malformed));
        events.push(malformed);

        let event_lines = line_numbers(events.len());
        let raw_refs: Vec<&str> = raw.iter().map(String::as_str).collect();
        let outcome = replay_with_damage(&events, &event_lines, &[], &HashSet::new(), &raw_refs);
        match outcome {
            ReplayOutcome::Partial {
                last_good_round, ..
            } => assert_eq!(last_good_round, 3),
            other => panic!("expected Partial, got {other:?}"),
        }
    }

    /// AC11b: damage in the very first review event ⇒ Fatal, no continue
    /// option (represented by the enum variant itself).
    #[test]
    fn fatal_when_the_first_review_event_is_damaged() {
        let bad = note_event(
            1,
            serde_json::json!({"review": {"v": 1, "type": "nonsense"}}),
        );
        let raw = [raw_line_for(&bad)];
        let events = vec![bad];
        let raw_refs: Vec<&str> = raw.iter().map(String::as_str).collect();
        let outcome = replay_with_damage(&events, &[1], &[], &HashSet::new(), &raw_refs);
        assert!(matches!(outcome, ReplayOutcome::Fatal { .. }));
    }

    /// AC11b: damage while round 1 is still in progress (never sealed) ⇒
    /// Fatal — "no complete round is valid".
    #[test]
    fn fatal_when_no_round_ever_completes() {
        let turn1 = note_event(
            1,
            to_note_payload(&ReviewPayload::TurnSent {
                round: 1,
                to: Role::Main,
                restart_note: None,
            }),
        );
        let mut raw = vec![raw_line_for(&turn1)];
        let events = vec![turn1];
        raw.push("{garbage".to_string());
        let raw_refs: Vec<&str> = raw.iter().map(String::as_str).collect();
        let outcome = replay_with_damage(&events, &[1], &[2], &HashSet::new(), &raw_refs);
        assert!(matches!(outcome, ReplayOutcome::Fatal { .. }));
    }

    /// AC11d: readable post-round-3 events (good, bad-sig, and
    /// schema-invalid-but-numerically-readable) report HIGHER cost/time
    /// than round 3's own — the adopted values must be at least that high,
    /// never lower, and a lower-valued post-N event must not pull them
    /// down.
    #[test]
    fn rollback_adopts_the_highest_readable_post_round_values() {
        let (mut events, mut raw) = build_rounds(3); // round 3: exec=3000, cost=300
        // Round-4 TurnSent seals round 3.
        let turn4 = note_event(
            7,
            to_note_payload(&ReviewPayload::TurnSent {
                round: 4,
                to: Role::Main,
                restart_note: None,
            }),
        );
        raw.push(raw_line_for(&turn4));
        events.push(turn4);
        // A bad-signature event reporting a HIGHER value.
        let bad_sig_verdict = note_event(
            8,
            to_note_payload(&ReviewPayload::Verdict {
                round: 4,
                kind: VerdictKind::Revise,
                cumulative: Cumulative {
                    exec_time_ms: 9_000,
                    cost_usd_micros: 900,
                },
            }),
        );
        raw.push(raw_line_for(&bad_sig_verdict));
        events.push(bad_sig_verdict.clone());
        // A schema-invalid payload whose numeric field still parses —
        // crafted directly as raw JSON so the outer event parses but the
        // review envelope does not.
        let malformed_raw = serde_json::json!({
            "seq": 9_u64, "ts": chrono::Utc::now(), "actor": "system",
            "kind": "note",
            "payload": {"review": {"v": 1, "type": "verdict", "round": 4,
                "exec_time_ms": 12_000, "cost_usd_micros": 1_200
                // missing required "kind" field -> fails to deserialize
            }}
        });
        raw.push(malformed_raw.to_string());
        let malformed_event = note_event(
            9,
            serde_json::json!({"review": {"v": 1, "type": "verdict", "round": 4,
                "exec_time_ms": 12_000, "cost_usd_micros": 1_200}}),
        );
        events.push(malformed_event);
        // A LOWER-valued event afterward must not pull the totals down.
        let low_verdict = note_event(
            10,
            to_note_payload(&ReviewPayload::Verdict {
                round: 4,
                kind: VerdictKind::Revise,
                cumulative: Cumulative {
                    exec_time_ms: 1,
                    cost_usd_micros: 1,
                },
            }),
        );
        raw.push(raw_line_for(&low_verdict));
        // events vector stops accumulating valid structured events here —
        // we feed line 10 to the raw-scan path only, matching how a
        // post-damage tail is still scanned for AC11d even though replay
        // itself stopped at the first damage (line 8, bad signature).

        let event_lines = line_numbers(events.len());
        let mut unverified = HashSet::new();
        unverified.insert(bad_sig_verdict.seq);
        let raw_refs: Vec<&str> = raw.iter().map(String::as_str).collect();

        let outcome = replay_with_damage(&events, &event_lines, &[], &unverified, &raw_refs);
        match outcome {
            ReplayOutcome::Partial {
                ledger,
                last_good_round,
                ..
            } => {
                assert_eq!(last_good_round, 3);
                assert_eq!(
                    ledger.exec_time_ms, 12_000,
                    "adopts the highest READABLE post-N value"
                );
                assert_eq!(ledger.cost_usd_micros, 1_200);
            }
            other => panic!("expected Partial, got {other:?}"),
        }
    }

    /// A clean log with no damage at all replays to the full ledger.
    #[test]
    fn clean_replay_with_no_damage_returns_the_full_ledger() {
        let (events, raw) = build_rounds(3);
        let event_lines = line_numbers(events.len());
        let raw_refs: Vec<&str> = raw.iter().map(String::as_str).collect();
        let outcome = replay_with_damage(&events, &event_lines, &[], &HashSet::new(), &raw_refs);
        match outcome {
            ReplayOutcome::Clean(ledger) => assert_eq!(ledger.round, 3),
            other => panic!("expected Clean, got {other:?}"),
        }
    }

    /// §8.2: "an illegal state transition (e.g. a `finding_status` for an
    /// ID never issued)" counts as damage too, rolling back the same way.
    #[test]
    fn an_illegal_state_transition_is_damage() {
        let (mut events, mut raw) = build_rounds(3);
        let bad_status = note_event(
            7,
            to_note_payload(&ReviewPayload::FindingStatus {
                round: 4,
                id: "F99".to_string(),
                status: super::super::schema::FindingStatus::Resolved,
                reason: None,
            }),
        );
        raw.push(raw_line_for(&bad_status));
        events.push(bad_status);
        let event_lines = line_numbers(events.len());
        let raw_refs: Vec<&str> = raw.iter().map(String::as_str).collect();
        let outcome = replay_with_damage(&events, &event_lines, &[], &HashSet::new(), &raw_refs);
        match outcome {
            ReplayOutcome::Partial {
                last_good_round, ..
            } => assert_eq!(last_good_round, 3),
            other => panic!("expected Partial, got {other:?}"),
        }
    }

    /// A line too truncated to yield the number contributes nothing to the
    /// AC11d lower bound (it must not panic or silently invent a value).
    #[test]
    fn extract_readable_u64_returns_none_for_a_truncated_number() {
        assert_eq!(
            extract_readable_u64("\"exec_time_ms\":12", "\"exec_time_ms\":"),
            Some(12)
        );
        assert_eq!(
            extract_readable_u64("\"exec_time_ms\":", "\"exec_time_ms\":"),
            None
        );
        assert_eq!(
            extract_readable_u64("no key here", "\"exec_time_ms\":"),
            None
        );
    }

    #[allow(dead_code)]
    fn unused_prior_update_silencer() -> PriorUpdateDto {
        PriorUpdateDto {
            id: "F1".into(),
            status: super::super::schema::FindingStatus::Open,
            reason: None,
        }
    }

    #[allow(dead_code)]
    fn unused_mode_silencer() -> Mode {
        Mode::Auto
    }

    #[allow(dead_code)]
    fn unused_severity_silencer() -> Severity {
        Severity::Low
    }
}

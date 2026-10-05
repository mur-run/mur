//! AC11c (no tampering): for the AC11a (partial) and AC11b (fatal) replay
//! scenarios, every byte of `events.jsonl` that existed before a replay run
//! is unchanged afterwards. Only marker events are appended — never a
//! rewrite, truncation, or edit of an existing line.
//!
//! Unlike `rollback.rs`'s unit tests (which build `ChannelEvent`s in-memory
//! and never touch a filesystem), this module drives a REAL
//! `mur_channel::store::ChannelStore` against a real `events.jsonl`, so the
//! "no tampering" claim is checked against the actual write path
//! (`append_event`'s single `write_all`), not just the pure fold.

use std::collections::HashSet;
use std::fs;

use mur_channel::store::ChannelStore;
use mur_common::channel::{Channel, ChannelActor, ChannelState, EventKind, Goal};
use mur_common::identity::AgentIdentity;
use tempfile::TempDir;

use super::rollback::{ReplayOutcome, replay_with_damage};
use super::schema::ReviewPayload;
use super::schema::{Cumulative, Role, VerdictKind, to_note_payload};

fn sample_channel(id: &str) -> Channel {
    let now = chrono::Utc::now();
    Channel {
        v: mur_common::channel::CHANNEL_SCHEMA_VERSION,
        id: id.to_string(),
        title: "t".into(),
        goal: Goal::default(),
        state: ChannelState::Working,
        purpose: None,
        owner: ChannelActor::Human { name: "me".into() },
        participants: vec![],
        created_at: now,
        updated_at: now,
    }
}

/// Write `round` complete rounds (TurnSent + Verdict pairs) to a real
/// channel, via `append_event` — the same write path the running driver
/// uses — so the on-disk bytes are the ones AC11c must prove untouched.
fn write_clean_rounds(store: &ChannelStore, id: &str, up_to: u32) {
    for round in 1..=up_to {
        store
            .append_event(
                id,
                ChannelActor::System,
                EventKind::Note,
                to_note_payload(&ReviewPayload::TurnSent {
                    round,
                    to: Role::Main,
                    restart_note: None,
                    human_wait_ms: 0,
                }),
                None,
                None,
                None,
            )
            .unwrap();
        store
            .append_event(
                id,
                ChannelActor::System,
                EventKind::Note,
                to_note_payload(&ReviewPayload::Verdict {
                    round,
                    kind: VerdictKind::Revise,
                    cumulative: Cumulative {
                        exec_time_ms: u64::from(round) * 1000,
                        cost_usd_micros: u64::from(round) * 100,
                    },
                }),
                None,
                None,
                None,
            )
            .unwrap();
    }
}

/// Load the full raw text + a line-split view of a channel's `events.jsonl`,
/// for both the damage-detection read path and the byte-identity check.
fn load_raw_and_lines(store: &ChannelStore, id: &str) -> (String, Vec<String>) {
    let raw = fs::read_to_string(store.events_path(id)).unwrap();
    let lines: Vec<String> = raw.lines().map(str::to_string).collect();
    (raw, lines)
}

/// AC11c, partial case (pairs with AC11a): damage an on-disk log after
/// round 3 by appending an unparseable line directly to `events.jsonl`
/// (bypassing `append_event`, since a garbled append cannot come from the
/// store's own write path) — then replay it, and confirm that every byte
/// present BEFORE the replay call is still present, byte-for-byte,
/// afterwards. Finally, append the Continue marker event
/// (`resumed_from_checkpoint`) the normal way and confirm it is the only
/// thing added.
#[test]
fn ac11c_partial_replay_does_not_touch_pre_existing_bytes() {
    let tmp = TempDir::new().unwrap();
    let store = ChannelStore::new(tmp.path());
    let id = "c-ac11c-partial";
    store.create(&sample_channel(id)).unwrap();
    write_clean_rounds(&store, id, 3);

    // Round 4 starts, then the log is damaged by an unparseable line.
    store
        .append_event(
            id,
            ChannelActor::System,
            EventKind::Note,
            to_note_payload(&ReviewPayload::TurnSent {
                round: 4,
                to: Role::Main,
                restart_note: None,
                human_wait_ms: 0,
            }),
            None,
            None,
            None,
        )
        .unwrap();
    {
        let path = store.events_path(id);
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        use std::io::Write;
        f.write_all(b"{not json at all\n").unwrap();
    }

    // Snapshot every byte BEFORE replay runs.
    let before_raw = fs::read_to_string(store.events_path(id)).unwrap();

    let writer = AgentIdentity::generate();
    let (events, report) = store
        .load_events_with_damage(id, &writer.verifying_key_bytes(), false)
        .unwrap();
    let raw_lines: Vec<String> = before_raw.lines().map(str::to_string).collect();
    let raw_refs: Vec<&str> = raw_lines.iter().map(String::as_str).collect();
    let unverified_seqs: HashSet<u64> = report.unverified.iter().map(|e| e.seq).collect();

    let outcome = replay_with_damage(
        &events,
        &report.event_lines,
        &report.unparseable_lines,
        &unverified_seqs,
        &raw_refs,
    );
    let (last_good_round, damage_line, damage_reason) = match outcome {
        ReplayOutcome::Partial {
            last_good_round,
            damage_line,
            damage_reason,
            ..
        } => (last_good_round, damage_line, damage_reason),
        other => panic!("expected Partial, got {other:?}"),
    };
    assert_eq!(last_good_round, 3);

    // The replay call itself must not have touched the file at all.
    let after_replay_raw = fs::read_to_string(store.events_path(id)).unwrap();
    assert_eq!(
        before_raw, after_replay_raw,
        "replay (the read path) must not write anything"
    );

    // Continue: append the ONE marker event this outcome allows, via the
    // normal write path. This is the only write AC11c permits.
    store
        .append_event(
            id,
            ChannelActor::System,
            EventKind::Note,
            to_note_payload(&ReviewPayload::ResumedFromCheckpoint {
                round: last_good_round,
                damage_line: Some(damage_line),
                damage_reason,
                cumulative: Cumulative {
                    exec_time_ms: 3000,
                    cost_usd_micros: 300,
                },
            }),
            None,
            None,
            None,
        )
        .unwrap();

    let (after_marker_raw, after_marker_lines) = load_raw_and_lines(&store, id);
    let before_lines: Vec<String> = before_raw.lines().map(str::to_string).collect();

    // Every pre-existing byte is still there, in order, untouched.
    assert!(
        after_marker_raw.starts_with(&before_raw),
        "pre-existing bytes must be an unmodified prefix of the file after the marker append"
    );
    // Exactly one new line was appended: the marker event.
    assert_eq!(
        after_marker_lines.len(),
        before_lines.len() + 1,
        "only the marker event is appended — no line is rewritten, truncated, or removed"
    );
    let marker_line = &after_marker_lines[before_lines.len()];
    assert!(
        marker_line.contains("resumed_from_checkpoint"),
        "the single appended line must be the resumed_from_checkpoint marker: {marker_line}"
    );
}

/// AC11c, fatal case (pairs with AC11b): damage the very first review
/// event, confirm replay reports `Fatal` without touching any byte, then
/// append the `session_stopped(corrupted)` marker via the normal write path
/// and confirm it is the only thing added.
#[test]
fn ac11c_fatal_replay_does_not_touch_pre_existing_bytes() {
    let tmp = TempDir::new().unwrap();
    let store = ChannelStore::new(tmp.path());
    let id = "c-ac11c-fatal";
    store.create(&sample_channel(id)).unwrap();

    // The very first review event is damaged: a well-formed Note whose
    // review envelope does not validate (bad `type`).
    store
        .append_event(
            id,
            ChannelActor::System,
            EventKind::Note,
            serde_json::json!({"review": {"v": 1, "type": "nonsense"}}),
            None,
            None,
            None,
        )
        .unwrap();

    let before_raw = fs::read_to_string(store.events_path(id)).unwrap();

    let writer = AgentIdentity::generate();
    let (events, report) = store
        .load_events_with_damage(id, &writer.verifying_key_bytes(), false)
        .unwrap();
    let raw_lines: Vec<String> = before_raw.lines().map(str::to_string).collect();
    let raw_refs: Vec<&str> = raw_lines.iter().map(String::as_str).collect();
    let unverified_seqs: HashSet<u64> = report.unverified.iter().map(|e| e.seq).collect();

    let outcome = replay_with_damage(
        &events,
        &report.event_lines,
        &report.unparseable_lines,
        &unverified_seqs,
        &raw_refs,
    );
    assert!(
        matches!(outcome, ReplayOutcome::Fatal { .. }),
        "expected Fatal, got {outcome:?}"
    );

    let after_replay_raw = fs::read_to_string(store.events_path(id)).unwrap();
    assert_eq!(
        before_raw, after_replay_raw,
        "replay (the read path) must not write anything, even in the fatal case"
    );

    // Fatal marker: a session_stopped(corrupted) event, appended the normal
    // way (the channel IS appendable here; AC11g covers the non-appendable
    // case with the out-of-channel marker file instead).
    store
        .append_event(
            id,
            ChannelActor::System,
            EventKind::Note,
            to_note_payload(&ReviewPayload::SessionStopped {
                reason: super::constants::REVIEW_STOP_REASON_CORRUPTED.to_string(),
                unresolved: vec![],
                cumulative: Cumulative {
                    exec_time_ms: 0,
                    cost_usd_micros: 0,
                },
            }),
            None,
            None,
            None,
        )
        .unwrap();

    let (after_marker_raw, after_marker_lines) = load_raw_and_lines(&store, id);
    let before_lines: Vec<String> = before_raw.lines().map(str::to_string).collect();

    assert!(
        after_marker_raw.starts_with(&before_raw),
        "pre-existing bytes must be an unmodified prefix of the file after the marker append"
    );
    assert_eq!(
        after_marker_lines.len(),
        before_lines.len() + 1,
        "only the marker event is appended in the fatal case too"
    );
    let marker_line = &after_marker_lines[before_lines.len()];
    assert!(
        marker_line.contains("session_stopped") && marker_line.contains("corrupted"),
        "the single appended line must be the session_stopped(corrupted) marker: {marker_line}"
    );
}

//! Red-first tests for the #1770 hash chain. Each test asserts a property the
//! pre-chain store does NOT have, so on the stub they fail on an assertion,
//! not on compilation.

use super::*;
use crate::store::DamageReport;
use mur_common::identity::AgentIdentity;
use std::fs;
use tempfile::TempDir;

const CH: &str = "c1";

struct Fx {
    _tmp: TempDir,
    store: ChannelStore,
    id: AgentIdentity,
}

fn fx() -> Fx {
    let tmp = TempDir::new().unwrap();
    let store = ChannelStore::new(tmp.path());
    Fx {
        _tmp: tmp,
        store,
        id: AgentIdentity::generate(),
    }
}

impl Fx {
    /// A router-signed (v2) event.
    fn v2(&self, n: u64) -> ChannelEvent {
        self.store
            .append_signed_event(
                CH,
                &self.id,
                0,
                ChannelActor::System,
                EventKind::Message,
                serde_json::json!({ "n": n }),
                None,
            )
            .unwrap()
    }

    /// A v1 event signed by an external issuer before it reaches the store —
    /// the `mur-core/src/server/governance.rs` path, which cannot know the tail.
    fn v1_external(&self, n: u64) -> ChannelEvent {
        let payload = serde_json::json!({ "cmd": n });
        let actor = ChannelActor::Human {
            name: "commander".into(),
        };
        let sig = crate::sign::sign_event(&self.id, CH, &actor, EventKind::Message, &payload, None);
        self.store
            .append_event(
                CH,
                actor,
                EventKind::Message,
                payload,
                None,
                Some(sig),
                Some(0),
            )
            .unwrap()
    }

    /// An unsigned agent line.
    fn unsigned(&self, n: u64) -> ChannelEvent {
        self.store
            .append_event(
                CH,
                ChannelActor::Agent { id: "a".into() },
                EventKind::Message,
                serde_json::json!({ "agent": n }),
                None,
                None,
                None,
            )
            .unwrap()
    }

    fn lines(&self) -> Vec<String> {
        fs::read_to_string(self.store.events_path(CH))
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn rewrite(&self, lines: &[String]) {
        let mut body = lines.join("\n");
        body.push('\n');
        fs::write(self.store.events_path(CH), body).unwrap();
    }

    fn report(&self) -> DamageReport {
        self.store
            .load_events_with_damage(CH, &self.id.verifying_key_bytes(), false)
            .unwrap()
            .1
    }
}

/// The anchor of every chain. If this value changes, every existing v2 log
/// fails verification at its first line — so it is pinned literally.
#[test]
fn genesis_is_pinned_and_anchors_the_first_line() {
    assert_eq!(GENESIS_PREV_HASH, "0".repeat(64));
    let f = fx();
    let ev = f.v2(1);
    assert_eq!(ev.prev_hash.as_deref(), Some(GENESIS_PREV_HASH));
    assert!(f.report().is_clean());
}

#[test]
fn each_v2_event_commits_to_the_raw_line_before_it() {
    let f = fx();
    f.v2(1);
    let ev2 = f.v2(2);
    let lines = f.lines();
    assert_eq!(ev2.prev_hash, Some(line_hash(&lines[0])));
    let r = f.report();
    assert!(r.is_clean(), "an untouched v2 log is clean: {r:?}");
}

/// The reason for option A: a deleted line is a CHAIN break, not a signature
/// failure — the surviving event's own signature is still valid.
#[test]
fn deleting_a_middle_line_is_a_chain_break_not_a_bad_signature() {
    let f = fx();
    f.v2(1);
    f.v2(2);
    let ev3 = f.v2(3);
    let mut lines = f.lines();
    lines.remove(1);
    f.rewrite(&lines);

    let r = f.report();
    assert_eq!(r.chain_breaks, vec![ev3.seq]);
    assert!(r.unverified.is_empty(), "sig of ev3 is intact: {r:?}");
}

#[test]
fn swapping_two_lines_breaks_both_links() {
    let f = fx();
    f.v2(1);
    let ev2 = f.v2(2);
    let ev3 = f.v2(3);
    let mut lines = f.lines();
    lines.swap(1, 2);
    f.rewrite(&lines);

    assert_eq!(f.report().chain_breaks, vec![ev3.seq, ev2.seq]);
}

/// Mixed log, v1 and unsigned lines in the MIDDLE (upgrade-era history plus
/// ongoing commander/agent traffic). They carry no `prev_hash`, but the v2
/// line after them commits to them, so deleting one is still caught.
#[test]
fn v1_and_unsigned_lines_in_the_middle_are_covered_by_the_next_v2() {
    let f = fx();
    f.v2(1);
    let v1 = f.v1_external(2);
    let un = f.unsigned(3);
    let ev4 = f.v2(4);
    assert_eq!(v1.prev_hash, None);
    assert_eq!(un.prev_hash, None);
    assert!(f.report().is_clean());

    let mut lines = f.lines();
    lines.remove(2); // the unsigned line, directly before ev4
    f.rewrite(&lines);
    assert_eq!(f.report().chain_breaks, vec![ev4.seq]);
}

/// The governance.rs case, and the one that keeps happening: the log ENDS in
/// an externally signed v1 command, and the next router append is what makes
/// that command tamper-evident.
#[test]
fn v1_tail_is_committed_to_by_the_next_router_append() {
    let f = fx();
    f.v2(1);
    f.v1_external(2);

    // Known window (#1770 comment): until a v2 line follows, deleting the v1
    // tail is undetectable. Characterization, not a goal — pinned so a change
    // that closes the window has to update this on purpose.
    let original = f.lines();
    let mut no_tail = original.clone();
    no_tail.pop();
    f.rewrite(&no_tail);
    assert!(f.report().chain_breaks.is_empty());
    f.rewrite(&original);

    let ev3 = f.v2(3);
    let lines = f.lines();
    assert_eq!(ev3.prev_hash, Some(line_hash(&lines[1])));
    assert!(f.report().is_clean());

    let mut lines = lines;
    lines.remove(1);
    f.rewrite(&lines);
    assert_eq!(f.report().chain_breaks, vec![ev3.seq]);
}

/// A log with no v2 line has nothing to check — pre-upgrade logs stay clean.
#[test]
fn a_log_without_v2_lines_reports_no_chain_breaks() {
    let f = fx();
    f.v1_external(1);
    f.unsigned(2);
    f.v1_external(3);
    let mut lines = f.lines();
    lines.remove(1);
    f.rewrite(&lines);
    assert!(f.report().chain_breaks.is_empty());
}

/// The signature must cover `prev_hash`; otherwise an attacker deletes a line
/// and simply rewrites the next event's `prev_hash` to match.
#[test]
fn rewriting_prev_hash_to_hide_a_deletion_invalidates_the_signature() {
    let f = fx();
    f.v2(1);
    f.v2(2);
    let ev3 = f.v2(3);
    let mut lines = f.lines();
    lines.remove(1);
    let mut forged: ChannelEvent = serde_json::from_str(&lines[1]).unwrap();
    forged.prev_hash = Some(line_hash(&lines[0]));
    lines[1] = serde_json::to_string(&forged).unwrap();
    f.rewrite(&lines);

    let r = f.report();
    assert!(r.chain_breaks.is_empty(), "the link itself now matches");
    assert_eq!(
        r.unverified.iter().map(|e| e.seq).collect::<Vec<_>>(),
        vec![ev3.seq],
        "but the signature over the original prev_hash does not"
    );
}

/// Downgrade forgery: stripping `prev_hash` from a v2 line must not let it
/// pass as a v1 line (which the chain does not check).
#[test]
fn stripping_prev_hash_does_not_downgrade_to_a_valid_v1_event() {
    let f = fx();
    f.v2(1);
    let ev2 = f.v2(2);
    let mut lines = f.lines();
    let mut forged: ChannelEvent = serde_json::from_str(&lines[1]).unwrap();
    forged.prev_hash = None;
    lines[1] = serde_json::to_string(&forged).unwrap();
    f.rewrite(&lines);

    let r = f.report();
    assert_eq!(
        r.unverified.iter().map(|e| e.seq).collect::<Vec<_>>(),
        vec![ev2.seq]
    );
}

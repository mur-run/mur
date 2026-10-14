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

/// The chain hashes RAW bytes, so any future path that rewrites the log by
/// parse → serialize (compaction, archive, migration) must reproduce every
/// line byte-for-byte, or it breaks every link after it. Pins it for all
/// three line shapes, including `prev_hash: None` staying absent.
#[test]
fn every_line_shape_survives_a_serde_round_trip_byte_for_byte() {
    let f = fx();
    f.v2(1);
    f.v1_external(2);
    f.unsigned(3);
    f.v2(4);
    let lines = f.lines();
    assert!(!lines[1].contains("prev_hash"));
    assert!(!lines[2].contains("prev_hash"));
    for line in &lines {
        let ev: ChannelEvent = serde_json::from_str(line).unwrap();
        assert_eq!(&serde_json::to_string(&ev).unwrap(), line);
    }
}

// ---- signed heads (#1770 part 2, #1868 format) ----

use crate::store::AnchorMismatchKind as K;

impl Fx {
    fn mur_home(&self) -> &std::path::Path {
        self._tmp.path()
    }

    fn heads(&self) -> std::path::PathBuf {
        crate::anchor::heads_dir(self.mur_home())
    }

    /// A head signed by `signer` over the CURRENT log.
    fn head_by(&self, signer: &AgentIdentity, channel_id: &str) -> crate::head::Head {
        let log = fs::read(self.store.events_path(CH)).unwrap();
        let state = crate::head::chain_log(&log);
        let seq = serde_json::from_str::<ChannelEvent>(f_last(&log))
            .unwrap()
            .seq;
        crate::head::Head::seal(
            signer,
            crate::head::HeadFields {
                channel_id: channel_id.into(),
                seq,
                lines: state.lines,
                chain_hash: hex::encode(state.hash),
                key_version: Some(1),
                legacy_through: None,
            },
            chrono::Utc::now(),
        )
    }

    /// Seal a valid head at the current tail of the log.
    fn anchor_now(&self) {
        crate::head_io::write_head(&self.heads(), &self.head_by(&self.id, CH)).unwrap();
    }

    fn mismatch(&self) -> K {
        self.report().anchor_mismatch.expect("anchor mismatch").kind
    }
}

fn f_last(log: &[u8]) -> &str {
    std::str::from_utf8(log).unwrap().lines().last().unwrap()
}

#[test]
fn no_head_means_no_mismatch() {
    let f = fx();
    f.v2(0);
    f.v2(1);
    let r = f.report();
    assert_eq!(r.anchor_mismatch, None);
    assert!(r.is_clean());
}

#[test]
fn log_exactly_at_head_is_clean() {
    let f = fx();
    f.v2(0);
    f.v2(1);
    f.anchor_now();
    let r = f.report();
    assert_eq!(r.anchor_mismatch, None);
    assert!(r.is_clean());
}

#[test]
fn log_longer_than_head_is_clean_when_the_covered_prefix_is_intact() {
    let f = fx();
    f.v2(0);
    f.anchor_now();
    f.unsigned(1); // sandboxed agent appends past the head: allowed.
    f.v2(2);
    let r = f.report();
    assert_eq!(r.anchor_mismatch, None);
    assert!(r.is_clean());
}

#[test]
fn truncating_below_the_head_is_a_mismatch_even_though_every_link_holds() {
    let f = fx();
    f.v2(0);
    f.v2(1);
    f.v2(2);
    f.anchor_now();
    let lines = f.lines();
    f.rewrite(&lines[..2]); // drop seq 2: chain_breaks sees nothing wrong.
    let r = f.report();
    assert!(r.chain_breaks.is_empty(), "adjacent links are all valid");
    let m = r.anchor_mismatch.as_ref().expect("anchor mismatch");
    assert_eq!(m.kind, K::Truncated);
    assert_eq!((m.head.lines, m.log_lines), (3, 2));
    assert!(!r.is_clean());
}

#[test]
fn replacing_a_covered_line_is_a_mismatch_even_at_the_same_length() {
    let f = fx();
    f.v2(0);
    f.v2(1);
    f.anchor_now();
    let lines = f.lines();
    f.rewrite(&lines[..1]);
    f.v2(1); // same seq, same line count, different bytes.
    assert_eq!(f.lines().len(), 2);
    assert_eq!(f.mismatch(), K::Rewritten);
}

/// The cumulative chain covers lines no `prev_hash` does: an unsigned line
/// edited in the middle of a head-covered prefix is caught.
#[test]
fn editing_an_unsigned_covered_line_is_a_mismatch() {
    let f = fx();
    f.v2(0);
    f.unsigned(1);
    f.anchor_now();
    let mut lines = f.lines();
    lines[1] = lines[1].replace("\"seq\":1", "\"seq\":1 ");
    f.rewrite(&lines);
    assert_eq!(f.mismatch(), K::Rewritten);
}

#[test]
fn emptied_log_with_a_head_is_a_mismatch() {
    let f = fx();
    f.v2(0);
    f.anchor_now();
    fs::write(f.store.events_path(CH), b"").unwrap();
    let m = f.report().anchor_mismatch.expect("anchor mismatch");
    assert_eq!((m.kind, m.log_lines), (K::Truncated, 0));
}

/// The point of signing: an attacker who can write `channel-heads/` but has
/// no router key cannot re-anchor a truncated log.
#[test]
fn a_head_forged_by_another_key_is_a_mismatch() {
    let f = fx();
    f.v2(0);
    f.v2(1);
    let forger = AgentIdentity::generate();
    crate::head_io::write_head(&f.heads(), &f.head_by(&forger, CH)).unwrap();
    assert_eq!(f.mismatch(), K::BadSignature);
}

#[test]
fn a_hand_edited_head_is_a_mismatch() {
    let f = fx();
    f.v2(0);
    f.v2(1);
    let mut h = f.head_by(&f.id, CH);
    h.lines = 1; // pretend the head only ever covered seq 0
    crate::head_io::write_head(&f.heads(), &h).unwrap();
    assert_eq!(f.mismatch(), K::BadSignature);
}

#[test]
fn a_validly_signed_head_of_another_channel_is_a_mismatch() {
    let f = fx();
    f.v2(0);
    let other = f.head_by(&f.id, "elsewhere");
    let path = crate::head_io::head_path(&f.heads(), CH).unwrap();
    fs::create_dir_all(f.heads()).unwrap();
    fs::write(path, serde_json::to_vec(&other).unwrap()).unwrap();
    assert_eq!(f.mismatch(), K::WrongChannel);
}

#[test]
fn a_corrupt_head_file_is_an_error_not_no_head() {
    let f = fx();
    f.v2(0);
    fs::create_dir_all(f.heads()).unwrap();
    fs::write(crate::head_io::head_path(&f.heads(), CH).unwrap(), b"{nope").unwrap();
    assert!(
        f.store
            .load_events_with_damage(CH, &f.id.verifying_key_bytes(), false)
            .is_err()
    );
}

#[test]
fn a_dirty_marker_alone_makes_the_channel_not_clean() {
    let f = fx();
    f.v2(0);
    f.anchor_now();
    crate::anchor::mark_dirty(f.mur_home(), CH).unwrap();
    let r = f.report();
    assert!(r.anchor_dirty);
    assert!(!r.is_clean());
    crate::anchor::clear_dirty(f.mur_home(), CH).unwrap();
    assert!(f.report().is_clean());
}

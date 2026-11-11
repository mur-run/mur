//! #2287: lines written by a newer MUR. An unknown `kind` is not damage, but
//! it is no pass on the rest of the line either — the signature and the
//! chain link are checked as for any other line. The records under test are
//! appended past the last event, the tail case no later line protects.

use super::*;
use crate::sign::sign_input_v2_wire_kind;
use mur_common::identity::AgentIdentity;
use tempfile::TempDir;

const CH: &str = "c1";
const FUTURE_KIND: &str = "kind-from-a-newer-mur";

struct Fx {
    _tmp: TempDir,
    store: ChannelStore,
    id: AgentIdentity,
}

fn fx() -> Fx {
    let tmp = TempDir::new().unwrap();
    let store = ChannelStore::new(tmp.path());
    let f = Fx {
        _tmp: tmp,
        store,
        id: AgentIdentity::generate(),
    };
    f.signed(1);
    f
}

impl Fx {
    fn signed(&self, n: u64) -> ChannelEvent {
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

    fn path(&self) -> PathBuf {
        self.store.events_path(CH).unwrap()
    }

    fn content(&self) -> String {
        fs::read_to_string(self.path()).unwrap()
    }

    /// A record a newer MUR would write at the tail: next `seq`, chained to
    /// the current last line, signed by the router over its own kind string.
    fn future_record(&self) -> serde_json::Value {
        self.future_record_linked_to(&chain::tail_hash(&self.content()))
    }

    /// [`Self::future_record`] signed over `prev` as its predecessor, so a
    /// wrong link carries a valid signature and only the chain can catch it.
    fn future_record_linked_to(&self, prev: &str) -> serde_json::Value {
        let content = self.content();
        let last: serde_json::Value =
            serde_json::from_str(content.lines().last().unwrap()).unwrap();
        let prev = prev.to_owned();
        let payload = serde_json::json!({ "future": true });
        let input = sign_input_v2_wire_kind(
            CH,
            &ChannelActor::System,
            FUTURE_KIND,
            &payload,
            None,
            &prev,
        );
        let sig = multibase::encode(multibase::Base::Base58Btc, self.id.sign_bytes(&input));
        serde_json::json!({
            "seq": last["seq"].as_u64().unwrap() + 1,
            "ts": Utc::now(),
            "actor": ChannelActor::System,
            "kind": FUTURE_KIND,
            "payload": payload,
            "sig": sig,
            "key_version": 0,
            "prev_hash": prev,
        })
    }

    fn push(&self, line: &str) {
        let mut f = OpenOptions::new().append(true).open(self.path()).unwrap();
        writeln!(f, "{line}").unwrap();
    }

    fn report(&self) -> DamageReport {
        self.store
            .load_events_with_damage(CH, &self.id.verifying_key_bytes(), false)
            .unwrap()
            .1
    }
}

#[test]
fn a_validly_signed_future_kind_record_is_a_note_not_damage() {
    let f = fx();
    f.push(&f.future_record().to_string()); // line 2
    let r = f.report();
    assert_eq!(r.newer_kind_lines, vec![2]);
    assert!(r.unparseable_lines.is_empty(), "{r:?}");
    assert!(r.unverified_newer_kind.is_empty(), "{r:?}");
    assert!(r.chain_breaks.is_empty(), "{r:?}");
    assert!(r.is_clean(), "{r:?}");

    // Its seq is still taken, and the next append chains over it.
    let future_seq = r.newer_kind_events[0].seq;
    let next = f.signed(3);
    assert_eq!(next.seq, future_seq + 1);
    assert!(f.report().is_clean());
}

/// Review P1 on #2349: an unknown kind with a signature that does not verify
/// must not pass, even as the last line.
#[test]
fn a_future_kind_tail_record_with_a_bad_signature_is_damage() {
    let f = fx();
    let mut rec = f.future_record();
    rec["payload"] = serde_json::json!({ "future": "forged" });
    f.push(&rec.to_string());
    let r = f.report();
    assert_eq!(r.newer_kind_lines, vec![2]);
    assert_eq!(r.unverified_newer_kind, r.newer_kind_events, "{r:?}");
    assert!(!r.is_clean());
}

/// Review P1 on #2349: re-labelling a signed event with an unknown kind
/// changes the signed bytes — it is a forgery, not a newer writer's line.
#[test]
fn relabelling_a_signed_event_with_an_unknown_kind_is_damage() {
    let f = fx();
    let mut rec = serde_json::to_value(f.signed(2)).unwrap();
    let mut lines: Vec<String> = f.content().lines().map(str::to_owned).collect();
    rec["kind"] = serde_json::json!(FUTURE_KIND);
    *lines.last_mut().unwrap() = rec.to_string();
    fs::write(f.path(), lines.join("\n") + "\n").unwrap();
    let r = f.report();
    assert_eq!(r.newer_kind_lines, vec![2]);
    assert_eq!(r.unverified_newer_kind.len(), 1, "{r:?}");
    assert!(!r.is_clean());
}

/// Review P1 on #2349: a validly signed future-kind tail record that names
/// the wrong predecessor is a broken link.
#[test]
fn a_future_kind_tail_record_with_a_broken_link_is_damage() {
    let f = fx();
    f.push(&f.future_record_linked_to(GENESIS_PREV_HASH).to_string());
    let r = f.report();
    assert_eq!(r.newer_kind_lines, vec![2]);
    assert!(
        r.unverified_newer_kind.is_empty(),
        "the signature holds: {r:?}"
    );
    assert_eq!(r.chain_breaks, vec![r.newer_kind_events[0].seq], "{r:?}");
    assert!(!r.is_clean());
}

/// An unknown kind does not excuse a broken envelope: a line missing a
/// required field (here `ts`) is corrupt, not a newer writer's.
#[test]
fn a_future_kind_with_a_malformed_envelope_is_corrupt() {
    let f = fx();
    let mut rec = f.future_record();
    rec.as_object_mut().unwrap().remove("ts");
    f.push(&rec.to_string());
    let mut rec = f.future_record();
    rec["actor"] = serde_json::json!(42);
    f.push(&rec.to_string());
    let r = f.report();
    assert!(r.newer_kind_lines.is_empty(), "{r:?}");
    assert_eq!(r.unparseable_lines, vec![2, 3]);
    assert!(!r.is_clean());
}

/// A line whose `kind` this binary DOES know but whose body fails to parse
/// is damage, not a newer writer — it must not hide as a note.
#[test]
fn known_kind_with_a_broken_body_is_corrupt_not_newer() {
    let f = fx();
    let mut rec = serde_json::to_value(f.signed(2)).unwrap();
    rec["seq"] = serde_json::json!(3);
    rec["actor"] = serde_json::json!(42);
    f.push(&rec.to_string());
    let r = f.report();
    assert!(r.newer_kind_lines.is_empty());
    assert_eq!(r.unparseable_lines, vec![3]);
    assert!(!r.is_clean());
}

/// Both classes side by side: the truncated line is damage, the valid
/// future-kind line only a note.
#[test]
fn newer_kind_and_corrupt_lines_are_told_apart() {
    let f = fx();
    f.push(&f.future_record().to_string()); // line 2
    f.push("{\"seq\": "); // line 3
    let r = f.report();
    assert_eq!(r.newer_kind_lines, vec![2]);
    assert_eq!(r.unparseable_lines, vec![3]);
    assert!(r.unverified_newer_kind.is_empty(), "{r:?}");
    assert!(!r.is_clean());
}

/// The wire-kind sign input is the typed one for every known kind, byte for
/// byte — otherwise routing verification through it would break every
/// signature already on disk.
#[test]
fn wire_kind_sign_input_matches_the_typed_one_for_known_kinds() {
    let actor = ChannelActor::Agent { id: "qa".into() };
    let p = serde_json::json!({ "text": "hi" });
    for kind in [
        EventKind::Message,
        EventKind::HitlResponse,
        EventKind::AskReply,
    ] {
        let wire = serde_json::to_value(kind).unwrap();
        assert_eq!(
            crate::sign::sign_input_v2(CH, &actor, kind, &p, Some("k"), "ph"),
            sign_input_v2_wire_kind(CH, &actor, wire.as_str().unwrap(), &p, Some("k"), "ph"),
        );
    }
}

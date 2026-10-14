//! The channel verifier (channel-log-integrity spec section 8). One test per
//! state of the 8.1 table, plus the per-line classes (8.2) and the `seq` hole
//! rules (8.4).

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use chrono::Utc;
use mur_channel::ChannelStore;
use mur_channel::head::{Head, HeadFields};
use mur_channel::head_io::{head_path, write_head};
use mur_channel::seal::{HEADS_DIR_NAME, HeadSealer};
use mur_channel::sign::sign_event;
use mur_channel::verify::{
    ChannelReport, HeadFault, IntegrityState, LineClass, SeqFaultKind, verify_channel,
};
use mur_common::channel::{Channel, ChannelActor, ChannelState, EventKind, Goal};
use mur_common::identity::AgentIdentity;
use tempfile::TempDir;

const CH: &str = "ver-ch";
const KV: u32 = 3;

fn human() -> ChannelActor {
    ChannelActor::Human { name: "me".into() }
}

struct Fx {
    tmp: TempDir,
    store: ChannelStore,
    id: Arc<AgentIdentity>,
}

fn fx() -> Fx {
    let tmp = TempDir::new().unwrap();
    let store = ChannelStore::new(tmp.path());
    let now = Utc::now();
    store
        .create(&Channel {
            v: mur_common::channel::CHANNEL_SCHEMA_VERSION,
            id: CH.into(),
            title: "t".into(),
            goal: Goal::default(),
            state: ChannelState::Working,
            purpose: None,
            owner: human(),
            participants: vec![],
            created_at: now,
            updated_at: now,
        })
        .unwrap();
    Fx {
        tmp,
        store,
        id: Arc::new(AgentIdentity::generate()),
    }
}

impl Fx {
    fn heads(&self) -> PathBuf {
        self.tmp.path().join(HEADS_DIR_NAME)
    }
    fn log(&self) -> PathBuf {
        self.store.events_path(CH)
    }
    fn head(&self) -> Head {
        mur_channel::head_io::read_head(&self.heads(), CH)
            .unwrap()
            .expect("a head")
    }
    /// An unsigned, unsealed append: how the Hub and the CLI write.
    fn plain(&self, text: &str) {
        self.append(text, None, None);
    }
    /// A router-signed, sealed append.
    fn sealed(&self, text: &str) {
        let payload = serde_json::json!({ "text": text });
        let sig = sign_event(&self.id, CH, &human(), EventKind::Message, &payload, None);
        self.append(text, Some(sig), Some(KV));
    }
    /// A sealed append whose signature is not the router's.
    fn sealed_by_other(&self, text: &str) {
        let other = AgentIdentity::generate();
        let payload = serde_json::json!({ "text": text });
        let sig = sign_event(&other, CH, &human(), EventKind::Message, &payload, None);
        self.append(text, Some(sig), Some(KV));
    }
    fn append(&self, text: &str, sig: Option<String>, kv: Option<u32>) {
        let sealer = HeadSealer::new(self.tmp.path(), self.id.clone(), KV);
        let hook = move |i: &mur_channel::store::SealInput<'_>| sealer.seal(i);
        let seal: Option<&mur_channel::store::SealHook<'_>> = sig.is_some().then_some(&hook);
        self.store
            .append_event_with(
                CH,
                human(),
                EventKind::Message,
                serde_json::json!({ "text": text }),
                None,
                sig,
                kv,
                seal,
            )
            .unwrap();
    }
    fn verify(&self) -> ChannelReport {
        let key = self.id.verifying_key_bytes();
        verify_channel(self.tmp.path(), CH, &|kv| (kv == Some(KV)).then_some(key)).unwrap()
    }
    fn lines(&self) -> Vec<String> {
        fs::read_to_string(self.log())
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect()
    }
    fn write_lines(&self, lines: &[String]) {
        fs::write(self.log(), lines.join("\n") + "\n").unwrap();
    }
}

#[test]
fn a_sealed_untouched_log_is_intact_and_its_lines_are_sealed() {
    let f = fx();
    f.sealed("a");
    f.sealed("b");
    let r = f.verify();
    assert_eq!(r.state, IntegrityState::Intact);
    assert!(!r.is_failure());
    assert!(r.lines.iter().all(|l| l.class == LineClass::Sealed));
    assert_eq!(r.lines.len(), 2);
}

#[test]
fn a_log_with_no_head_is_not_sealed_and_every_line_is_unsealed() {
    let f = fx();
    f.plain("a");
    f.plain("b");
    let r = f.verify();
    assert_eq!(r.state, IntegrityState::NoHead);
    assert!(!r.state.is_error(), "8.1: NoHead is not an error by itself");
    assert!(r.lines.iter().all(|l| l.class == LineClass::Unsealed));
}

#[test]
fn no_head_does_not_hide_a_seq_hole() {
    let f = fx();
    f.plain("a");
    f.plain("b");
    f.plain("c");
    let mut l = f.lines();
    l.remove(1);
    f.write_lines(&l);
    let r = f.verify();
    assert_eq!(r.state, IntegrityState::NoHead);
    assert_eq!(r.seq_findings.len(), 1);
    assert_eq!(r.seq_findings[0].kind, SeqFaultKind::Gap);
    assert!(
        r.is_failure(),
        "8.1: the continuity check runs under NoHead"
    );
}

#[test]
fn neither_log_nor_head_is_no_log() {
    let f = fx();
    let r = f.verify();
    assert_eq!(r.state, IntegrityState::NoLog);
    assert!(!r.is_failure());
}

#[test]
fn a_head_without_a_log_is_an_orphan_not_an_error() {
    let f = fx();
    f.sealed("a");
    fs::remove_file(f.log()).unwrap();
    let r = f.verify();
    assert_eq!(r.state, IntegrityState::LogMissing);
    assert!(!r.is_failure(), "8.1: report it, do not fail");
}

#[test]
fn a_head_signed_by_another_key_is_invalid() {
    let f = fx();
    f.sealed("a");
    let key = AgentIdentity::generate().verifying_key_bytes();
    let r = verify_channel(f.tmp.path(), CH, &|_| Some(key)).unwrap();
    assert_eq!(
        r.state,
        IntegrityState::HeadInvalid(HeadFault::BadSignature)
    );
    assert!(r.is_failure());
}

#[test]
fn a_head_for_another_channel_is_invalid() {
    let f = fx();
    f.sealed("a");
    let mut h = f.head();
    h.channel_id = "other".into();
    // Re-sign so only the channel mismatch is wrong, not the signature.
    let resigned = Head::seal(
        &f.id,
        HeadFields {
            channel_id: h.channel_id.clone(),
            seq: h.seq,
            lines: h.lines,
            chain_hash: h.chain_hash.clone(),
            key_version: h.key_version,
            legacy_through: h.legacy_through,
        },
        h.ts,
    );
    fs::write(
        head_path(&f.heads(), CH).unwrap(),
        serde_json::to_vec(&resigned).unwrap(),
    )
    .unwrap();
    assert_eq!(
        f.verify().state,
        IntegrityState::HeadInvalid(HeadFault::WrongChannel("other".into()))
    );
}

#[test]
fn an_unknown_head_version_is_invalid() {
    let f = fx();
    f.sealed("a");
    let mut h = f.head();
    h.v = 99;
    write_head(&f.heads(), &h).unwrap();
    assert_eq!(
        f.verify().state,
        IntegrityState::HeadInvalid(HeadFault::UnknownVersion(99))
    );
}

#[test]
fn an_unknown_key_version_is_invalid() {
    let f = fx();
    f.sealed("a");
    let r = verify_channel(f.tmp.path(), CH, &|_| None).unwrap();
    assert_eq!(
        r.state,
        IntegrityState::HeadInvalid(HeadFault::UnknownKeyVersion(Some(KV)))
    );
}

#[test]
fn a_head_that_is_not_json_is_invalid_and_trusts_nothing() {
    let f = fx();
    f.sealed("a");
    fs::write(head_path(&f.heads(), CH).unwrap(), b"not a head").unwrap();
    let r = f.verify();
    assert!(matches!(
        r.state,
        IntegrityState::HeadInvalid(HeadFault::Unparseable(_))
    ));
    assert!(r.lines.iter().all(|l| l.class == LineClass::Unsealed));
}

#[test]
fn a_deleted_line_below_the_head_is_a_gap_named_by_line() {
    let f = fx();
    f.sealed("a");
    f.sealed("b");
    f.sealed("c");
    let mut l = f.lines();
    l.remove(1);
    f.write_lines(&l);
    let r = f.verify();
    assert_eq!(r.state, IntegrityState::Gap, "Gap names the place");
    assert_eq!(r.seq_findings[0].line_no, 2);
    assert_eq!((r.seq_findings[0].prev_seq, r.seq_findings[0].seq), (0, 2));
}

#[test]
fn a_repeated_seq_is_its_own_finding() {
    let f = fx();
    f.sealed("a");
    f.sealed("b");
    let mut l = f.lines();
    l.insert(1, l[0].clone());
    f.write_lines(&l);
    assert_eq!(f.verify().state, IntegrityState::DuplicateSeq);
}

#[test]
fn a_seq_that_goes_backwards_is_reordered() {
    let f = fx();
    f.sealed("a");
    f.sealed("b");
    f.sealed("c");
    let mut l = f.lines();
    let first = l.remove(0);
    l.push(first);
    f.write_lines(&l);
    assert_eq!(f.verify().state, IntegrityState::Reordered);
}

#[test]
fn losing_the_tail_of_a_sealed_log_is_tail_truncated() {
    let f = fx();
    f.sealed("a");
    f.sealed("b");
    f.sealed("c");
    let mut l = f.lines();
    l.pop();
    f.write_lines(&l);
    assert_eq!(f.verify().state, IntegrityState::TailTruncated);
}

#[test]
fn a_log_shorter_than_the_head_claims_is_truncated_even_with_seq_intact() {
    // A trailing unparseable line counts in `lines` but carries no `seq`:
    // losing it leaves `seq` fine and only `lines` shows it (8.4).
    let f = fx();
    f.sealed("a");
    let mut l = f.lines();
    l.push("{torn".into());
    f.write_lines(&l);
    f.sealed("b"); // next seal chains the torn line too
    assert_eq!(f.verify().state, IntegrityState::Intact);
    let mut l = f.lines();
    l.remove(1); // drop the torn line
    f.write_lines(&l);
    assert_eq!(f.verify().state, IntegrityState::TailTruncated);
}

#[test]
fn editing_a_sealed_line_in_place_breaks_the_chain() {
    let f = fx();
    f.sealed("a");
    f.sealed("b");
    let mut l = f.lines();
    l[0] = l[0].replace("\"a\"", "\"A\"");
    f.write_lines(&l);
    let r = f.verify();
    assert_eq!(r.state, IntegrityState::ChainBroken { lines: 2 });
    assert!(r.is_failure());
}

#[test]
fn lines_after_the_head_are_unsealed_and_the_log_is_still_intact() {
    let f = fx();
    f.sealed("a");
    f.plain("hub line");
    let r = f.verify();
    assert_eq!(r.state, IntegrityState::Intact);
    assert_eq!(r.lines[0].class, LineClass::Sealed);
    assert_eq!(r.lines[1].class, LineClass::Unsealed);
}

#[test]
fn a_sealed_line_the_router_did_not_sign_is_sealed_unsigned() {
    let f = fx();
    f.sealed("a");
    f.sealed_by_other("b");
    let r = f.verify();
    assert_eq!(r.lines[0].class, LineClass::Sealed);
    assert_eq!(r.lines[1].class, LineClass::SealedUnsigned);
}

#[test]
fn an_unsigned_line_swept_into_a_seal_is_sealed_unsigned() {
    let f = fx();
    f.sealed("a");
    f.plain("hub");
    f.sealed("c");
    let r = f.verify();
    assert_eq!(r.lines[1].class, LineClass::SealedUnsigned);
}

#[test]
fn lines_present_at_first_seal_are_legacy() {
    let f = fx();
    f.plain("old 0");
    f.plain("old 1");
    f.sealed("router");
    let r = f.verify();
    assert_eq!(r.state, IntegrityState::Intact);
    let classes: Vec<_> = r.lines.iter().map(|l| l.class).collect();
    assert_eq!(
        classes,
        [LineClass::Legacy, LineClass::Legacy, LineClass::Sealed]
    );
}

#[test]
fn a_hole_inside_the_legacy_region_is_a_warning_not_an_error() {
    let f = fx();
    f.plain("old 0");
    f.plain("old 1");
    f.plain("old 2");
    let mut l = f.lines();
    l.remove(1); // the hole predates the first seal
    f.write_lines(&l);
    f.sealed("router");
    let r = f.verify();
    assert_eq!(r.state, IntegrityState::Intact, "sealed as found");
    assert_eq!(r.seq_findings.len(), 1);
    assert!(r.seq_findings[0].legacy);
    assert!(!r.is_failure());
}

#[test]
fn an_unparseable_line_is_listed_by_file_line_and_replacing_it_breaks_the_chain() {
    let f = fx();
    f.plain("old 0");
    let mut l = f.lines();
    l.push("{not json".into());
    f.write_lines(&l);
    f.sealed("router");
    let r = f.verify();
    assert_eq!(r.unparseable_lines, [2]);
    assert_eq!(r.state, IntegrityState::Intact);

    let mut l = f.lines();
    l[1] = "{different junk".into();
    f.write_lines(&l);
    assert_eq!(
        f.verify().state,
        IntegrityState::ChainBroken { lines: 3 },
        "A1: raw bytes are in the chain"
    );
}

#[test]
fn an_unterminated_tail_is_reported_and_changes_nothing() {
    let f = fx();
    f.sealed("a");
    let mut bytes = fs::read(f.log()).unwrap();
    bytes.extend_from_slice(b"{\"seq\":1,");
    fs::write(f.log(), bytes).unwrap();
    let r = f.verify();
    assert_eq!(r.state, IntegrityState::Intact);
    assert_eq!(r.unterminated_tail_bytes, 9);
    assert_eq!(r.lines.len(), 1, "the tail is not a line");
}

#[test]
fn verifying_writes_nothing() {
    let f = fx();
    f.sealed("a");
    let before = (
        fs::read(f.log()).unwrap(),
        fs::read(head_path(&f.heads(), CH).unwrap()).unwrap(),
    );
    f.verify();
    let after = (
        fs::read(f.log()).unwrap(),
        fs::read(head_path(&f.heads(), CH).unwrap()).unwrap(),
    );
    assert_eq!(before, after);
}

#[test]
fn a_head_with_a_bad_signature_seals_nothing_and_its_legacy_claim_is_ignored() {
    // The head is evidence, not a trust anchor. If a forged head's
    // `legacy_through` were honoured, raising it would downgrade lines to
    // `Legacy` and hide a hole from the error count.
    let f = fx();
    f.plain("old 0");
    f.sealed("router");
    let key = AgentIdentity::generate().verifying_key_bytes();
    let r = verify_channel(f.tmp.path(), CH, &|_| Some(key)).unwrap();
    assert!(matches!(r.state, IntegrityState::HeadInvalid(_)));
    assert!(
        r.lines.iter().all(|l| l.class == LineClass::Unsealed),
        "no line is sealed by an untrusted head: {:?}",
        r.lines
    );
}

#[test]
fn a_hole_is_not_excused_by_a_forged_legacy_through() {
    let f = fx();
    f.plain("old 0");
    f.plain("old 1");
    f.plain("old 2");
    let mut l = f.lines();
    l.remove(1);
    f.write_lines(&l);
    f.sealed("router"); // honest head: legacy_through = 2
    // Same head, signed by someone else, so it must not excuse the hole.
    let key = AgentIdentity::generate().verifying_key_bytes();
    let r = verify_channel(f.tmp.path(), CH, &|_| Some(key)).unwrap();
    assert!(
        r.seq_findings.iter().all(|s| !s.legacy),
        "a legacy excuse from an untrusted head must not apply"
    );
    assert!(r.is_failure());
}

#[test]
fn a_line_appended_after_the_seal_is_unsealed_even_if_it_reuses_an_old_seq() {
    // Chain membership is by line position, which is what the chain commits
    // to. A copy of an early line appended later has `seq <= head.seq` but is
    // not in the chain, so it must not be reported as sealed.
    let f = fx();
    f.sealed("a");
    f.sealed("b");
    let mut l = f.lines();
    l.push(l[0].clone()); // a third line carrying seq 0, after head.seq = 1
    f.write_lines(&l);
    let r = f.verify();
    // seq goes 0, 1, 0: backwards, so Reordered (not DuplicateSeq).
    assert_eq!(r.state, IntegrityState::Reordered);
    let last = r.lines.last().unwrap();
    assert_eq!(last.line_no, 3);
    assert_eq!(last.class, LineClass::Unsealed);
}

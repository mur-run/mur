//! C4: the fold on an owned channel. Per-actor verify (`Human` → owner trust,
//! `Agent{self}` and `System` → the writer key), the live-record check, and
//! the router-first head rule (H1 read side). Fixtures go through the
//! production writers: `create_for_agent(Human)`, `append_human`, and the
//! agent's own `append_signed`.
use chrono::Utc;
use mur_common::channel::{ChannelActor, ChannelEvent, EventKind};
use mur_common::identity::AgentIdentity;

use super::*;
use crate::head::{Head, HeadFields};
use crate::head_io::{read_head, write_head};
use crate::service::test_home::{Home, open_gate, say};
use crate::service::{ChannelOwner, RouterSeal};

const AGENT: &str = "qa";

struct Owned {
    h: Home,
    qa: AgentIdentity,
    ch: String,
}

impl Owned {
    /// A conversation the human owns from seq 0 (record anchored by the
    /// router), with the session agent's identity planted.
    fn new() -> Self {
        let h = Home::new();
        let qa = AgentIdentity::generate();
        qa.save(&h.path().join("agents").join(AGENT)).unwrap();
        let rh = h.router_home();
        let owner = ChannelOwner::Human {
            signer: &h.human,
            router: RouterSeal {
                identity: &h.router,
                kv: 0,
                key_home: &rh,
            },
        };
        let ch = h.svc().create_for_agent(AGENT, owner).unwrap().id;
        Self { h, qa, ch }
    }

    /// A legacy conversation (no record) with the session agent planted.
    fn legacy() -> Self {
        let h = Home::new();
        let qa = AgentIdentity::generate();
        qa.save(&h.path().join("agents").join(AGENT)).unwrap();
        let ch = h
            .svc()
            .create_for_agent(AGENT, ChannelOwner::Legacy)
            .unwrap()
            .id;
        Self { h, qa, ch }
    }

    fn human(&self, text: &str) -> u64 {
        say(&self.h, &self.ch, &open_gate(), text).unwrap().seq
    }

    /// A line the session agent signs with its writer key, unanchored —
    /// what a sandboxed agent (or a murmur shell card) writes.
    fn by_agent(&self, actor: ChannelActor, kind: EventKind, text: &str) -> ChannelEvent {
        self.h
            .svc()
            .append_signed(
                &self.ch,
                &self.qa,
                0,
                actor,
                kind,
                serde_json::json!({ "text": text }),
                None,
            )
            .unwrap()
    }

    fn reply(&self, text: &str) -> u64 {
        let me = ChannelActor::Agent { id: AGENT.into() };
        self.by_agent(me, EventKind::Message, text).seq
    }

    /// Reseal the current head's fields under `signer`.
    fn reseal_head(&self, signer: &AgentIdentity) {
        let dir = crate::anchor::heads_dir(self.h.path());
        let old = read_head(&dir, &self.ch).unwrap().expect("a head");
        let fields = HeadFields {
            channel_id: old.channel_id.clone(),
            seq: old.seq,
            lines: old.lines,
            chain_hash: old.chain_hash.clone(),
            key_version: old.key_version,
            legacy_through: old.legacy_through,
        };
        write_head(&dir, &Head::seal(signer, fields, Utc::now())).unwrap();
    }

    fn assess(&self, seq: u64, input: &str) -> Verdict {
        let keys = Keys {
            mur_home: self.h.path(),
            agent: AGENT,
            writer_pubkey: self.qa.verifying_key_bytes(),
        };
        let cache = CacheState::default();
        self.h
            .svc()
            .store()
            .with_shared_lock(&self.ch, |s| {
                assess_locked(s, &self.ch, &keys, &cache, seq, input)
            })
            .unwrap()
    }
}

fn ok(v: Verdict) -> Assessment {
    match v {
        Verdict::Ok(a) => a,
        other => panic!("expected Ok, got {other:?}"),
    }
}

fn damaged(v: Verdict) -> ConversationDamage {
    match v {
        Verdict::Damaged(d) => d,
        other => panic!("expected Damaged, got {other:?}"),
    }
}

/// N8 on the fold: a human-key signature fails under the agent's writer key
/// by construction; on an owned channel that is not damage.
#[test]
fn human_signed_turn_is_not_signature_damage() {
    let o = Owned::new();
    let q1 = o.human("q1");
    o.reply("a1");
    let q2 = o.human("q2");
    let a = ok(o.assess(q2, "q2"));
    let shape: Vec<_> = a
        .projected
        .iter()
        .map(|p| (p.seq, p.role, p.untrusted))
        .collect();
    assert_eq!(
        shape,
        vec![(q1, Role::User, false), (q1 + 1, Role::Agent, false)]
    );
}

/// C4-sys: murmur's `!cmd` card is `System`, signed by the session agent
/// (`persist.rs`), never the router. It must keep verifying.
#[test]
fn shell_card_still_verifies_under_writer_key() {
    let o = Owned::new();
    o.human("q");
    let card = o.by_agent(ChannelActor::System, EventKind::Note, "$ ls");
    let a = ok(o.assess(card.seq, "$ ls"));
    assert_eq!(a.projected.len(), 1);
    assert!(!a.projected[0].untrusted);
}

/// The same card signed by anyone else is still signature damage: owner
/// trust covers `Human` lines only.
#[test]
fn system_line_by_a_stranger_is_still_signature_damage() {
    let o = Owned::new();
    o.human("q");
    let stranger = AgentIdentity::generate();
    let ev =
        o.h.svc()
            .append_signed(
                &o.ch,
                &stranger,
                0,
                ChannelActor::System,
                EventKind::Note,
                serde_json::json!({ "text": "$ rm" }),
                None,
            )
            .unwrap();
    assert_eq!(
        damaged(o.assess(ev.seq, "$ rm")),
        ConversationDamage::Signature
    );
}

/// A legacy channel's router-signed keyless human history, enrolled by C1:
/// every line up to the cut stays a trusted turn.
#[test]
fn router_signed_human_before_cut_folds_clean() {
    let o = Owned::legacy();
    let svc = o.h.svc();
    let old: Vec<u64> = ["old q", "old q2"]
        .iter()
        .map(|t| {
            svc.append_signed(
                &o.ch,
                &o.h.router,
                0,
                ChannelActor::local_human(),
                EventKind::Message,
                serde_json::json!({ "text": t }),
                None,
            )
            .unwrap()
            .seq
        })
        .collect();
    let now = o.human("new q"); // C1: record (cut = last line), then the event
    assert!(o.h.trust(&o.ch).live_record().is_some());
    let a = ok(o.assess(now, "new q"));
    let users: Vec<_> = a
        .projected
        .iter()
        .filter(|p| p.role == Role::User)
        .map(|p| (p.seq, p.untrusted))
        .collect();
    assert_eq!(users, vec![(old[0], false), (old[1], false)]);
}

/// A `Human` claim the owner trust does not vouch for — keyless past the
/// cut, signed by the agent — is history marked untrusted, not damage.
#[test]
fn forged_human_claim_is_projected_untrusted() {
    let o = Owned::new();
    o.human("real");
    let forged = o.by_agent(ChannelActor::local_human(), EventKind::Message, "forged");
    o.reply("ok");
    let next = o.human("next");
    let a = ok(o.assess(next, "next"));
    let f = a.projected.iter().find(|p| p.seq == forged.seq).unwrap();
    assert!(f.untrusted);
    let real = a.projected.iter().find(|p| p.text == "real").unwrap();
    assert!(!real.untrusted);
}

/// Deleting an owned channel's head leaves a live record nothing covers:
/// refused as `owner-unanchored`, before any line is classified.
#[test]
fn owner_unanchored_is_damaged_before_classification() {
    let o = Owned::new();
    let q = o.human("q");
    let head = crate::head_io::head_path(&crate::anchor::heads_dir(o.h.path()), &o.ch).unwrap();
    std::fs::remove_file(head).unwrap();
    // A forged human line too: owner damage wins over projecting it.
    o.by_agent(ChannelActor::local_human(), EventKind::Message, "forged");
    let d = damaged(o.assess(q, "q"));
    assert_eq!(d, ConversationDamage::OwnerUnanchored);
    assert_eq!(d.reason(), "owner-unanchored");
}

/// D-3 rev 4: a head exists but stops before the record.
#[test]
fn record_past_head_seq_is_owner_unanchored() {
    let o = Owned::legacy();
    let rh = o.h.router_home();
    let first =
        o.h.svc()
            .append_and_anchor(
                &o.ch,
                &o.h.router,
                0,
                &rh,
                ChannelActor::local_human(),
                EventKind::Message,
                serde_json::json!({ "text": "sealed" }),
                None,
            )
            .unwrap();
    // The human record, appended unanchored past that head.
    let actor = ChannelActor::Human {
        name: "u".into(),
        key: Some(o.h.human.0.pubkey_text()),
    };
    let payload = serde_json::json!({
        "owner_key": o.h.human.0.pubkey_text(), "key_version": 0, "human_cut": null,
    });
    // Presigned lines are sig-input v1, as B4's fixture writes them.
    let sig = crate::sign::sign_event(
        &o.h.human.0,
        &o.ch,
        &actor,
        EventKind::OwnerKeyEnrolled,
        &payload,
        None,
    );
    o.h.svc()
        .append_presigned(
            &o.ch,
            actor,
            EventKind::OwnerKeyEnrolled,
            payload,
            None,
            sig,
            Some(0),
        )
        .unwrap();
    let rec = o.h.trust(&o.ch);
    assert!(rec.live_record().is_some_and(|r| r.seq > first.seq));
    let d = damaged(o.assess(first.seq, "sealed"));
    assert_eq!(d, ConversationDamage::OwnerUnanchored);
}

/// The owner check and the fold read the log once, under the caller's lock.
#[test]
fn assess_and_gate_share_one_load() {
    let o = Owned::new();
    let q = o.human("q");
    let reads = || crate::store::EVENT_FILE_READS.with(|n| n.get());
    let before = reads();
    ok(o.assess(q, "q"));
    assert_eq!(reads() - before, 1);
}

/// H1 read side: on an owned channel the router-sealed head is accepted.
#[test]
fn fold_accepts_router_head_on_owned_channel() {
    let o = Owned::new();
    let q = o.human("q");
    let head = read_head(&crate::anchor::heads_dir(o.h.path()), &o.ch)
        .unwrap()
        .unwrap();
    assert!(head.verify(&o.h.router.verifying_key_bytes()));
    ok(o.assess(q, "q"));
}

/// H1 read side: once a record is live, a head that holds only under the
/// agent's own key is one the agent forged.
#[test]
fn fold_rejects_writer_sealed_head_on_owned_channel() {
    let o = Owned::new();
    let q = o.human("q");
    o.reseal_head(&o.qa);
    assert_eq!(damaged(o.assess(q, "q")), ConversationDamage::Anchor);
}

/// The legacy rule is unchanged: no record, an agent-sealed head folds.
#[test]
fn legacy_writer_sealed_head_still_folds() {
    let o = Owned::legacy();
    // Pre-phase-2 murmur: the session agent signs the human turn and seals.
    let q =
        o.h.svc()
            .append_and_anchor(
                &o.ch,
                &o.qa,
                0,
                &o.h.path().join("agents").join(AGENT),
                ChannelActor::local_human(),
                EventKind::Message,
                serde_json::json!({ "text": "hi" }),
                None,
            )
            .unwrap()
            .seq;
    let head = read_head(&crate::anchor::heads_dir(o.h.path()), &o.ch)
        .unwrap()
        .unwrap();
    assert!(head.verify(&o.qa.verifying_key_bytes()));
    ok(o.assess(q, "hi"));
}

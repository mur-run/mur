//! B2: provenance of keyed `Human` events and channel-level owner trust.

use std::fs;
use std::path::Path;

use mur_common::channel::{ChannelActor, ChannelEvent, EventKind, HumanCut};
use mur_common::identity::human::{human_identity_dir, human_key_dir, human_rotations_path};
use mur_common::identity::{AgentIdentity, RotationAttestation, RotationReason};

use super::*;
use crate::head::{Head, HeadFields};

const CH: &str = "ch-b2";

fn attest(old: Option<&AgentIdentity>, new: &AgentIdentity, kv: u32) -> RotationAttestation {
    let a = RotationAttestation::new(
        "u",
        old.map(|o| o.pubkey_text()).unwrap_or_default(),
        new.pubkey_text(),
        kv.saturating_sub(1),
        kv,
        "2026-11-09T00:00:00Z",
        RotationReason::Scheduled,
    );
    match old {
        None => a.into_bootstrap(),
        Some(o) => {
            let mut a = a;
            a.sign(o.signing_key());
            a
        }
    }
}

/// Enroll a human chain whose keys are `keys` (bootstrap first); the last is
/// the head and is written to `identity.pub`.
fn enroll(mur: &Path, keys: &[&AgentIdentity]) {
    let mut chain = Vec::new();
    for (i, k) in keys.iter().enumerate() {
        chain.push(attest(i.checked_sub(1).map(|j| keys[j]), k, i as u32));
    }
    keys.last()
        .unwrap()
        .save_split(&human_identity_dir(mur), &human_key_dir(mur))
        .unwrap();
    let body: String = chain
        .iter()
        .map(|a| format!("{}\n", serde_json::to_string(a).unwrap()))
        .collect();
    fs::write(human_rotations_path(mur), body).unwrap();
}

fn router(mur: &Path) -> AgentIdentity {
    let r = AgentIdentity::from_secret_bytes(&[3u8; 32]);
    r.save(&mur.join("agents").join(super::super::CONTROL_PLANE_AGENT))
        .unwrap();
    r
}

fn event(
    seq: u64,
    signer: &AgentIdentity,
    actor: ChannelActor,
    kind: EventKind,
    payload: serde_json::Value,
    kv: Option<u32>,
) -> ChannelEvent {
    let sig = crate::sign::sign_event(signer, CH, &actor, kind, &payload, None);
    ChannelEvent {
        seq,
        ts: chrono::Utc::now(),
        actor,
        kind,
        payload,
        idempotency_key: None,
        sig: Some(sig),
        key_version: kv,
        prev_hash: None,
    }
}

fn keyed(id: &AgentIdentity) -> ChannelActor {
    ChannelActor::Human {
        name: "u".into(),
        key: Some(id.pubkey_text()),
    }
}

fn keyless() -> ChannelActor {
    ChannelActor::Human {
        name: "u".into(),
        key: None,
    }
}

fn msg(seq: u64, signer: &AgentIdentity, actor: ChannelActor, kv: Option<u32>) -> ChannelEvent {
    event(
        seq,
        signer,
        actor,
        EventKind::Message,
        serde_json::json!({"text": "hi"}),
        kv,
    )
}

fn record(seq: u64, human: &AgentIdentity, kv: u32, cut: Option<HumanCut>) -> ChannelEvent {
    let payload = serde_json::json!({
        "owner_key": human.pubkey_text(), "key_version": kv, "human_cut": cut,
    });
    event(
        seq,
        human,
        keyed(human),
        EventKind::OwnerKeyEnrolled,
        payload,
        Some(kv),
    )
}

fn log(events: &[ChannelEvent]) -> String {
    events
        .iter()
        .map(|e| format!("{}\n", serde_json::to_string(e).unwrap()))
        .collect()
}

fn cut_at(events: &[ChannelEvent], seq: u64) -> HumanCut {
    let line = serde_json::to_string(events.iter().find(|e| e.seq == seq).unwrap()).unwrap();
    HumanCut {
        seq,
        line_hash: crate::store::line_hash(&line),
    }
}

fn head(r: &AgentIdentity, seq: u64) -> Head {
    Head::seal(
        r,
        HeadFields {
            channel_id: CH.into(),
            seq,
            lines: seq + 1,
            chain_hash: "00".into(),
            key_version: Some(0),
            legacy_through: None,
        },
        chrono::Utc::now(),
    )
}

#[test]
fn keyed_human_verifies_on_local_chain() {
    let tmp = tempfile::tempdir().unwrap();
    let h = AgentIdentity::generate();
    enroll(tmp.path(), &[&h]);
    let ev = msg(0, &h, keyed(&h), Some(0));
    assert!(super::super::verify_event(tmp.path(), CH, &ev, true));
}

#[test]
fn keyed_human_unknown_key_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let h = AgentIdentity::generate();
    enroll(tmp.path(), &[&h]);
    let stranger = AgentIdentity::generate();
    let ev = msg(0, &stranger, keyed(&stranger), Some(0));
    assert!(!super::super::verify_event(tmp.path(), CH, &ev, false));
}

#[test]
fn old_attested_version_verifies_provenance() {
    let tmp = tempfile::tempdir().unwrap();
    let (h0, h1) = (AgentIdentity::generate(), AgentIdentity::generate());
    enroll(tmp.path(), &[&h0, &h1]);
    let old = msg(0, &h0, keyed(&h0), Some(0));
    assert!(super::super::verify_event(tmp.path(), CH, &old, true));
    // The same old key claiming the head's version is not the head's key.
    let lying = msg(1, &h0, keyed(&h0), Some(1));
    assert!(!super::super::verify_event(tmp.path(), CH, &lying, false));
}

/// A version only an agent-style disk fallback would know is not on the
/// human chain, and nothing falls back for it.
#[test]
fn fallback_only_version_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let h = AgentIdentity::generate();
    enroll(tmp.path(), &[&h]);
    let ev = msg(0, &h, keyed(&h), Some(7));
    assert!(!super::super::verify_event(tmp.path(), CH, &ev, false));
    // And with no enrolled chain at all, a keyed human resolves nothing.
    let bare = tempfile::tempdir().unwrap();
    router(bare.path());
    assert!(!super::super::verify_event(
        bare.path(),
        CH,
        &msg(0, &h, keyed(&h), None),
        false
    ));
}

#[test]
fn keyless_human_after_cut_fails_even_require_sig_false() {
    let tmp = tempfile::tempdir().unwrap();
    let r = router(tmp.path());
    let h = AgentIdentity::generate();
    enroll(tmp.path(), &[&h]);
    let before = msg(0, &r, keyless(), Some(0));
    let cut = cut_at(std::slice::from_ref(&before), 0);
    let events = vec![
        before.clone(),
        record(1, &h, 0, Some(cut)),
        msg(2, &r, keyless(), Some(0)),
    ];
    let t = HumanTrust::from_log(tmp.path(), CH, &log(&events), Some(&head(&r, 2)));
    assert!(t.live_record().is_some());
    assert!(t.damage().is_empty(), "{:?}", t.damage());
    assert!(t.is_trusted_human(tmp.path(), CH, &before, false));
    assert!(!t.is_trusted_human(tmp.path(), CH, &events[2], false));
    // Without a live record the same line is today's rule: router-signed passes.
    let legacy = HumanTrust::from_log(tmp.path(), CH, &log(&events[2..]), None);
    assert!(legacy.is_trusted_human(tmp.path(), CH, &events[2], false));
}

#[test]
fn foreign_record_ignored_not_fatal() {
    let tmp = tempfile::tempdir().unwrap();
    let r = router(tmp.path());
    let h = AgentIdentity::generate();
    enroll(tmp.path(), &[&h]);
    let foreign = AgentIdentity::generate();
    let events = vec![record(0, &foreign, 0, None), msg(1, &r, keyless(), Some(0))];
    let t = HumanTrust::from_log(tmp.path(), CH, &log(&events), Some(&head(&r, 1)));
    assert!(t.live_record().is_none());
    assert_eq!(t.ignored_records(), &[0]);
    assert!(t.damage().is_empty());
    assert!(t.is_trusted_human(tmp.path(), CH, &events[1], false));
}

#[test]
fn first_valid_record_wins_later_warned() {
    let tmp = tempfile::tempdir().unwrap();
    let r = router(tmp.path());
    let h = AgentIdentity::generate();
    enroll(tmp.path(), &[&h]);
    let events = vec![record(0, &h, 0, None), record(1, &h, 0, None)];
    let t = HumanTrust::from_log(tmp.path(), CH, &log(&events), Some(&head(&r, 1)));
    assert_eq!(t.live_record().map(|l| l.seq), Some(0));
    assert_eq!(t.later_records(), &[1]);
}

#[test]
fn cut_line_hash_mismatch_is_damaged() {
    let tmp = tempfile::tempdir().unwrap();
    let r = router(tmp.path());
    let h = AgentIdentity::generate();
    enroll(tmp.path(), &[&h]);
    let bad = HumanCut {
        seq: 0,
        line_hash: "ff".into(),
    };
    let events = vec![msg(0, &r, keyless(), Some(0)), record(1, &h, 0, Some(bad))];
    let t = HumanTrust::from_log(tmp.path(), CH, &log(&events), Some(&head(&r, 1)));
    assert_eq!(t.damage(), &[OwnerDamage::CutLineHashMismatch]);
}

#[test]
fn cut_beyond_head_is_damaged() {
    let tmp = tempfile::tempdir().unwrap();
    let r = router(tmp.path());
    let h = AgentIdentity::generate();
    enroll(tmp.path(), &[&h]);
    let mut events = vec![record(0, &h, 0, None), msg(1, &r, keyless(), Some(0))];
    let cut = cut_at(&events, 1);
    events[0] = record(0, &h, 0, Some(cut));
    let t = HumanTrust::from_log(tmp.path(), CH, &log(&events), Some(&head(&r, 0)));
    assert_eq!(t.damage(), &[OwnerDamage::CutBeyondHead]);
    // And a record with no head at all is unanchored.
    let none = HumanTrust::from_log(tmp.path(), CH, &log(&events[..1]), None);
    assert!(none.damage().contains(&OwnerDamage::Unanchored));
}

/// An agent called `identity` keys to `agents/identity`, never to the
/// human's `identity/`; and the human key dir's name is not an agent.
#[test]
fn agent_named_identity_cannot_reach_human_dir() {
    let home = Path::new("/m");
    let as_agent = ChannelActor::Agent {
        id: "identity".into(),
    };
    assert_eq!(
        super::super::actor_key_dir(home, &as_agent),
        Some(home.join("agents").join("identity"))
    );
    let dot_human = ChannelActor::Agent {
        id: ".human".into(),
    };
    assert_eq!(super::super::actor_key_dir(home, &dot_human), None);
}

/// Spec §2.3: a keyed human is trusted only when the channel has a live
/// owner record. A message that verifies on the local chain is provenance,
/// not enrollment; the legacy fallback is for keyless humans only.
#[test]
fn keyed_human_without_live_record_is_untrusted() {
    let tmp = tempfile::tempdir().unwrap();
    router(tmp.path());
    let h = AgentIdentity::generate();
    enroll(tmp.path(), &[&h]);
    let ev = msg(0, &h, keyed(&h), Some(0));
    // Provenance holds on its own...
    assert!(super::super::verify_event(tmp.path(), CH, &ev, true));
    // ...but an unenrolled channel does not trust it.
    let t = HumanTrust::from_log(tmp.path(), CH, &log(std::slice::from_ref(&ev)), None);
    assert!(t.live_record().is_none());
    assert!(!t.is_trusted_human(tmp.path(), CH, &ev, false));
    assert!(!t.is_trusted_human(tmp.path(), CH, &ev, true));
}

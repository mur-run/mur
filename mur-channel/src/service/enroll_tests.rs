//! C1: an owner record on a legacy channel's first human write.

use std::cell::Cell;
use std::fs;

use mur_common::channel::{
    CHANNEL_SCHEMA_VERSION, ChannelActor, EventKind, OwnerKeyEnrolledPayload,
};
use mur_common::identity::human::{human_identity_dir, human_key_dir, human_rotations_path};
use mur_common::identity::{AgentIdentity, RotationAttestation, RotationReason};

use super::super::test_home::*;
use super::*;
use crate::head_io::read_head;
use crate::store::line_hash;

#[test]
fn first_human_append_writes_v3_record_event_in_order() {
    let h = Home::new();
    let id = h.legacy_channel(0);
    say(&h, &id, &open_gate(), "hi").unwrap();

    let evs = h.svc().load_events(&id).unwrap();
    let kinds: Vec<_> = evs.iter().map(|e| e.kind).collect();
    assert_eq!(kinds, [EventKind::OwnerKeyEnrolled, EventKind::Message]);
    assert_eq!(
        h.svc().store().load_manifest(&id).unwrap().v,
        CHANNEL_SCHEMA_VERSION
    );
    // Both lines are covered by one router-sealed head.
    let head = read_head(&crate::anchor::heads_dir(h.path()), &id)
        .unwrap()
        .unwrap();
    assert_eq!(head.seq, 1);
    assert!(head.verify(&h.router.verifying_key_bytes()));
    assert_eq!(h.trust(&id).live_record().map(|r| r.seq), Some(0));
    assert!(h.trust(&id).damage().is_empty());
}

#[test]
fn cut_is_prior_last_line() {
    let h = Home::new();
    let id = h.legacy_channel(3);
    let before = h.log(&id);
    let last = before.lines().last().unwrap().to_owned();
    say(&h, &id, &open_gate(), "hi").unwrap();

    let rec = &h.svc().load_events(&id).unwrap()[3];
    assert_eq!(rec.kind, EventKind::OwnerKeyEnrolled);
    let p: OwnerKeyEnrolledPayload = serde_json::from_value(rec.payload.clone()).unwrap();
    let cut = p.human_cut.expect("a non-empty log has a cut");
    assert_eq!(cut.seq, 2);
    assert_eq!(cut.line_hash, line_hash(&last));
    // The prior lines are untouched.
    assert!(h.log(&id).starts_with(&before));
}

#[test]
fn empty_log_has_no_cut() {
    let h = Home::new();
    let id = h.legacy_channel(0);
    say(&h, &id, &open_gate(), "hi").unwrap();
    let rec = &h.svc().load_events(&id).unwrap()[0];
    let p: OwnerKeyEnrolledPayload = serde_json::from_value(rec.payload.clone()).unwrap();
    assert_eq!(p.human_cut, None);
}

#[test]
fn second_append_writes_no_second_record() {
    let h = Home::new();
    let id = h.legacy_channel(1);
    say(&h, &id, &open_gate(), "one").unwrap();
    say(&h, &id, &open_gate(), "two").unwrap();
    let records = h
        .svc()
        .load_events(&id)
        .unwrap()
        .iter()
        .filter(|e| e.kind == EventKind::OwnerKeyEnrolled)
        .count();
    assert_eq!(records, 1);
}

#[test]
fn damaged_log_is_not_enrolled() {
    let h = Home::new();
    let id = h.legacy_channel(2);
    let before = h.log(&id);
    let gate = EnrollGate {
        identity_protected: true,
        log_intact: &|| Ok(false),
        legacy_sealer: None,
    };
    let err = say(&h, &id, &gate, "hi").unwrap_err();
    assert!(format!("{err:#}").contains("damaged"), "{err:#}");
    assert_eq!(h.log(&id), before, "nothing was written");
    assert!(
        read_head(&crate::anchor::heads_dir(h.path()), &id)
            .unwrap()
            .is_none()
    );
}

#[test]
fn unprotected_identity_dir_refuses_enroll() {
    let h = Home::new();
    let id = h.legacy_channel(1);
    let before = h.log(&id);
    let gate = EnrollGate {
        identity_protected: false,
        log_intact: &|| Ok(true),
        legacy_sealer: None,
    };
    let err = say(&h, &id, &gate, "hi").unwrap_err();
    assert!(format!("{err:#}").contains("identity"), "{err:#}");
    assert_eq!(h.log(&id), before);
}

#[test]
fn integrity_is_checked_after_the_cheap_refusals_and_only_when_enrolling() {
    let h = Home::new();
    let id = h.legacy_channel(0);
    let calls = Cell::new(0);
    let counting = || {
        calls.set(calls.get() + 1);
        Ok(true)
    };
    let gate = EnrollGate {
        identity_protected: true,
        log_intact: &counting,
        legacy_sealer: None,
    };
    say(&h, &id, &gate, "one").unwrap();
    say(&h, &id, &gate, "two").unwrap();
    assert_eq!(calls.get(), 1, "an enrolled channel is not re-checked");
}

#[test]
fn signer_not_chain_head_refuses() {
    let h = Home::new();
    let id = h.legacy_channel(1);
    let before = h.log(&id);
    let other = TestHuman(AgentIdentity::generate(), 0);
    let rh = h.router_home();
    let err = h
        .svc()
        .append_human(
            &id,
            &other,
            h.seal(&rh),
            &open_gate(),
            EventKind::Message,
            serde_json::json!({ "text": "x" }),
            None,
        )
        .unwrap_err();
    assert!(format!("{err:#}").contains("head of the local human chain"));
    assert_eq!(h.log(&id), before);
}

#[test]
fn foreign_record_present_still_enrolls() {
    let h = Home::new();
    let id = h.legacy_channel(0);
    // An agent plants a record naming somebody else's key (D-7).
    let forger = AgentIdentity::generate();
    let key = mur_common::identity::encode_pubkey_bytes(&forger.verifying_key_bytes());
    h.svc()
        .store()
        .append_event(
            &id,
            ChannelActor::local_human_with_key(key.clone()),
            EventKind::OwnerKeyEnrolled,
            serde_json::to_value(OwnerKeyEnrolledPayload {
                owner_key: key,
                key_version: 0,
                human_cut: None,
            })
            .unwrap(),
            None,
            None,
            None,
        )
        .unwrap();
    assert!(h.trust(&id).live_record().is_none());

    say(&h, &id, &open_gate(), "hi").unwrap();
    let live = h.trust(&id);
    assert_eq!(live.live_record().map(|r| r.seq), Some(1));
    assert_eq!(live.ignored_records(), [0]);
}

#[test]
fn record_anchor_is_router_sealed_over_a_legacy_agent_head() {
    let h = Home::new();
    let id = h.legacy_channel(0);
    // A legacy agent-sealed head (what an anchored pre-phase-2 write leaves).
    let agent = AgentIdentity::generate();
    let ah = h.path().join("agents").join("qa");
    agent.save(&ah).unwrap();
    h.svc()
        .append_and_anchor(
            &id,
            &agent,
            0,
            &ah,
            ChannelActor::Agent { id: "qa".into() },
            EventKind::Message,
            serde_json::json!({ "text": "old" }),
            None,
        )
        .unwrap();
    let gate = EnrollGate {
        legacy_sealer: Some(LegacySealer {
            identity: &agent,
            kv: 0,
            key_home: &ah,
        }),
        ..open_gate()
    };
    say(&h, &id, &gate, "new").unwrap();
    let head = read_head(&crate::anchor::heads_dir(h.path()), &id)
        .unwrap()
        .unwrap();
    assert!(head.verify(&h.router.verifying_key_bytes()));
    assert!(!head.verify(&agent.verifying_key_bytes()));
}

#[test]
fn retired_key_record_is_not_live() {
    let h = Home::new();
    let id = h.legacy_channel(0);
    // Rekey: A (version 0) is retired, B (version 1) is the chain head.
    let a = &h.human.0;
    let b = AgentIdentity::generate();
    let mut rot = RotationAttestation::new(
        "u",
        a.pubkey_text(),
        b.pubkey_text(),
        0,
        1,
        "2026-11-10T00:00:00Z",
        RotationReason::Scheduled,
    );
    rot.sign(a.signing_key());
    fs::write(
        human_rotations_path(h.path()),
        format!(
            "{}\n{}\n",
            serde_json::to_string(&bootstrap(a)).unwrap(),
            serde_json::to_string(&rot).unwrap()
        ),
    )
    .unwrap();
    fs::remove_dir_all(human_key_dir(h.path())).unwrap();
    fs::remove_file(human_identity_dir(h.path()).join("identity.pub")).unwrap();
    b.save_split(&human_identity_dir(h.path()), &human_key_dir(h.path()))
        .unwrap();

    // Someone holding the retired key plants a record after the rekey. It is
    // not anchored, so it grants nothing.
    let a_key = mur_common::identity::encode_pubkey_bytes(&a.verifying_key_bytes());
    h.svc()
        .store()
        .append_signed_by_with(
            &id,
            0,
            &|m| a.sign_bytes(m),
            ChannelActor::local_human_with_key(a_key.clone()),
            EventKind::OwnerKeyEnrolled,
            serde_json::to_value(OwnerKeyEnrolledPayload {
                owner_key: a_key,
                key_version: 0,
                human_cut: None,
            })
            .unwrap(),
            None,
            crate::store::SealHooks::default(),
        )
        .unwrap();
    assert!(h.trust(&id).live_record().is_none());

    // The record is dormant: it verifies on the chain, a head would make it
    // live. C1 must not anchor over it.
    assert_eq!(h.trust(&id).dormant_records(), [0]);
    let rh = h.router_home();
    let new_human = TestHuman(b, 1);
    let before = h.log(&id);
    let err = h
        .svc()
        .append_human(
            &id,
            &new_human,
            h.seal(&rh),
            &open_gate(),
            EventKind::Message,
            serde_json::json!({ "text": "hi" }),
            None,
        )
        .unwrap_err();
    assert!(format!("{err:#}").contains("retired human key"), "{err:#}");
    assert_eq!(h.log(&id), before, "nothing was written");
    assert!(
        read_head(&crate::anchor::heads_dir(h.path()), &id)
            .unwrap()
            .is_none(),
        "no head sealed over the planted record"
    );
}

#[test]
fn read_only_v3_on_v2_channel_is_byte_identical() {
    let h = Home::new();
    let id = h.legacy_channel(2);
    let log = h.log(&id);
    let manifest = fs::read(manifest_path(&h, &id)).unwrap();
    let _ = h.svc().load_events(&id).unwrap();
    let _ = h.svc().store().load_manifest(&id).unwrap();
    let _ = h.trust(&id);
    assert_eq!(h.log(&id), log);
    assert_eq!(fs::read(manifest_path(&h, &id)).unwrap(), manifest);
}

#[test]
fn c1_on_v3_manifest_without_record_skips_bump() {
    let h = Home::new();
    let id = h.legacy_channel(0);
    let path = manifest_path(&h, &id);
    let before = fs::read_to_string(&path).unwrap();
    assert!(before.contains("v: 3"), "B3 left a v3 manifest");
    say(&h, &id, &open_gate(), "hi").unwrap();
    // `v` is untouched; the only manifest change is the append's own
    // title/updated_at bookkeeping, never a second schema bump.
    let after = fs::read_to_string(&path).unwrap();
    assert!(after.contains("v: 3"));
    let strip = |t: &str| {
        t.lines()
            .filter(|l| !l.starts_with("updated_at:") && !l.starts_with("title:"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(strip(&before), strip(&after));
}

/// Crash after the record is on disk but before its head is sealed: the
/// record is live but flagged `Unanchored` (damage, reported — never a silent
/// trust), and the next human write seals the head over it without writing a
/// second record.
#[test]
fn crash_between_record_and_anchor_is_flagged_then_completes() {
    let h = Home::new();
    let id = h.legacy_channel(1);
    let key = mur_common::identity::encode_pubkey_bytes(&h.human.pubkey());
    let payload = serde_json::to_value(OwnerKeyEnrolledPayload {
        owner_key: key.clone(),
        key_version: 0,
        human_cut: None,
    })
    .unwrap();
    let boom = |_: &crate::store::SealInput<'_>| Err(anyhow::anyhow!("crash"));
    let err = h
        .svc()
        .store()
        .append_signed_by_with(
            &id,
            0,
            &|m| h.human.sign(m),
            ChannelActor::local_human_with_key(key),
            EventKind::OwnerKeyEnrolled,
            payload,
            None,
            crate::store::SealHooks::post(Some(&boom)),
        )
        .unwrap_err();
    assert!(format!("{err:#}").contains("crash"));
    let t = h.trust(&id);
    assert!(
        t.damage()
            .contains(&crate::verify::human::OwnerDamage::Unanchored),
        "a record no head covers is reported, not silently trusted: {:?}",
        t.damage()
    );

    say(&h, &id, &open_gate(), "after").unwrap();
    let t = h.trust(&id);
    assert!(t.damage().is_empty(), "{:?}", t.damage());
    let records = h
        .svc()
        .load_events(&id)
        .unwrap()
        .iter()
        .filter(|e| e.kind == EventKind::OwnerKeyEnrolled)
        .count();
    assert_eq!(records, 1, "no second record");
}

/// Crash after `v: 3` but before the record: reopen is a plain legacy channel
/// (untrusted), and the next human write enrolls it with no second bump.
#[test]
fn crash_after_manifest_before_record_reopens_as_legacy() {
    let h = Home::new();
    let id = h.legacy_channel(0);
    assert_eq!(h.svc().store().load_manifest(&id).unwrap().v, 3);
    assert!(h.trust(&id).live_record().is_none());
    say(&h, &id, &open_gate(), "hi").unwrap();
    assert!(h.trust(&id).live_record().is_some());
}

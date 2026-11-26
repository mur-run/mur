use crate::payload::{RefPayload, WireRef};
use crate::payload_tests::payload;
use crate::sign::*;
use mur_common::channel::{ChannelActor, EventKind};
use mur_common::identity::{AgentIdentity, RotationAttestation, RotationReason};
use std::path::Path;

fn agent(mur_home: &Path, name: &str) -> AgentIdentity {
    let home = mur_home.join("agents").join(name);
    std::fs::create_dir_all(&home).unwrap();
    let id = AgentIdentity::generate();
    id.save(&home).unwrap();
    id
}

type Tamper = Box<dyn Fn(&mut RefPayload)>;

fn signed(id: &AgentIdentity) -> WireRef {
    sign_ref(id, &payload())
}

#[test]
fn a_signed_ref_verifies() {
    let tmp = tempfile::tempdir().unwrap();
    let id = agent(tmp.path(), "alice");
    assert_eq!(verify_ref(tmp.path(), &signed(&id)), Ok(()));
}

#[test]
fn a_ref_from_another_member_key_does_not_verify() {
    let tmp = tempfile::tempdir().unwrap();
    let _alice = agent(tmp.path(), "alice");
    let mallory = AgentIdentity::generate();
    assert_eq!(
        verify_ref(tmp.path(), &signed(&mallory)),
        Err(VerifyError::BadSignature)
    );
}

#[test]
fn tampering_each_field_fails_verification() {
    let tmp = tempfile::tempdir().unwrap();
    let id = agent(tmp.path(), "alice");
    let _bob = agent(tmp.path(), "bob");
    let good = signed(&id);
    let tamper: Vec<(&str, Tamper)> = vec![
        ("fleet", Box::new(|p| p.fleet = "g".into())),
        ("run", Box::new(|p| p.run = "r2".into())),
        ("member", Box::new(|p| p.member = "bob".into())),
        ("name", Box::new(|p| p.name = "other.md".into())),
        ("seq", Box::new(|p| p.seq += 1)),
        ("sha256", Box::new(|p| p.sha256 = "b".repeat(64))),
        ("size", Box::new(|p| p.size += 1)),
        ("admitted_at", Box::new(|p| p.admitted_at += 1)),
        ("request_id", Box::new(|p| p.request_id = "f".repeat(32))),
    ];
    for (field, f) in tamper {
        let mut w = good.clone();
        f(&mut w.payload);
        assert!(
            verify_ref(tmp.path(), &w).is_err(),
            "tampered {field} verified"
        );
    }
    let mut w = good.clone();
    w.sig.push('x');
    assert!(verify_ref(tmp.path(), &w).is_err(), "tampered sig verified");
}

#[test]
fn a_member_that_is_not_a_valid_name_is_refused_before_any_path_is_built() {
    let tmp = tempfile::tempdir().unwrap();
    let id = agent(tmp.path(), "alice");
    let mut w = signed(&id);
    w.payload.member = "../alice".into();
    assert_eq!(verify_ref(tmp.path(), &w), Err(VerifyError::InvalidMember));
}

#[test]
fn an_unknown_member_has_no_key() {
    let tmp = tempfile::tempdir().unwrap();
    let id = AgentIdentity::generate();
    assert_eq!(
        verify_ref(tmp.path(), &signed(&id)),
        Err(VerifyError::NoKey)
    );
}

#[test]
fn a_channel_event_signature_never_verifies_as_a_ref() {
    let tmp = tempfile::tempdir().unwrap();
    let id = agent(tmp.path(), "alice");
    let p = payload();
    // Sign the canonical JSON of the same payload as a channel event would
    // sign it, with no domain prefix, and offer it as a reference signature.
    let bytes = mur_common::identity::canonical_json(&p);
    let w = WireRef {
        payload: p.clone(),
        sig: id.sign_multibase(&bytes),
    };
    assert_eq!(verify_ref(tmp.path(), &w), Err(VerifyError::BadSignature));
    // And a real channel event signature over a payload carrying the ref.
    let actor = ChannelActor::Agent { id: "alice".into() };
    let ev_payload = serde_json::json!({"fleet_shared_ref": serde_json::to_value(&p).unwrap()});
    let sig = mur_channel::sign::sign_event(
        &id,
        "fleet-f",
        &actor,
        EventKind::Artifact,
        &ev_payload,
        None,
    );
    let w = WireRef { payload: p, sig };
    assert_eq!(verify_ref(tmp.path(), &w), Err(VerifyError::BadSignature));
}

#[test]
fn a_ref_signature_never_verifies_as_a_channel_event() {
    let tmp = tempfile::tempdir().unwrap();
    let id = agent(tmp.path(), "alice");
    let w = signed(&id);
    let pubkey = id.verifying_key_bytes();
    let actor = ChannelActor::Agent { id: "alice".into() };
    let ev_payload = serde_json::json!({"fleet_shared_ref": serde_json::to_value(&w).unwrap()});
    for payload in [ev_payload, serde_json::to_value(&w.payload).unwrap()] {
        assert!(!mur_channel::sign::verify_event_sig(
            "fleet-f",
            &actor,
            EventKind::Artifact,
            &payload,
            None,
            &w.sig,
            &pubkey
        ));
    }
}

#[test]
fn a_ref_signed_before_a_key_rotation_verifies_at_its_key_version() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("agents").join("alice");
    std::fs::create_dir_all(&home).unwrap();
    let old = AgentIdentity::generate();
    let new = AgentIdentity::generate();
    new.save(&home).unwrap();
    let boot = RotationAttestation::new(
        "u",
        "",
        old.pubkey_text(),
        0,
        0,
        "t0",
        RotationReason::Scheduled,
    )
    .into_bootstrap();
    let mut rot = RotationAttestation::new(
        "u",
        old.pubkey_text(),
        new.pubkey_text(),
        0,
        1,
        "t1",
        RotationReason::Scheduled,
    );
    rot.sign(old.signing_key());
    let chain = [boot, rot]
        .iter()
        .map(|a| serde_json::to_string(a).unwrap() + "\n")
        .collect::<String>();
    std::fs::write(home.join("rotations.jsonl"), chain).unwrap();

    let mut before = payload();
    before.key_version = 0;
    assert_eq!(verify_ref(tmp.path(), &sign_ref(&old, &before)), Ok(()));
    let mut after = payload();
    after.key_version = 1;
    assert_eq!(verify_ref(tmp.path(), &sign_ref(&new, &after)), Ok(()));
    // The old key does not verify a reference that claims the new version.
    assert!(verify_ref(tmp.path(), &sign_ref(&old, &after)).is_err());
}

#[test]
fn the_signed_bytes_are_the_domain_then_canonical_json() {
    let tmp = tempfile::tempdir().unwrap();
    let id = agent(tmp.path(), "alice");
    let w = signed(&id);
    let mut bytes = REF_SIGN_DOMAIN.to_vec();
    bytes.extend(mur_common::identity::canonical_json(&w.payload));
    assert_eq!(REF_SIGN_DOMAIN, b"mur.fleet_shared_ref.v1\0");
    assert!(mur_common::identity::verify_bytes(
        &id.verifying_key_bytes(),
        &bytes,
        &w.sig
    ));
}

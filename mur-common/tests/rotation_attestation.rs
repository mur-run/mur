use mur_common::identity::{AgentIdentity, IdentityError, RotationAttestation, RotationReason};

fn att(
    old_v: u32,
    new_v: u32,
    reason: RotationReason,
    old_pub: &str,
    new_pub: &str,
) -> RotationAttestation {
    RotationAttestation::new(
        "01JQX4TM8Y9K7VQH6B2N3R5DPE",
        old_pub,
        new_pub,
        old_v,
        new_v,
        "2026-04-25T10:00:00+08:00",
        reason,
    )
}

#[test]
fn sign_then_verify_roundtrip() {
    let old = AgentIdentity::generate();
    let new = AgentIdentity::generate();
    let mut a = att(
        1,
        2,
        RotationReason::Scheduled,
        &old.pubkey_text(),
        &new.pubkey_text(),
    );
    a.sign(old.signing_key());
    a.verify(&old.pubkey_text()).expect("must verify");
    assert!(
        a.signature.starts_with('z'),
        "signature should be multibase"
    );
}

#[test]
fn tampered_new_pubkey_fails_verify() {
    let old = AgentIdentity::generate();
    let new = AgentIdentity::generate();
    let mut a = att(
        1,
        2,
        RotationReason::Scheduled,
        &old.pubkey_text(),
        &new.pubkey_text(),
    );
    a.sign(old.signing_key());
    a.new_pubkey = AgentIdentity::generate().pubkey_text();
    let err = a.verify(&old.pubkey_text()).unwrap_err();
    assert!(matches!(err, IdentityError::InvalidKey(_)));
}

#[test]
fn wrong_old_pubkey_fails_verify() {
    let old = AgentIdentity::generate();
    let other = AgentIdentity::generate();
    let new = AgentIdentity::generate();
    let mut a = att(
        1,
        2,
        RotationReason::Scheduled,
        &old.pubkey_text(),
        &new.pubkey_text(),
    );
    a.sign(old.signing_key());
    let err = a.verify(&other.pubkey_text()).unwrap_err();
    assert!(matches!(err, IdentityError::InvalidKey(_)));
}

#[test]
fn empty_signature_rejected_by_strict_verify() {
    let old = AgentIdentity::generate();
    let new = AgentIdentity::generate();
    let a = att(
        1,
        2,
        RotationReason::Scheduled,
        &old.pubkey_text(),
        &new.pubkey_text(),
    );
    let err = a.verify(&old.pubkey_text()).unwrap_err();
    assert!(matches!(err, IdentityError::InvalidKey(_)));
}

#[test]
fn emergency_with_empty_signature_passes_lenient_verify() {
    let old = AgentIdentity::generate();
    let new = AgentIdentity::generate();
    let a = att(
        1,
        2,
        RotationReason::Emergency,
        &old.pubkey_text(),
        &new.pubkey_text(),
    );
    a.verify_or_emergency(&old.pubkey_text())
        .expect("emergency must pass lenient");
}

#[test]
fn emergency_with_signature_still_strictly_verifies() {
    let old = AgentIdentity::generate();
    let new = AgentIdentity::generate();
    let mut a = att(
        1,
        2,
        RotationReason::Emergency,
        &old.pubkey_text(),
        &new.pubkey_text(),
    );
    a.sign(old.signing_key());
    a.verify_or_emergency(&old.pubkey_text())
        .expect("signed emergency must verify");
    a.verify(&old.pubkey_text())
        .expect("signed emergency must verify under strict path too");
}

#[test]
fn bootstrap_entry_skips_verification() {
    let new = AgentIdentity::generate();
    let mut a = att(0, 0, RotationReason::Scheduled, "", &new.pubkey_text());
    a = a.into_bootstrap();
    a.verify("anything-bogus")
        .expect("bootstrap accepts any pubkey");
    a.verify("").expect("bootstrap accepts empty pubkey");
}

#[test]
fn canonical_bytes_excludes_signature_field() {
    let old = AgentIdentity::generate();
    let new = AgentIdentity::generate();
    let mut a = att(
        1,
        2,
        RotationReason::Scheduled,
        &old.pubkey_text(),
        &new.pubkey_text(),
    );
    let before = a.canonical_bytes();
    a.signature = "zSPOOF".into();
    let after = a.canonical_bytes();
    assert_eq!(
        before, after,
        "signature field must not appear in canonical bytes"
    );
}

#[test]
fn canonical_bytes_keys_are_sorted() {
    let old = AgentIdentity::generate();
    let new = AgentIdentity::generate();
    let a = att(
        1,
        2,
        RotationReason::Scheduled,
        &old.pubkey_text(),
        &new.pubkey_text(),
    );
    let bytes = a.canonical_bytes();
    let s = std::str::from_utf8(&bytes).unwrap();
    // First two keys alphabetically: "algorithm" before "new_key_version" before "new_pubkey"...
    let alg_idx = s.find("\"algorithm\"").unwrap();
    let uuid_idx = s.find("\"uuid\"").unwrap();
    let new_pub_idx = s.find("\"new_pubkey\"").unwrap();
    assert!(
        alg_idx < new_pub_idx,
        "algorithm must precede new_pubkey alphabetically"
    );
    assert!(
        new_pub_idx < uuid_idx,
        "new_pubkey must precede uuid alphabetically"
    );
}

#[test]
fn serde_roundtrip() {
    let old = AgentIdentity::generate();
    let new = AgentIdentity::generate();
    let mut a = att(
        3,
        4,
        RotationReason::OwnerChange,
        &old.pubkey_text(),
        &new.pubkey_text(),
    );
    a.sign(old.signing_key());
    let json = serde_json::to_string(&a).unwrap();
    let back: RotationAttestation = serde_json::from_str(&json).unwrap();
    assert_eq!(a, back);
}

//! The signed head of spec 7.2: what the router signs, and what a verifier
//! must reject. Expected canonical bytes are spelled out literally rather than
//! derived from the code under test.

use chrono::{TimeZone, Utc};
use mur_channel::head::{HEAD_VERSION, Head, HeadFields};
use mur_common::identity::AgentIdentity;

const CHAIN_HEX: &str = "aa00000000000000000000000000000000000000000000000000000000000001";

fn fields() -> HeadFields {
    HeadFields {
        channel_id: "chan-1".into(),
        seq: 7,
        lines: 9,
        chain_hash: CHAIN_HEX.into(),
        key_version: Some(2),
        legacy_through: Some(3),
    }
}

fn sealed(id: &AgentIdentity) -> Head {
    Head::seal(
        id,
        fields(),
        Utc.with_ymd_and_hms(2026, 10, 14, 0, 0, 0).unwrap(),
    )
}

#[test]
fn head_version_starts_at_one() {
    assert_eq!(HEAD_VERSION, 1);
    assert_eq!(sealed(&AgentIdentity::generate()).v, 1);
}

#[test]
fn signature_verifies_under_the_signing_key() {
    let id = AgentIdentity::generate();
    assert!(sealed(&id).verify(&id.verifying_key_bytes()));
}

#[test]
fn signature_does_not_verify_under_another_key() {
    let id = AgentIdentity::generate();
    let other = AgentIdentity::generate();
    assert!(!sealed(&id).verify(&other.verifying_key_bytes()));
}

#[test]
fn sign_input_is_the_sorted_key_object_without_sig_or_ts() {
    let expected = format!(
        r#"{{"chain_hash":"{CHAIN_HEX}","channel_id":"chan-1","key_version":2,"legacy_through":3,"lines":9,"seq":7,"v":1}}"#
    );
    let head = sealed(&AgentIdentity::generate());
    assert_eq!(String::from_utf8(head.sign_input()).unwrap(), expected);
}

#[test]
fn null_key_version_and_legacy_through_are_signed_as_null() {
    let mut f = fields();
    f.key_version = None;
    f.legacy_through = None;
    let head = Head::seal(&AgentIdentity::generate(), f, Utc::now());
    let input = String::from_utf8(head.sign_input()).unwrap();
    assert!(input.contains(r#""key_version":null"#), "{input}");
    assert!(input.contains(r#""legacy_through":null"#), "{input}");
}

#[test]
fn timestamp_is_not_signed() {
    let id = AgentIdentity::generate();
    let mut head = sealed(&id);
    head.ts = Utc.with_ymd_and_hms(2030, 1, 1, 0, 0, 0).unwrap();
    assert!(head.verify(&id.verifying_key_bytes()));
}

#[test]
fn changing_any_signed_field_breaks_the_signature() {
    let id = AgentIdentity::generate();
    let pk = id.verifying_key_bytes();
    let base = sealed(&id);

    let mut h = base.clone();
    h.channel_id = "chan-2".into();
    assert!(!h.verify(&pk), "channel_id");

    let mut h = base.clone();
    h.seq += 1;
    assert!(!h.verify(&pk), "seq");

    let mut h = base.clone();
    h.lines += 1;
    assert!(!h.verify(&pk), "lines");

    let mut h = base.clone();
    h.chain_hash = CHAIN_HEX.replace("aa", "ab");
    assert!(!h.verify(&pk), "chain_hash");

    let mut h = base.clone();
    h.key_version = Some(3);
    assert!(!h.verify(&pk), "key_version");

    let mut h = base.clone();
    h.key_version = None;
    assert!(!h.verify(&pk), "key_version dropped");

    // Raising legacy_through would hide a hole; clearing it would hide that
    // the channel was ever legacy. Both must fail.
    let mut h = base.clone();
    h.legacy_through = Some(8);
    assert!(!h.verify(&pk), "legacy_through raised");

    let mut h = base;
    h.legacy_through = None;
    assert!(!h.verify(&pk), "legacy_through cleared");
}

#[test]
fn unknown_version_is_rejected_even_with_a_matching_signature() {
    let id = AgentIdentity::generate();
    let mut head = sealed(&id);
    head.v = HEAD_VERSION + 1;
    assert!(!head.verify(&id.verifying_key_bytes()));
}

#[test]
fn malformed_signature_is_rejected_not_panicked_on() {
    let id = AgentIdentity::generate();
    let pk = id.verifying_key_bytes();
    for bad in ["", "not-multibase", "z1111"] {
        let mut head = sealed(&id);
        head.sig = bad.into();
        assert!(!head.verify(&pk), "{bad:?}");
    }
}

#[test]
fn json_round_trip_keeps_the_signature_valid() {
    let id = AgentIdentity::generate();
    let head = sealed(&id);
    let text = serde_json::to_string(&head).unwrap();
    let back: Head = serde_json::from_str(&text).unwrap();
    assert_eq!(back, head);
    assert!(back.verify(&id.verifying_key_bytes()));
}

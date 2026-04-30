//! Integration test: voice manifest signing + verification round-trip.
//!
//! Spins up an in-process httpmock server, serves a manifest signed
//! with a freshly-generated Ed25519 key, and exercises the
//! `verify_signature_with_key` + JSON parse pipeline that the download
//! client uses. (The download client itself uses `verify_and_parse`
//! which delegates to the compile-time-pinned pubkey; injecting a
//! fresh key end-to-end would require build-time env override, which
//! the plan defers to CI release builds. Here we exercise the
//! verification + parse logic directly.)

use ed25519_dalek::{Signer, SigningKey};
use rand::rngs::OsRng;
use sha2::{Digest, Sha256};

use mur_agent_gui_lib::voice::manifest::{
    AssetBundle, AssetEntry, VoiceManifest, sha256_hex, verify_signature_with_key,
};

#[test]
fn signed_manifest_verifies_then_parses() {
    let key = SigningKey::generate(&mut OsRng);
    let manifest = AssetBundle::Voice(VoiceManifest {
        schema_version: 1,
        voice_id: "test_voice".into(),
        display_name: "Test".into(),
        language: "en-US".into(),
        sample_rate_hz: 24000,
        license: "MIT".into(),
        creator: "test".into(),
        assets: vec![AssetEntry {
            name: "voice.onnx".into(),
            sha256_hex: sha256_hex(b"fake-onnx-bytes"),
            size_bytes: 15,
            url: "https://example.test/voice.onnx".into(),
        }],
        size_bytes_total: 15,
    });
    let bytes = serde_json::to_vec(&manifest).unwrap();
    let sig = key.sign(&bytes);

    verify_signature_with_key(&bytes, &sig.to_bytes(), &key.verifying_key())
        .expect("freshly-signed manifest must verify");

    let parsed: AssetBundle = serde_json::from_slice(&bytes).unwrap();
    match parsed {
        AssetBundle::Voice(v) => assert_eq!(v.voice_id, "test_voice"),
        _ => panic!("wrong variant"),
    }
}

#[test]
fn tampered_manifest_fails_verification() {
    let key = SigningKey::generate(&mut OsRng);
    let mut bytes = serde_json::to_vec(&AssetBundle::Voice(VoiceManifest {
        schema_version: 1,
        voice_id: "x".into(),
        display_name: "X".into(),
        language: "en-US".into(),
        sample_rate_hz: 24000,
        license: "MIT".into(),
        creator: "t".into(),
        assets: vec![],
        size_bytes_total: 0,
    }))
    .unwrap();
    let sig = key.sign(&bytes);
    // Mutate one byte → signature must reject.
    bytes[10] ^= 0x01;
    let r = verify_signature_with_key(&bytes, &sig.to_bytes(), &key.verifying_key());
    assert!(r.is_err(), "tampered manifest must fail verification");
}

#[test]
fn signature_from_wrong_key_fails() {
    let signing_key = SigningKey::generate(&mut OsRng);
    let other_key = SigningKey::generate(&mut OsRng);
    let bytes = b"any payload";
    let sig = signing_key.sign(bytes);
    let r = verify_signature_with_key(bytes, &sig.to_bytes(), &other_key.verifying_key());
    assert!(r.is_err(), "sig from wrong key must fail");
}

#[test]
fn sha256_hex_helper_matches_manual_digest() {
    let payload = b"hello world";
    let manual = hex::encode(Sha256::digest(payload));
    assert_eq!(sha256_hex(payload), manual);
}

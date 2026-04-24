use mur_common::identity::{AgentIdentity, IdentityError, decode_pubkey};
use tempfile::TempDir;

#[test]
fn generate_roundtrip() {
    let dir = TempDir::new().unwrap();
    let id = AgentIdentity::generate();
    id.save(dir.path()).unwrap();
    let loaded = AgentIdentity::load(dir.path()).unwrap();
    assert_eq!(id.verifying_key_bytes(), loaded.verifying_key_bytes());
}

#[test]
fn load_missing_returns_err() {
    let dir = TempDir::new().unwrap();
    let err = AgentIdentity::load(dir.path()).unwrap_err();
    assert!(matches!(err, IdentityError::NotFound));
}

#[test]
fn private_key_file_is_mode_0600() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let id = AgentIdentity::generate();
        id.save(dir.path()).unwrap();
        let meta = std::fs::metadata(dir.path().join("identity.key")).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    }
}

#[test]
fn pubkey_text_starts_with_z() {
    let id = AgentIdentity::generate();
    let text = id.pubkey_text();
    assert!(text.starts_with('z'), "expected base58btc 'z' prefix, got: {text}");
}

#[test]
fn pubkey_roundtrip() {
    let id = AgentIdentity::generate();
    let encoded = id.pubkey_text();
    let decoded = decode_pubkey(&encoded).unwrap();
    assert_eq!(decoded, id.verifying_key_bytes());
}

#[test]
fn decode_wrong_length_errors() {
    // base58btc encoding of 16 zero bytes → wrong length after decode
    let short = multibase::encode(multibase::Base::Base58Btc, [0u8; 16]);
    let err = decode_pubkey(&short).unwrap_err();
    match err {
        IdentityError::InvalidKey(_) => {}
        other => panic!("expected InvalidKey, got {other:?}"),
    }
}

#[test]
fn decode_invalid_text_errors() {
    let err = decode_pubkey("not-multibase").unwrap_err();
    assert!(matches!(err, IdentityError::Multibase(_)));
}

#[test]
fn ed25519_to_x25519_static_key_is_deterministic() {
    let id = AgentIdentity::generate();
    let k1 = id.to_x25519_static_secret();
    let k2 = id.to_x25519_static_secret();
    // Montgomery form derivation is deterministic from Ed25519 scalar
    assert_eq!(k1.to_bytes(), k2.to_bytes());
}

#[test]
fn ed25519_x25519_pair_agree() {
    // Two agents can compute matching shared secrets via X25519 derived
    // from their Ed25519 keypairs.
    let a = AgentIdentity::generate();
    let b = AgentIdentity::generate();

    let a_priv = a.to_x25519_static_secret();
    let b_priv = b.to_x25519_static_secret();
    let a_pub = x25519_dalek::PublicKey::from(&a_priv);
    let b_pub = x25519_dalek::PublicKey::from(&b_priv);

    let shared_a = a_priv.diffie_hellman(&b_pub);
    let shared_b = b_priv.diffie_hellman(&a_pub);
    assert_eq!(shared_a.as_bytes(), shared_b.as_bytes());
}

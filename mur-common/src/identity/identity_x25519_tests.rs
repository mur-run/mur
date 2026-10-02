use super::*;

#[test]
fn x25519_pub_matches_secret_derivation() {
    // The public-side Ed25519→X25519 conversion must equal the X25519
    // public derived from the agent's own static secret — otherwise the
    // Noise peer-auth allowlist would never match `get_remote_static()`.
    let id = AgentIdentity::generate();
    let from_secret = x25519_dalek::PublicKey::from(&id.to_x25519_static_secret());
    let from_pub = x25519_pub_from_multibase(&id.public_key_multibase()).unwrap();
    assert_eq!(from_secret.as_bytes(), &from_pub);
}

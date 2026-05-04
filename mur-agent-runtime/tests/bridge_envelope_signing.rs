use mur_agent_runtime::bridge::verify::verify_inbound_envelope;
use mur_common::bridge::envelope::{EnvelopeError, sign_payload};
use mur_common::bridge::peer::TrustedPeer;
use mur_common::identity::{AgentIdentity, encode_pubkey};

fn peer_for(id: &AgentIdentity, ver: Option<u32>) -> TrustedPeer {
    TrustedPeer {
        pubkey_multibase: encode_pubkey(&id.verifying_key()),
        name: "stub".into(),
        key_version: ver,
    }
}

#[test]
fn signed_by_trusted_passes() {
    let id = AgentIdentity::generate();
    let env = sign_payload(b"hi".to_vec(), &id, 0);
    verify_inbound_envelope(&env, &[peer_for(&id, None)]).unwrap();
}

#[test]
fn unknown_peer_rejected() {
    let id = AgentIdentity::generate();
    let env = sign_payload(b"hi".to_vec(), &id, 0);
    assert!(matches!(
        verify_inbound_envelope(&env, &[]).unwrap_err(),
        EnvelopeError::UntrustedPeer
    ));
}

#[test]
fn key_version_pin_mismatch_rejected() {
    let id = AgentIdentity::generate();
    let env = sign_payload(b"hi".to_vec(), &id, 1);
    assert!(matches!(
        verify_inbound_envelope(&env, &[peer_for(&id, Some(2))]).unwrap_err(),
        EnvelopeError::UntrustedPeer
    ));
}

#[test]
fn key_version_pin_match_accepted() {
    let id = AgentIdentity::generate();
    let env = sign_payload(b"hi".to_vec(), &id, 4);
    verify_inbound_envelope(&env, &[peer_for(&id, Some(4))]).unwrap();
}

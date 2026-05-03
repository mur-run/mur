//! Track C1 acceptance: stub bridge → SignedEnvelope → user-agent verify
//! against trusted_peers[] → 2xx advances offset.
//! This exercises the bridge plumbing in-process; full supervisor spawn
//! is the shell harness's job.

use mur_agent_runtime::bridge::ack::AckTracker;
use mur_agent_runtime::bridge::dedupe::DedupeStore;
use mur_agent_runtime::bridge::verify::verify_inbound_envelope;
use mur_common::bridge::envelope::sign_payload;
use mur_common::bridge::peer::TrustedPeer;
use mur_common::identity::{AgentIdentity, encode_pubkey};
use tempfile::TempDir;

#[test]
fn stub_bridge_full_loop() {
    let tmp = TempDir::new().unwrap();
    let bridge_id = AgentIdentity::generate();
    let trust = vec![TrustedPeer {
        pubkey_multibase: encode_pubkey(&bridge_id.verifying_key()),
        name: "stub_bridge".into(),
        key_version: None,
    }];
    let mut dedupe = DedupeStore::open(tmp.path(), "stub_bridge").unwrap();
    let mut tracker: AckTracker<u64> = AckTracker::new(0);

    for n in 1u64..=3 {
        let key = format!("msg-{n}");
        assert!(!dedupe.is_seen(&key).unwrap());
        dedupe.mark_seen(&key).unwrap();

        let inner = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "message/send",
            "params": { "agent": "coach", "body": format!("hello #{n}") },
            "id": n,
        });
        let env = sign_payload(serde_json::to_vec(&inner).unwrap(), &bridge_id, 0);
        verify_inbound_envelope(&env, &trust).expect("verifies");
        tracker.start_pending(n);
        tracker.confirm();
    }
    assert_eq!(tracker.committed_offset(), 3);
}

#[test]
fn untrusted_attacker_rejected() {
    let attacker = AgentIdentity::generate();
    let trusted = AgentIdentity::generate();
    let trust = vec![TrustedPeer {
        pubkey_multibase: encode_pubkey(&trusted.verifying_key()),
        name: "stub".into(),
        key_version: None,
    }];
    let env = sign_payload(b"evil".to_vec(), &attacker, 0);
    assert!(matches!(
        verify_inbound_envelope(&env, &trust).unwrap_err(),
        mur_common::bridge::envelope::EnvelopeError::UntrustedPeer
    ));
}

#[test]
fn five_xx_keeps_offset() {
    let mut t: AckTracker<u64> = AckTracker::new(10);
    t.start_pending(20);
    t.reject();
    assert_eq!(t.committed_offset(), 10);
}

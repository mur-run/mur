use mur_agent_runtime::transport::noise::{build_initiator, build_responder};
use mur_common::identity::AgentIdentity;

#[test]
fn xk_handshake_completes_in_three_messages() {
    let responder_id = AgentIdentity::generate();
    let initiator_id = AgentIdentity::generate();

    let responder_static = responder_id.to_x25519_static_secret().to_bytes();
    let responder_pub = x25519_dalek::PublicKey::from(
        &responder_id.to_x25519_static_secret(),
    );

    let mut responder = build_responder(&responder_static).unwrap();
    let mut initiator = build_initiator(
        &initiator_id.to_x25519_static_secret().to_bytes(),
        responder_pub.as_bytes(),
    )
    .unwrap();

    // msg 1: -> e, es
    let mut buf1 = [0u8; 1024];
    let n = initiator.write_message(&[], &mut buf1).unwrap();
    responder.read_message(&buf1[..n], &mut []).unwrap();

    // msg 2: <- e, ee
    let mut buf2 = [0u8; 1024];
    let n = responder.write_message(&[], &mut buf2).unwrap();
    initiator.read_message(&buf2[..n], &mut []).unwrap();

    // msg 3: -> s, se
    let mut buf3 = [0u8; 1024];
    let n = initiator.write_message(&[], &mut buf3).unwrap();
    responder.read_message(&buf3[..n], &mut []).unwrap();

    assert!(initiator.is_handshake_finished());
    assert!(responder.is_handshake_finished());

    // Post-handshake: responder learns initiator's static pubkey
    let remote_static = responder.get_remote_static().unwrap();
    assert_eq!(
        remote_static,
        x25519_dalek::PublicKey::from(&initiator_id.to_x25519_static_secret())
            .as_bytes()
    );
}

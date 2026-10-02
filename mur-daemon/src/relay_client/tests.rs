use super::*;
use mur_common::identity::AgentIdentity;

fn signed_envelope(id: &AgentIdentity) -> mur_common::bridge::envelope::SignedEnvelope {
    mur_common::bridge::envelope::sign_payload(b"{}".to_vec(), id, 1)
}

#[test]
fn only_hello_is_allowed_before_pairing() {
    let unpaired: Option<String> = None;
    let hello = ClientFrame::Hello {
        pubkey: "pk".to_string(),
        token: "t".to_string(),
        agent: "mur".to_string(),
    };
    // Hello may proceed pre-pairing; every other frame is gated.
    assert!(frame_allowed_before_dispatch(&hello, &unpaired));
    for frame in [
        ClientFrame::AudioStreamEnd,
        ClientFrame::AudioStreamStart { sample_rate: 16000 },
        ClientFrame::ChannelQuery {
            op: "list".to_string(),
            channel_id: None,
            since_seq: None,
        },
    ] {
        assert!(
            !frame_allowed_before_dispatch(&frame, &unpaired),
            "non-Hello frame must be rejected before pairing (closes the \
                 no-Hello voice/ChannelQuery bypass)"
        );
    }

    // Once paired, the same frames are allowed.
    let paired = Some("pk".to_string());
    assert!(frame_allowed_before_dispatch(
        &ClientFrame::AudioStreamEnd,
        &paired
    ));
    assert!(frame_allowed_before_dispatch(
        &ClientFrame::ChannelQuery {
            op: "events".to_string(),
            channel_id: Some("c".to_string()),
            since_seq: None,
        },
        &paired
    ));
}

#[test]
fn relay_envelope_authorized_pins_to_the_connection_device() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path();
    let id = AgentIdentity::generate();
    let env = signed_envelope(&id);
    let pk = env.bridge_pubkey_multibase.clone();
    mur_core::mobile::add_paired_device(home, &pk).unwrap();

    // Paired device, key matches the connection, signature intact → authorized.
    assert!(relay_envelope_authorized(home, &env, &Some(pk.clone())));

    // Connection not yet paired (None) → rejected even though the device is in
    // the store and the signature is valid.
    assert!(!relay_envelope_authorized(home, &env, &None));

    // Connection paired as a DIFFERENT device → rejected (no cross-device use).
    assert!(!relay_envelope_authorized(
        home,
        &env,
        &Some("other".to_string())
    ));

    // Tampered payload → signature no longer verifies → rejected.
    let mut tampered = env.clone();
    tampered.payload = br#"{"x":1}"#.to_vec();
    assert!(!relay_envelope_authorized(home, &tampered, &Some(pk)));
}

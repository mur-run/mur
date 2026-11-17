//! At-mention phase 2, B1: adding `key` to `ChannelActor::Human` must not
//! change a single byte a pre-phase-2 signature covers. The fixtures were
//! written by the code before B1 (fixed secret, see `writer.pub`), so a
//! serde change that adds `"key": null` or reorders the actor breaks here.

use mur_channel::sign::{verify_event_sig, verify_one};
use mur_common::channel::{ChannelActor, ChannelEvent};

const V1: &str = include_str!("fixtures/pre_phase2/human_v1.json");
const V2: &str = include_str!("fixtures/pre_phase2/human_v2.json");
const WRITER: &str = include_str!("fixtures/pre_phase2/writer.pub");
const CHANNEL: &str = "ch-golden";

fn writer() -> [u8; 32] {
    mur_common::identity::decode_pubkey(WRITER.trim()).expect("fixture pubkey")
}

fn load(line: &str) -> ChannelEvent {
    serde_json::from_str(line.trim_end()).expect("fixture event")
}

#[test]
fn golden_pre_phase2_human_signature_still_verifies() {
    let v1 = load(V1);
    assert!(
        v1.prev_hash.is_none(),
        "v1 fixture must exercise sign_input"
    );
    assert!(matches!(v1.actor, ChannelActor::Human { key: None, .. }));
    assert!(verify_event_sig(
        CHANNEL,
        &v1.actor,
        v1.kind,
        &v1.payload,
        v1.idempotency_key.as_deref(),
        v1.sig.as_deref().unwrap(),
        &writer(),
    ));
    assert!(verify_one(CHANNEL, &v1, &writer(), true));

    let v2 = load(V2);
    assert!(
        v2.prev_hash.is_some(),
        "v2 fixture must exercise sign_input_v2"
    );
    assert!(verify_one(CHANNEL, &v2, &writer(), true));
}

#[test]
fn golden_pre_phase2_events_round_trip_byte_identical() {
    for line in [V1, V2] {
        let again = serde_json::to_string(&load(line)).unwrap();
        assert_eq!(again, line.trim_end());
    }
}

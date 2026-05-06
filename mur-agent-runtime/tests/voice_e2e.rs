//! E2E test for voice enable/disable CLI round-trip.
//!
//! Does NOT test actual audio hardware or model inference —
//! those require physical audio devices and large model files.
//! This test verifies the profile schema write + read path only.

use mur_common::agent::{AgentProfile, VoiceId};

#[test]
fn voice_config_enables_and_round_trips_profile() {
    let mut profile = AgentProfile::default_for_tests();
    assert!(!profile.voice.enabled);
    assert_eq!(profile.voice.voice_id, VoiceId::AfHeart);

    profile.voice.enabled = true;
    profile.voice.voice_id = VoiceId::AmMichael;

    let yaml = serde_yaml_ng::to_string(&profile).expect("serialize");
    let loaded: AgentProfile = serde_yaml_ng::from_str(&yaml).expect("deserialize");

    assert!(loaded.voice.enabled);
    assert_eq!(loaded.voice.voice_id, VoiceId::AmMichael);
}

#[test]
fn voice_config_disabled_by_default_on_fresh_profile() {
    let profile = AgentProfile::default_for_tests();
    assert!(!profile.voice.enabled);
}

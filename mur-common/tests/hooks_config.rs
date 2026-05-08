use mur_common::HooksConfig;

#[test]
fn hooks_config_defaults() {
    let cfg = HooksConfig::default();
    assert!(cfg.ledger, "ledger default is true");
    assert_eq!(
        cfg.companion_voice, None,
        "companion_voice default is None (auto)"
    );
    assert_eq!(cfg.voice_input, None, "voice_input default is None (auto)");
}

#[test]
fn hooks_config_roundtrip_empty_yaml() {
    // A profile.yaml with no hooks: block at all should deserialize to defaults.
    let yaml = "ledger: true\n";
    let cfg: HooksConfig = serde_yaml::from_str(yaml).unwrap();
    assert!(cfg.ledger);
    assert_eq!(cfg.companion_voice, None);
}

#[test]
fn hooks_config_partial_override_ledger_false() {
    let yaml = "ledger: false\n";
    let cfg: HooksConfig = serde_yaml::from_str(yaml).unwrap();
    assert!(!cfg.ledger);
    assert_eq!(cfg.companion_voice, None);
    assert_eq!(cfg.voice_input, None);
}

#[test]
fn hooks_config_explicit_companion_voice_true() {
    let yaml = "companion_voice: true\n";
    let cfg: HooksConfig = serde_yaml::from_str(yaml).unwrap();
    assert!(cfg.ledger, "ledger still defaults to true");
    assert_eq!(cfg.companion_voice, Some(true));
    assert_eq!(cfg.voice_input, None);
}

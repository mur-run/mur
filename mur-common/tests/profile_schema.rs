use mur_common::agent::AgentProfile;

#[test]
fn profile_identity_defaults_are_empty_and_optional() {
    // Loading an old P0a-style YAML without identity block must still work;
    // Default values populate the field.
    let yaml = include_str!("fixtures/profile_p0a_minimal.yaml");
    let p: AgentProfile = serde_yaml_ng::from_str(yaml).unwrap();
    assert!(p.identity.pubkey.is_empty() || p.identity.pubkey.starts_with('z'));
    assert!(p.identity.owner.is_none());
}

#[test]
fn profile_identity_roundtrip() {
    let yaml = include_str!("fixtures/profile_p0a5_with_identity.yaml");
    let p: AgentProfile = serde_yaml_ng::from_str(yaml).unwrap();
    assert_eq!(p.identity.pubkey, "zABCD1234");
    assert_eq!(p.identity.owner.as_deref(), Some("david@twdd.com.tw"));

    // Roundtrip
    let emitted = serde_yaml_ng::to_string(&p).unwrap();
    let p2: AgentProfile = serde_yaml_ng::from_str(&emitted).unwrap();
    assert_eq!(p, p2);
}

#[test]
fn tcp_transport_default_disabled() {
    let yaml = include_str!("fixtures/profile_p0a_minimal.yaml");
    let p: AgentProfile = serde_yaml_ng::from_str(yaml).unwrap();
    assert!(!p.transport.tcp.enabled, "tcp must default disabled");
    assert!(p.transport.tcp.bind.is_empty());
}

#[test]
fn tcp_transport_roundtrip() {
    let yaml = include_str!("fixtures/profile_p0a5_tcp_enabled.yaml");
    let p: AgentProfile = serde_yaml_ng::from_str(yaml).unwrap();
    assert!(p.transport.tcp.enabled);
    assert_eq!(p.transport.tcp.bind, "0.0.0.0:39393");
    assert_eq!(
        p.transport.tcp.noise.pattern,
        "Noise_XK_25519_ChaChaPoly_BLAKE2s"
    );
}

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

#[test]
fn lifecycle_execution_defaults_daemon() {
    let yaml = include_str!("fixtures/profile_p0a_minimal.yaml");
    let p: AgentProfile = serde_yaml_ng::from_str(yaml).unwrap();
    assert_eq!(
        p.lifecycle.execution,
        mur_common::agent::ExecutionMode::Daemon
    );
    assert!(p.lifecycle.schedule.is_empty());
}

#[test]
fn lifecycle_schedule_on_demand_roundtrip() {
    let yaml = include_str!("fixtures/profile_p0a5_scheduled.yaml");
    let p: AgentProfile = serde_yaml_ng::from_str(yaml).unwrap();
    assert_eq!(
        p.lifecycle.execution,
        mur_common::agent::ExecutionMode::OnDemand
    );
    assert_eq!(p.lifecycle.schedule.len(), 1);
    assert_eq!(p.lifecycle.schedule[0].cron, "0 9 * * 1-5");
}

#[test]
fn file_transfer_defaults_sensible() {
    let yaml = include_str!("fixtures/profile_p0a_minimal.yaml");
    let p: AgentProfile = serde_yaml_ng::from_str(yaml).unwrap();
    assert_eq!(p.file_transfer.accept_incoming_file_max_bytes, 10_485_760);
    assert_eq!(p.file_transfer.require_approval_above_bytes, 10_485_760);
}

#[test]
fn deployment_defaults_to_laptop() {
    let yaml = include_str!("fixtures/profile_p0a_minimal.yaml");
    let p: AgentProfile = serde_yaml_ng::from_str(yaml).unwrap();
    assert_eq!(
        p.deployment.deployment_type,
        mur_common::agent::DeploymentType::Laptop
    );
    assert_eq!(p.deployment.environment.as_deref(), Some("dev"));
}

#[test]
fn identity_rekey_fields_default_when_absent() {
    // Legacy P0a.5 profile (no rekey fields) should still deserialize
    let yaml = include_str!("fixtures/profile_p0a5_with_identity.yaml");
    let p: AgentProfile = serde_yaml_ng::from_str(yaml).unwrap();
    assert_eq!(p.identity.algorithm, "ed25519");
    assert_eq!(p.identity.key_version, 0);
    assert!(p.identity.previous_pubkey.is_none());
    assert!(p.identity.grace_expires_at.is_none());
    assert!(p.identity.rotated_at.is_none());
    assert!(p.identity.emergency_rekey_at.is_none());
}

#[test]
fn identity_rekey_fields_roundtrip() {
    let yaml = include_str!("fixtures/profile_p0a6_rotated.yaml");
    let p: AgentProfile = serde_yaml_ng::from_str(yaml).unwrap();
    assert_eq!(p.identity.pubkey, "zNEWKEY");
    assert_eq!(p.identity.algorithm, "ed25519");
    assert_eq!(p.identity.key_version, 2);
    assert_eq!(p.identity.previous_pubkey.as_deref(), Some("zOLDKEY"));
    assert_eq!(p.identity.previous_key_version, Some(1));
    assert!(p.identity.grace_expires_at.is_some());
    assert!(p.identity.rotated_at.is_some());
    assert!(p.identity.created_at_key.is_some());

    // Roundtrip
    let emitted = serde_yaml_ng::to_string(&p).unwrap();
    let p2: AgentProfile = serde_yaml_ng::from_str(&emitted).unwrap();
    assert_eq!(p, p2);
}

#[test]
fn identity_default_sets_algorithm_ed25519() {
    use mur_common::agent::IdentityConfig;
    let d = IdentityConfig::default();
    assert_eq!(d.algorithm, "ed25519");
    assert_eq!(d.key_version, 0);
    assert!(d.pubkey.is_empty());
    assert!(d.owner.is_none());
    assert!(d.previous_pubkey.is_none());
}

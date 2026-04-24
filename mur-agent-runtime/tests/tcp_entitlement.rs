use mur_agent_runtime::supervisor::validate_tcp_entitlement;
use mur_common::agent::AgentProfile;

fn minimal_profile() -> AgentProfile {
    let yaml = include_str!("fixtures/profile_minimal.yaml");
    serde_yaml_ng::from_str(yaml).expect("fixture parses")
}

#[test]
fn tcp_disabled_is_ok() {
    let p = minimal_profile();
    assert!(validate_tcp_entitlement(&p).is_ok());
}

#[test]
fn tcp_enabled_but_no_inbound_port_is_error() {
    let mut p = minimal_profile();
    p.transport.tcp.enabled = true;
    p.transport.tcp.bind = "0.0.0.0:39393".into();
    // entitlements.network.inbound.ports is empty in fixture
    let err = validate_tcp_entitlement(&p).unwrap_err();
    assert!(
        err.to_string().contains("inbound.ports"),
        "error must mention inbound.ports, got: {err}"
    );
}

#[test]
fn tcp_enabled_with_matching_port_ok() {
    let mut p = minimal_profile();
    p.transport.tcp.enabled = true;
    p.transport.tcp.bind = "0.0.0.0:39393".into();
    p.entitlements.network.inbound.ports = vec![39393];
    assert!(validate_tcp_entitlement(&p).is_ok());
}

#[test]
fn tcp_bind_without_port_is_error() {
    let mut p = minimal_profile();
    p.transport.tcp.enabled = true;
    p.transport.tcp.bind = "invalid-bind".into();
    let err = validate_tcp_entitlement(&p).unwrap_err();
    assert!(
        err.to_string().contains("port") || err.to_string().contains("bind"),
        "error must mention port or bind, got: {err}"
    );
}

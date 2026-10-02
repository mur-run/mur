use super::*;

#[test]
fn restricted_mode_populates_allow_hosts() {
    let home = PathBuf::from("/tmp/agent_home");
    let policy = SandboxPolicy::from_entitlements(&minimal_entitlements(), &home);
    assert_eq!(
        policy.net_allow_hosts,
        Some(vec!["api.anthropic.com".to_string()])
    );
}

#[test]
fn unrestricted_mode_allows_all_hosts() {
    let mut ent = minimal_entitlements();
    ent.network.outbound.mode = NetworkOutboundMode::Unrestricted;
    let home = PathBuf::from("/tmp/agent_home");
    let policy = SandboxPolicy::from_entitlements(&ent, &home);
    assert_eq!(policy.net_allow_hosts, None);
}

#[test]
fn off_mode_blocks_all_hosts() {
    let mut ent = minimal_entitlements();
    ent.network.outbound.mode = NetworkOutboundMode::Off;
    let home = PathBuf::from("/tmp/agent_home");
    let policy = SandboxPolicy::from_entitlements(&ent, &home);
    assert_eq!(policy.net_allow_hosts, Some(vec![]));
}

#[test]
fn proxy_only_denies_general_tcp_but_keeps_host_allowlist() {
    let mut ent = minimal_entitlements();
    ent.network.outbound.mode = NetworkOutboundMode::ProxyOnly;
    ent.network.outbound.allow_hosts = vec!["localhost".into(), "127.0.0.1".into()];
    let home = PathBuf::from("/tmp/agent_home");
    let policy = SandboxPolicy::from_entitlements(&ent, &home);
    // General TCP denied (empty port list, but PRESENT — not None/unrestricted).
    assert_eq!(policy.net_allow_ports, Some(vec![]));
    // Host allowlist retained (NOT emptied like Off) so the LLM host resolves.
    assert_eq!(
        policy.net_allow_hosts,
        Some(vec!["localhost".to_string(), "127.0.0.1".to_string()])
    );
    // Flag set so the port helpers add loopback carve-outs (distinguishes
    // ProxyOnly's Some([]) from Off's Some([]), which leaves this false).
    assert!(policy.net_loopback_allowed);
}

#[test]
fn proxy_only_self_profile_widens_for_in_process_proxy_upstream() {
    let mut ent = minimal_entitlements();
    ent.network.outbound.mode = NetworkOutboundMode::ProxyOnly;
    let home = PathBuf::from("/tmp/agent_home");
    let mut policy = SandboxPolicy::from_entitlements(&ent, &home);
    policy.allow_in_process_proxy_upstream();
    // The runtime SELF profile regains the Restricted general-port set so
    // the in-process egress proxy can dial upstream (os error 1 fix);
    // child profiles are built separately and stay ProxyOnly-strict.
    assert_eq!(
        policy.net_allow_ports,
        Some(RESTRICTED_GENERAL_PORTS.to_vec())
    );
}

#[test]
fn off_mode_is_never_widened_by_proxy_upstream() {
    let mut ent = minimal_entitlements();
    ent.network.outbound.mode = NetworkOutboundMode::Off;
    let home = PathBuf::from("/tmp/agent_home");
    let mut policy = SandboxPolicy::from_entitlements(&ent, &home);
    policy.allow_in_process_proxy_upstream();
    // Off is air-gapped: no proxy carve-out may reopen it.
    assert_eq!(policy.net_allow_ports, Some(vec![]));
}

#[test]
fn restricted_and_unrestricted_unchanged_by_proxy_upstream() {
    let mut ent = minimal_entitlements();
    ent.network.outbound.mode = NetworkOutboundMode::Restricted;
    let home = PathBuf::from("/tmp/agent_home");
    let mut policy = SandboxPolicy::from_entitlements(&ent, &home);
    policy.allow_in_process_proxy_upstream();
    assert_eq!(
        policy.net_allow_ports,
        Some(RESTRICTED_GENERAL_PORTS.to_vec())
    );

    ent.network.outbound.mode = NetworkOutboundMode::Unrestricted;
    let mut policy = SandboxPolicy::from_entitlements(&ent, &home);
    policy.allow_in_process_proxy_upstream();
    assert_eq!(policy.net_allow_ports, None);
}

/// Restricted: a loopback LLM port granted via `allow_loopback_ports` lands
/// ONLY in the loopback carve-out (SBPL `localhost:port`) — it must not
/// widen the general `*:port` list to remote hosts on the same port.
#[test]
fn restricted_llm_port_via_loopback_does_not_widen_general_list() {
    let mut ent = minimal_entitlements();
    ent.network.outbound.mode = NetworkOutboundMode::Restricted;
    let mut policy = SandboxPolicy::from_entitlements(&ent, &PathBuf::from("/tmp/a"));
    let before = policy.net_allow_ports.clone().unwrap();
    policy.allow_loopback_ports(&[8000, 8088]);
    assert_eq!(
        policy.net_allow_ports.unwrap(),
        before,
        "general list untouched"
    );
    assert!(policy.net_allow_loopback_ports.contains(&8000));
    assert!(policy.net_allow_loopback_ports.contains(&8088));
}

#[test]
fn allow_extra_ports_adds_llm_port_in_restricted_mode() {
    let mut ent = minimal_entitlements();
    ent.network.outbound.mode = NetworkOutboundMode::Restricted;
    let mut policy = SandboxPolicy::from_entitlements(&ent, &PathBuf::from("/tmp/a"));
    policy.allow_extra_ports(&[11434]);
    let ports = policy.net_allow_ports.unwrap();
    assert!(
        ports.contains(&11434),
        "ollama port must be granted: {ports:?}"
    );
    // Idempotent — re-adding doesn't duplicate.
    let mut p2 = SandboxPolicy::from_entitlements(&ent, &PathBuf::from("/tmp/a"));
    p2.allow_extra_ports(&[443]);
    assert_eq!(
        p2.net_allow_ports
            .unwrap()
            .iter()
            .filter(|&&p| p == 443)
            .count(),
        1
    );
}

/// Issue #006: user-declared extra egress ports must reach the sealed
/// policy under Restricted. Before this, `RESTRICTED_GENERAL_PORTS` was a
/// constant and a non-web port (ssh 2222, vite 5173) was unreachable with
/// no way to grant it short of `unrestricted` (which opens ALL ports).
#[test]
fn restricted_mode_includes_user_declared_allow_ports() {
    let mut ent = minimal_entitlements();
    ent.network.outbound.mode = NetworkOutboundMode::Restricted;
    ent.network.outbound.allow_ports = vec![2222, 5173];
    let policy = SandboxPolicy::from_entitlements(&ent, &PathBuf::from("/tmp/a"));
    let ports = policy.net_allow_ports.expect("restricted has a port list");
    for base in RESTRICTED_GENERAL_PORTS {
        assert!(
            ports.contains(&base),
            "base port {base} must remain: {ports:?}"
        );
    }
    assert!(
        ports.contains(&2222),
        "declared 2222 must be granted: {ports:?}"
    );
    assert!(
        ports.contains(&5173),
        "declared 5173 must be granted: {ports:?}"
    );
}

/// Boundary 1: `allow_ports` is a PORT grant, not a host grant — the port
/// opens to `*`, exactly like the base set. Declaring it must not shrink
/// or alter the host allowlist, which stays HostGuard's job.
#[test]
fn allow_ports_does_not_touch_host_allowlist() {
    let mut ent = minimal_entitlements();
    ent.network.outbound.mode = NetworkOutboundMode::Restricted;
    ent.network.outbound.allow_hosts = vec!["api.example.com".into()];
    ent.network.outbound.allow_ports = vec![2222];
    let policy = SandboxPolicy::from_entitlements(&ent, &PathBuf::from("/tmp/a"));
    assert_eq!(
        policy.net_allow_hosts,
        Some(vec!["api.example.com".to_string()])
    );
}

/// Fail-closed: the stricter modes must never be widened by a stale
/// `allow_ports` left in the profile. Off stays air-gapped, ProxyOnly
/// keeps denying general TCP.
#[test]
fn allow_ports_never_widens_off_or_proxy_only() {
    for mode in [NetworkOutboundMode::Off, NetworkOutboundMode::ProxyOnly] {
        let mut ent = minimal_entitlements();
        ent.network.outbound.mode = mode;
        ent.network.outbound.allow_ports = vec![2222];
        let policy = SandboxPolicy::from_entitlements(&ent, &PathBuf::from("/tmp/a"));
        assert_eq!(
            policy.net_allow_ports,
            Some(vec![]),
            "{mode:?} must not gain a general port from allow_ports"
        );
    }
}

/// Duplicates and a redundant re-declaration of a base port must not
/// produce duplicate SBPL / Landlock rules.
#[test]
fn allow_ports_dedupes_against_base_set() {
    let mut ent = minimal_entitlements();
    ent.network.outbound.mode = NetworkOutboundMode::Restricted;
    ent.network.outbound.allow_ports = vec![443, 2222, 2222];
    let policy = SandboxPolicy::from_entitlements(&ent, &PathBuf::from("/tmp/a"));
    let ports = policy.net_allow_ports.unwrap();
    assert_eq!(ports.iter().filter(|&&p| p == 443).count(), 1);
    assert_eq!(ports.iter().filter(|&&p| p == 2222).count(), 1);
}

#[test]
fn allow_extra_ports_respects_off_and_unrestricted() {
    // Off mode (Some([])) must NOT be silently re-opened.
    let mut off = minimal_entitlements();
    off.network.outbound.mode = NetworkOutboundMode::Off;
    let mut p_off = SandboxPolicy::from_entitlements(&off, &PathBuf::from("/tmp/a"));
    p_off.allow_extra_ports(&[11434]);
    assert_eq!(p_off.net_allow_ports, Some(vec![]));

    // Unrestricted (None) already allows everything — stays None.
    let mut unr = minimal_entitlements();
    unr.network.outbound.mode = NetworkOutboundMode::Unrestricted;
    let mut p_unr = SandboxPolicy::from_entitlements(&unr, &PathBuf::from("/tmp/a"));
    p_unr.allow_extra_ports(&[11434]);
    assert_eq!(p_unr.net_allow_ports, None);
}

#[test]
fn proxy_only_port_assembly_routes_llm_and_egress_to_loopback() {
    let mut policy = SandboxPolicy {
        net_allow_ports: Some(Vec::new()), // ProxyOnly: deny general TCP
        net_loopback_allowed: true,        // …but loopback carve-outs permitted
        ..Default::default()
    };
    // LLM port (cc-proxy) must land in LOOPBACK, not general ports.
    policy.allow_extra_ports(&[8088]);
    // Egress proxy port must be accepted even though general list is empty.
    policy.allow_loopback_ports(&[54321]);
    assert_eq!(
        policy.net_allow_ports,
        Some(vec![]),
        "general TCP stays denied"
    );
    assert!(
        policy.net_allow_loopback_ports.contains(&8088),
        "LLM port routed to loopback"
    );
    assert!(
        policy.net_allow_loopback_ports.contains(&54321),
        "egress proxy port accepted"
    );
}

#[test]
fn off_mode_port_assembly_stays_empty() {
    // Off is ALSO net_allow_ports = Some([]), but net_loopback_allowed = false
    // (the default) — neither helper may re-open loopback. This is the guard the
    // flag exists for.
    let mut policy = SandboxPolicy {
        net_allow_ports: Some(Vec::new()),
        // net_loopback_allowed left false
        ..Default::default()
    };
    policy.allow_extra_ports(&[8088]);
    policy.allow_loopback_ports(&[54321]);
    assert!(
        policy.net_allow_loopback_ports.is_empty(),
        "Off adds no loopback carve-outs"
    );
}

#[test]
fn restricted_port_assembly_unchanged() {
    // Non-empty general list = generic Restricted: LLM port still goes to
    // net_allow_ports (unchanged behavior).
    let mut policy = SandboxPolicy {
        net_allow_ports: Some(vec![80, 443]),
        ..Default::default()
    };
    policy.allow_extra_ports(&[8088]);
    assert!(policy.net_allow_ports.as_ref().unwrap().contains(&8088));
    assert!(!policy.net_allow_loopback_ports.contains(&8088));
}

#[test]
fn loopback_ports_respect_off_mode() {
    // Off = user denied all outbound; the carve-out must not reopen it.
    let mut p_off = SandboxPolicy {
        net_allow_ports: Some(vec![]),
        ..Default::default()
    };
    p_off.allow_loopback_ports(&[54321]);
    assert!(p_off.net_allow_loopback_ports.is_empty());

    // Restricted: carve-out applies, deduplicated.
    let mut p_r = SandboxPolicy {
        net_allow_ports: Some(vec![80, 443, 8080, 8443]),
        ..Default::default()
    };
    p_r.allow_loopback_ports(&[54321]);
    p_r.allow_loopback_ports(&[54321]);
    assert_eq!(p_r.net_allow_loopback_ports, vec![54321]);

    // Unrestricted (None): (allow default) already covers it; no rule needed.
    let mut p_u = SandboxPolicy {
        net_allow_ports: None,
        ..Default::default()
    };
    p_u.allow_loopback_ports(&[54321]);
    assert!(p_u.net_allow_loopback_ports.is_empty());
}

#[test]
fn connect_tcp_ports_proxy_only_is_loopback_only() {
    // ProxyOnly: empty general list + loopback ports → only the loopback ports
    // get ConnectTcp rules (general egress e.g. 443 is default-denied).
    let policy = SandboxPolicy {
        net_allow_ports: Some(Vec::new()),
        net_allow_loopback_ports: vec![8088, 54321],
        ..Default::default()
    };
    assert_eq!(connect_tcp_ports(&policy), vec![8088u16, 54321]);
}

#[test]
fn connect_tcp_ports_restricted_is_general_plus_loopback() {
    let policy = SandboxPolicy {
        net_allow_ports: Some(vec![80, 443]),
        net_allow_loopback_ports: vec![54321],
        ..Default::default()
    };
    assert_eq!(connect_tcp_ports(&policy), vec![80u16, 443, 54321]);
}

#[test]
fn connect_tcp_ports_off_and_unrestricted_are_empty() {
    let off = SandboxPolicy {
        net_allow_ports: Some(Vec::new()), // Off: empty general + empty loopback
        ..Default::default()
    };
    assert!(connect_tcp_ports(&off).is_empty());
    let unr = SandboxPolicy::default(); // Unrestricted: net_allow_ports = None
    assert!(connect_tcp_ports(&unr).is_empty());
}

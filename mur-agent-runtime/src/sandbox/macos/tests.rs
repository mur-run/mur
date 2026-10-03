use super::*;
use std::path::PathBuf;

fn policy_with(write: Vec<PathBuf>, deny: Vec<PathBuf>) -> SandboxPolicy {
    SandboxPolicy {
        fs_write: write,
        fs_deny: deny,
        ..Default::default()
    }
}

/// #850 option (c): every agent's private key is denied by ONE subtree
/// A malformed directory under `agents/` can no longer reach the profile
/// text at all — there is nothing to enumerate.
///
/// The guard this replaces skipped odd names so one hand-made directory
/// could not produce a profile that fails to compile and stops EVERY agent
/// starting. Under a subtree rule that failure mode does not exist, which
/// is strictly better than guarding against it.
#[test]
fn a_malformed_entry_under_agents_cannot_reach_the_profile() {
    let tmp = tempfile::tempdir().unwrap();
    let agents = tmp.path().join("agents");
    std::fs::create_dir_all(agents.join("alice")).unwrap();
    std::fs::create_dir_all(agents.join("we ird")).unwrap();

    let policy = policy_with_launch_chain(&agents.join("alice").to_string_lossy());
    let sbpl = build_sbpl_profile(&policy);

    assert!(
        !sbpl.contains("we ird"),
        "a malformed agent name reached the profile:\n{sbpl}"
    );
}

/// #850: the user's credential store is not an entitlement question. An
/// agent reaches its model through the runtime's own client, which
/// resolves credentials before the sandbox is sealed, so it never needs
/// these files — and reading them is exfiltrating the user's API keys or
/// taking over their account session.
#[test]
fn the_credential_store_is_denied_read_and_write() {
    let tmp = tempfile::tempdir().unwrap();
    let agents = tmp.path().join("agents");
    std::fs::create_dir_all(agents.join("alice")).unwrap();
    let policy = policy_with_launch_chain(&agents.join("alice").to_string_lossy());
    let sbpl = build_sbpl_profile(&policy);

    for p in [
        "secrets",
        "auth.json",
        "identity.key",
        "commander/signing.key",
        "mobile/pair-token",
        "actions-runner",
        "queue",
        "session",
        "conversations",
        "telemetry",
        "traces",
        "commander/.env",
        "runtime/vlc.json",
    ] {
        let full = tmp.path().join(p);
        for verb in ["file-read*", "file-write*"] {
            assert!(
                sbpl.contains(&format!("(deny {verb} (subpath \"{}\"))", full.display())),
                "{p} is not {verb}-denied:\n{sbpl}"
            );
        }
    }
}

/// The deny must NOT extend to a peer's verification material. Resolving a
/// peer's pubkey reads `identity.pub` and `rotations.jsonl` from that
/// peer's home, and verify-on-fold is per-actor — denying them would
/// fail-close every fleet, delegation and shared channel, silently. This
/// is the guard against "fix" it by denying the agents subtree.
#[test]
fn peer_verification_material_stays_readable() {
    let tmp = tempfile::tempdir().unwrap();
    let agents = tmp.path().join("agents");
    for name in ["alice", "bob"] {
        std::fs::create_dir_all(agents.join(name)).unwrap();
        std::fs::write(agents.join(name).join("identity.key"), b"k").unwrap();
    }
    let policy = policy_with_launch_chain(&agents.join("alice").to_string_lossy());
    let sbpl = build_sbpl_profile(&policy);

    for public in ["identity.pub", "rotations.jsonl"] {
        let p = agents.join("bob").join(public);
        assert!(
            !sbpl.contains(&format!("(deny file-read* (subpath \"{}\"))", p.display())),
            "bob's {public} was read-denied — peer signature verification \
                 will fail-close:\n{sbpl}"
        );
    }
    // And the agents tree itself must not be read-denied wholesale.
    assert!(
        !sbpl.contains(&format!(
            "(deny file-read* (subpath \"{}\"))",
            agents.display()
        )),
        "the whole agents tree was read-denied, which breaks verification:\n{sbpl}"
    );
}

fn policy_with_launch_chain(agent_home: &str) -> SandboxPolicy {
    let home = PathBuf::from(agent_home);
    let mur_home = home
        .parent()
        .and_then(|p| p.parent())
        .unwrap_or(&home)
        .to_path_buf();
    SandboxPolicy {
        launch_chain: crate::sandbox::launch_chain::LaunchChain::for_test(
            &home,
            &mur_home.join("bin"),
            &mur_home.join("home"),
        ),
        ..Default::default()
    }
}

/// #007, kernel side: the SBPL profile must NOT read-deny the agent's own
/// `profile.yaml`, or the tool gate would allow the read and the kernel
/// would answer with a bare EPERM — the exact unexplainable failure the
/// tool gate exists to prevent. `identity.key` must still be read-denied.
#[test]
fn sbpl_read_denies_own_key_but_not_own_profile() {
    let tmp = tempfile::tempdir().unwrap();
    let agent_home = tmp.path().join("agents").join("mur");
    std::fs::create_dir_all(&agent_home).unwrap();
    let mut policy = policy_with_launch_chain(&agent_home.to_string_lossy());
    policy.fs_deny = vec![
        agent_home.join("profile.yaml"),
        agent_home.join("identity.key"),
    ];
    let sbpl = build_sbpl_profile(&policy);

    let profile_p = agent_home.join("profile.yaml");
    let key_p = agent_home.join("identity.key");
    assert!(
        !sbpl.contains(&format!(
            "(deny file-read* (subpath \"{}\"))",
            profile_p.display()
        )),
        "own profile.yaml must not be read-denied:\n{sbpl}"
    );
    // Write protection (#712) is untouched.
    assert!(
        sbpl.contains(&format!(
            "(deny file-write* (subpath \"{}\"))",
            profile_p.display()
        )),
        "own profile.yaml must stay write-denied:\n{sbpl}"
    );
    assert!(
        sbpl.contains(&format!(
            "(deny file-read* (subpath \"{}\"))",
            key_p.display()
        )),
        "own identity.key must stay read-denied:\n{sbpl}"
    );
}

/// The SBPL twin of the tool-gate test: the agent's own public key
/// material is write-denied (a replaced key signs its own HITL approvals)
/// but never read-denied (peers verify the agent from it). The deny list is
/// the full `SELF_PROTECTED_AGENT_FILES` set, as `from_entitlements` builds
/// it, so the read carve-out is exercised against every sibling entry.
#[test]
fn sbpl_write_denies_own_public_key_material_but_keeps_it_readable() {
    let tmp = tempfile::tempdir().unwrap();
    let agent_home = tmp.path().join("agents").join("mur");
    std::fs::create_dir_all(&agent_home).unwrap();
    let mut policy = policy_with_launch_chain(&agent_home.to_string_lossy());
    policy.fs_deny = crate::sandbox::policy::SELF_PROTECTED_AGENT_FILES
        .iter()
        .map(|f| agent_home.join(f))
        .collect();
    let sbpl = build_sbpl_profile(&policy);

    for f in ["identity.pub", "rotations.jsonl"] {
        let p = agent_home.join(f);
        let rule = |verb: &str| format!("(deny {verb} (subpath \"{}\"))", p.display());
        // Last match wins, so the deny must come after the own-home allow.
        let allow = format!("(allow file-write* (subpath \"{}\"))", agent_home.display());
        let allow_at = sbpl.rfind(&allow).expect("own home re-allow");
        let deny_at = sbpl.rfind(&rule("file-write*"));
        assert!(
            deny_at.is_some_and(|d| d > allow_at),
            "own {f} must be write-denied after the own-home allow:\n{sbpl}"
        );
        assert!(
            !sbpl.contains(&rule("file-read*")),
            "own {f} must not be read-denied — peers verify from it:\n{sbpl}"
        );
    }
}

#[test]
fn writes_are_default_deny_with_baseline() {
    let sbpl = build_sbpl_profile(&policy_with(vec![], vec![]));
    assert!(
        sbpl.contains("(deny file-write* (subpath \"/\"))"),
        "missing default-deny-write baseline:\n{sbpl}"
    );
}

#[test]
fn system_write_paths_are_reallowed() {
    let sbpl = build_sbpl_profile(&policy_with(vec![], vec![]));
    for p in MACOS_SYSTEM_WRITE_PATHS {
        assert!(
            sbpl.contains(&format!("(allow file-write* (subpath \"{p}\"))")),
            "missing system write allow for {p}:\n{sbpl}"
        );
    }
}

#[test]
fn declared_write_path_is_allowed_after_baseline() {
    let sbpl = build_sbpl_profile(&policy_with(vec![PathBuf::from("/data/agent")], vec![]));
    let baseline = sbpl.find("(deny file-write* (subpath \"/\"))").unwrap();
    let allow = sbpl
        .find("(allow file-write* (subpath \"/data/agent\"))")
        .expect("declared write path must be allowed");
    assert!(
        allow > baseline,
        "allow must follow the deny baseline (last-match-wins)"
    );
}

#[test]
fn deny_path_wins_over_overlapping_write_grant() {
    // Issue #712: a denied file nested inside a granted write subtree
    // (e.g. the agent's own profile.yaml under agent_home) must be
    // emitted AFTER the allow so it wins the last-match-wins evaluation.
    let sbpl = build_sbpl_profile(&policy_with(
        vec![PathBuf::from("/data/agent")],
        vec![PathBuf::from("/data/agent/profile.yaml")],
    ));
    let allow = sbpl
        .find("(allow file-write* (subpath \"/data/agent\"))")
        .expect("write grant must be emitted");
    let deny = sbpl
        .find("(deny file-write* (subpath \"/data/agent/profile.yaml\"))")
        .expect("deny must be emitted");
    assert!(
        deny > allow,
        "deny must follow the overlapping allow (last-match-wins):\n{sbpl}"
    );
}

#[test]
fn agents_deny_precedes_the_self_reallow_which_precedes_the_self_file_denies() {
    let policy = policy_with_launch_chain("/data/.mur/agents/mur");
    let sbpl = build_sbpl_profile(&policy);

    let agents_deny = sbpl
        .find(r#"(deny file-write* (subpath "/data/.mur/agents"))"#)
        .expect("agents deny missing");
    let self_reallow = sbpl
        .rfind(r#"(allow file-write* (subpath "/data/.mur/agents/mur"))"#)
        .expect("self re-allow missing");
    let self_profile_deny = sbpl
        .find(r#"(deny file-write* (subpath "/data/.mur/agents/mur/profile.yaml"))"#)
        .expect("self profile deny missing");

    // SBPL is last-match-wins, so the ordering IS the mechanism. Asserting
    // the lines merely exist would pass on a profile that grants everything.
    assert!(
        agents_deny < self_reallow,
        "self re-allow must come after the agents deny or the agent cannot write its own home"
    );
    assert!(
        self_reallow < self_profile_deny,
        "self profile deny must come after the re-allow or self-protection is undone"
    );

    // Negative control: nothing re-denies the agent's own runtime files
    // after the re-allow — running.lock stays writable.
    assert!(
        !sbpl.contains(r#"(subpath "/data/.mur/agents/mur/running.lock")"#),
        "own-home runtime files must not be re-denied after the re-allow:\n{sbpl}"
    );
}

#[test]
fn malicious_path_is_escaped_not_injected() {
    // A path crafted to break out of the SBPL string literal must be
    // neutralized — the raw injection payload must not appear verbatim.
    let evil = PathBuf::from("x\") (allow file-write* (subpath \"/");
    let sbpl = build_sbpl_profile(&policy_with(vec![evil], vec![]));
    assert!(
        !sbpl.contains("x\") (allow file-write* (subpath \"/\"))"),
        "unescaped injection payload leaked into profile:\n{sbpl}"
    );
    assert!(
        sbpl.contains("\\\""),
        "quote should be backslash-escaped:\n{sbpl}"
    );
}

#[test]
fn off_mode_denies_network() {
    let mut policy = policy_with(vec![], vec![]);
    policy.net_allow_ports = Some(vec![]);
    let sbpl = build_sbpl_profile(&policy);
    assert!(sbpl.contains("(deny network-outbound)"));
    assert!(
        !sbpl.contains("(allow network-outbound"),
        "Off mode must not allow any outbound:\n{sbpl}"
    );
}

#[test]
fn restricted_uses_port_wildcard_not_hostname() {
    // Regression: hostname-based `remote tcp` is invalid SBPL and fails
    // sandbox_init for the whole profile (silent fail-open). Restricted
    // mode must emit `*:<port>` rules only.
    let mut policy = policy_with(vec![], vec![]);
    policy.net_allow_ports = Some(vec![443, 80]);
    policy.net_allow_hosts = Some(vec!["api.anthropic.com".to_string()]);
    let sbpl = build_sbpl_profile(&policy);
    assert!(sbpl.contains("(allow network-outbound (remote tcp \"*:443\"))"));
    assert!(sbpl.contains("(allow network-outbound (remote tcp \"*:80\"))"));
    assert!(
        !sbpl.contains("api.anthropic.com"),
        "SBPL must not contain hostnames (invalid `remote tcp` host):\n{sbpl}"
    );
}

/// Issue #006, kernel side: a user-declared extra port must reach the
/// actual SBPL profile as a general `*:port` clause. Asserting on the
/// policy struct alone would not prove the rule is installed.
#[test]
fn sbpl_emits_user_declared_extra_ports() {
    let mut policy = policy_with(vec![], vec![]);
    policy.net_allow_ports = Some(vec![80, 443, 8080, 8443, 2222, 5173]);
    let sbpl = build_sbpl_profile(&policy);
    assert!(sbpl.contains("(allow network-outbound (remote tcp \"*:2222\"))"));
    assert!(sbpl.contains("(allow network-outbound (remote tcp \"*:5173\"))"));
    // The baseline deny is still the first word on outbound.
    assert!(sbpl.contains("(deny network-outbound)"));
}

#[test]
fn restricted_allows_dns_resolution() {
    // Without the mDNSResponder socket allowance the `(deny network-outbound)`
    // baseline blocks macOS name resolution, so no external host resolves
    // and only loopback IPs work. Regression guard for that gap.
    let mut policy = policy_with(vec![], vec![]);
    policy.net_allow_ports = Some(vec![443]);
    let sbpl = build_sbpl_profile(&policy);
    assert!(
            sbpl.contains(
                "(allow network-outbound (remote unix-socket (path-literal \"/private/var/run/mDNSResponder\")))"
            ),
            "restricted profile must permit DNS via the mDNSResponder socket:\n{sbpl}"
        );
}

#[test]
fn off_mode_still_blocks_dns() {
    // Off (deny-all) must NOT get the DNS exception — it stays air-gapped.
    let mut policy = policy_with(vec![], vec![]);
    policy.net_allow_ports = Some(vec![]);
    let sbpl = build_sbpl_profile(&policy);
    assert!(
        !sbpl.contains("mDNSResponder"),
        "Off mode must not allow DNS:\n{sbpl}"
    );
}

#[test]
fn unrestricted_emits_no_network_rules() {
    let policy = policy_with(vec![], vec![]); // net_allow_ports defaults to None
    let sbpl = build_sbpl_profile(&policy);
    assert!(!sbpl.contains("network-outbound"));
}

#[test]
fn restricted_allows_scoped_unix_sockets() {
    // AF_UNIX connect is itself `network-outbound` under SBPL, so the
    // `(deny network-outbound)` baseline would otherwise break any
    // domain socket (test sockets, peer agent.sock dialing). These three
    // subpath carve-outs must be present without widening general
    // network access (TCP stays port-gated — see the next test).
    let mut policy = policy_with(vec![], vec![]);
    policy.net_allow_ports = Some(vec![443]);
    let sbpl = build_sbpl_profile(&policy);
    assert!(
        sbpl.contains(
            "(allow network-outbound (remote unix-socket (subpath \"/private/var/folders\")))"
        ),
        "restricted profile must allow unix sockets under macOS per-user temp:\n{sbpl}"
    );
    assert!(
        sbpl.contains("(allow network-outbound (remote unix-socket (subpath \"/private/tmp\")))"),
        "restricted profile must allow unix sockets under /private/tmp:\n{sbpl}"
    );
    let agents_dir = resolved_mur_home().join("agents");
    let agents_dir = sbpl_escape(&agents_dir.to_string_lossy());
    assert!(
        sbpl.contains(&format!(
            "(allow network-outbound (remote unix-socket (subpath \"{agents_dir}\")))"
        )),
        "restricted profile must allow unix sockets under <mur_home>/agents (A2A agent.sock):\n{sbpl}"
    );
}

#[test]
fn restricted_allows_unix_sockets_under_the_scratch_dir() {
    // #1642 made the scratch dir every child's TMPDIR, so Playwright MCP
    // binds `<scratch>/pw-*/browser/browser-*.sock` there and then dials
    // it. Without this carve-out `connect` fails with EPERM.
    let scratch = PathBuf::from("/Users/someone/.mur/tmp/agent_x");
    for ports in [vec![443], vec![]] {
        let mut policy = policy_with(vec![scratch.clone()], vec![]);
        policy.scratch_dir = Some(scratch.clone());
        policy.net_allow_ports = Some(ports);
        policy.net_allow_loopback_ports = vec![8080];
        let sbpl = build_sbpl_profile(&policy);
        assert!(
            sbpl.contains(
                "(allow network-outbound (remote unix-socket (subpath \"/Users/someone/.mur/tmp/agent_x\")))"
            ),
            "restricted profile must allow unix sockets under the scratch dir:\n{sbpl}"
        );
    }
}

#[test]
fn off_mode_does_not_allow_unix_sockets() {
    // Off (deny-all) must not get the scoped AF_UNIX carve-out either —
    // it stays fully air-gapped, matching off_mode_denies_network.
    let mut policy = policy_with(vec![], vec![]);
    policy.net_allow_ports = Some(vec![]);
    let sbpl = build_sbpl_profile(&policy);
    assert!(
        !sbpl.contains("(allow network-outbound"),
        "Off mode must not allow any outbound, including unix sockets:\n{sbpl}"
    );
}

#[test]
fn allowlist_mode_emits_exec_deny_baseline_and_system_reallows() {
    let mut policy = policy_with(vec![], vec![]);
    policy.spawn_mode = SpawnMode::Allowlist;
    policy.spawn_allowed_paths = vec![PathBuf::from("/usr/bin/env")];
    let sbpl = build_sbpl_profile(&policy);

    assert!(
        sbpl.contains("(deny process-exec* (subpath \"/\"))"),
        "missing default-deny-exec baseline:\n{sbpl}"
    );
    for p in MACOS_SYSTEM_EXEC_PATHS {
        assert!(
            sbpl.contains(&format!("(allow process-exec* (subpath \"{p}\"))")),
            "missing system exec re-allow for {p}:\n{sbpl}"
        );
    }
    for p in &policy.fs_exec {
        let p = sbpl_escape(&p.to_string_lossy());
        assert!(
            sbpl.contains(&format!("(allow process-exec* (subpath \"{p}\"))")),
            "missing fs_exec re-allow for {p}:\n{sbpl}"
        );
    }
    assert!(
        sbpl.contains("(allow process-exec* (path-literal \"/usr/bin/env\"))"),
        "missing path-literal allow for the resolved spawn binary:\n{sbpl}"
    );
}

#[test]
fn any_mode_emits_no_process_exec_clauses() {
    let mut policy = policy_with(vec![], vec![]);
    policy.spawn_mode = SpawnMode::Any;
    policy.spawn_allowed_paths = vec![PathBuf::from("/usr/bin/env")];
    let sbpl = build_sbpl_profile(&policy);
    assert!(
        !sbpl.contains("process-exec*"),
        "Any mode must fall through to the top-level allow default, no exec clauses:\n{sbpl}"
    );
}

#[test]
fn strict_mode_denies_system_exec_paths_but_allows_shell_literal() {
    let mut policy = policy_with(vec![], vec![]);
    policy.spawn_mode = SpawnMode::Strict;
    // Simulate what `SandboxPolicy::from_entitlements` would have
    // produced: the auto-seeded shell literal plus a fake
    // profile-declared allowlist entry and prefix.
    policy.spawn_allowed_paths = vec![PathBuf::from("/bin/bash"), PathBuf::from("/opt/fake/tool")];
    policy.spawn_allowed_prefixes = vec![PathBuf::from("/opt/fake")];
    let sbpl = build_sbpl_profile(&policy);

    assert!(
        sbpl.contains("(deny process-exec* (subpath \"/\"))"),
        "missing default-deny-exec baseline:\n{sbpl}"
    );
    for p in MACOS_SYSTEM_EXEC_PATHS {
        assert!(
            !sbpl.contains(&format!("(allow process-exec* (subpath \"{p}\"))")),
            "strict mode must NOT re-allow system exec path {p}:\n{sbpl}"
        );
    }
    assert!(
        sbpl.contains("(allow process-exec* (path-literal \"/bin/bash\"))"),
        "missing path-literal allow for the auto-seeded shell binary:\n{sbpl}"
    );
    assert!(
        sbpl.contains("(allow process-exec* (path-literal \"/opt/fake/tool\"))"),
        "missing path-literal allow for the profile's own spawn_allowed_paths entry:\n{sbpl}"
    );
    assert!(
        sbpl.contains("(allow process-exec* (subpath \"/opt/fake\"))"),
        "missing subpath allow for spawn_allowed_prefixes:\n{sbpl}"
    );
}

#[test]
fn loopback_port_carveout_is_localhost_scoped() {
    let mut policy = SandboxPolicy {
        net_allow_ports: Some(vec![80, 443]),
        ..Default::default()
    };
    policy.allow_loopback_ports(&[54321]);
    let sbpl = build_sbpl_profile(&policy);
    assert!(
        sbpl.contains("(allow network-outbound (remote tcp \"localhost:54321\"))"),
        "proxy port must be loopback-scoped: {sbpl}"
    );
    assert!(
        !sbpl.contains("\"*:54321\""),
        "proxy port must NOT be wildcard-host: {sbpl}"
    );
}

#[test]
fn restricted_loopback_only_policy_has_no_wildcard_tcp_allow() {
    // A worker whose egress is ONLY via loopback proxy: deny all general TCP outbound
    // and rely on loopback-only access. The airtight guarantee assumes no general
    // `(remote tcp "*:PORT")` allow exists (that would be a direct-egress escape hatch).
    let policy = SandboxPolicy {
        net_allow_ports: Some(Vec::new()), // deny all general TCP outbound
        net_allow_loopback_ports: vec![58999],
        ..Default::default()
    };
    let sbpl = build_sbpl_profile(&policy);

    // When all general TCP is denied, the deny network-outbound is present.
    assert!(sbpl.contains("(deny network-outbound)"));
    // The critical invariant: NO wildcard-host TCP allow (the escape hatch).
    assert!(
        !sbpl.contains("(remote tcp \"*:"),
        "restricted worker must not emit a wildcard-host tcp allow:\n{sbpl}"
    );
}

#[test]
fn proxy_only_sbpl_allows_loopback_and_dns_but_no_wildcard() {
    let policy = SandboxPolicy {
        net_allow_ports: Some(Vec::new()),           // deny general TCP
        net_allow_loopback_ports: vec![8088, 54321], // cc-proxy + egress proxy
        ..Default::default()
    };
    let sbpl = build_sbpl_profile(&policy);

    assert!(sbpl.contains("(deny network-outbound)"));
    // loopback carve-outs present…
    assert!(sbpl.contains("(remote tcp \"localhost:8088\")"));
    assert!(sbpl.contains("(remote tcp \"localhost:54321\")"));
    // …name resolution restored (loopback host resolution)…
    assert!(sbpl.contains("/private/var/run/mDNSResponder"));
    // …and NO wildcard-host tcp allow (the escape hatch).
    assert!(
        !sbpl.contains("(remote tcp \"*:"),
        "no wildcard tcp allow:\n{sbpl}"
    );
}

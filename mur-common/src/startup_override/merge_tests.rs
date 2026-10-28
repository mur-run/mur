use super::*;
use crate::agent::AgentProfile;

fn ent() -> Entitlements {
    let mut e = AgentProfile::default_for_tests().entitlements;
    e.network.outbound.mode = NetworkOutboundMode::Restricted;
    e.network.outbound.allow_hosts = vec!["api.example.com".into()];
    e.filesystem.read = vec!["~/src".into()];
    e.filesystem.write = vec!["~/src/out".into()];
    e.filesystem.deny = vec!["~/.ssh".into(), "~/src/out/keep".into()];
    e.processes.spawn.mode = SpawnMode::Allowlist;
    e.processes.spawn.allowed = vec!["git".into()];
    e
}

fn flags(net: bool, full: bool) -> OverrideFlags {
    OverrideFlags {
        no_hitl: false,
        net_unrestricted: net,
        full_perms: full,
    }
}

#[test]
fn no_flags_change_nothing() {
    let mut e = ent();
    let before = e.clone();
    assert!(apply(&mut e, flags(false, false)).is_empty());
    assert_eq!(e, before);
}

#[test]
fn no_hitl_alone_has_no_entitlement_field() {
    let mut e = ent();
    let before = e.clone();
    let f = OverrideFlags {
        no_hitl: true,
        ..OverrideFlags::default()
    };
    assert!(apply(&mut e, f).is_empty());
    assert_eq!(e, before);
}

#[test]
fn net_unrestricted_changes_only_the_outbound_mode() {
    let mut e = ent();
    let mut expected = e.clone();
    expected.network.outbound.mode = NetworkOutboundMode::Unrestricted;
    let changes = apply(&mut e, flags(true, false));
    assert_eq!(e, expected, "allow_hosts, fs and spawn stay as they were");
    assert_eq!(
        changes,
        vec!["network.outbound.mode: restricted -> unrestricted"]
    );
}

#[test]
fn full_perms_resets_deny_and_opens_spawn_only() {
    let mut e = ent();
    let mut expected = e.clone();
    expected.filesystem.deny = DEFAULT_DENY_PATHS.iter().map(|p| p.to_string()).collect();
    expected.processes.spawn.mode = SpawnMode::Any;
    let changes = apply(&mut e, flags(false, true));
    assert_eq!(
        e, expected,
        "grants, network and spawn.allowed are untouched"
    );
    assert_eq!(changes.len(), 2, "{changes:?}");
    assert!(changes[0].starts_with("filesystem.deny: [~/.ssh, ~/src/out/keep] -> ["));
    assert_eq!(changes[1], "processes.spawn.mode: allowlist -> any");
}

#[test]
fn the_reset_keeps_every_default_deny_path() {
    let mut e = ent();
    e.filesystem.deny.clear();
    apply(&mut e, flags(false, true));
    for p in DEFAULT_DENY_PATHS {
        assert!(e.filesystem.deny.iter().any(|d| d == p), "{p}");
    }
}

#[test]
fn an_already_relaxed_profile_reports_no_change() {
    let mut e = ent();
    e.network.outbound.mode = NetworkOutboundMode::Unrestricted;
    e.filesystem.deny = DEFAULT_DENY_PATHS.iter().map(|p| p.to_string()).collect();
    e.processes.spawn.mode = SpawnMode::Any;
    assert!(apply(&mut e, flags(true, true)).is_empty());
}

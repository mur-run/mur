use super::*;
use std::cell::Cell;

fn restricted_policy() -> SandboxPolicy {
    SandboxPolicy {
        net_allow_ports: Some(vec![]),
        ..Default::default()
    }
}

/// A socket path that exists, so canonicalisation succeeds, and sits outside
/// every allow rule (a fresh tempdir, not the policy's scratch).
fn outside_upstream() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("agent.sock");
    std::fs::write(&p, "").unwrap();
    (dir, p)
}

#[test]
fn unset_upstream_is_unresolved_and_never_probes() {
    let probed = Cell::new(false);
    let got = classify_at_start(&restricted_policy(), None, &mut |_, _| {
        probed.set(true);
        Classification::Boundary
    });
    assert_eq!(
        got,
        Classification::AuditOnly(AuditReason::UpstreamUnresolved)
    );
    assert!(!probed.get());
}

#[test]
fn unrestricted_network_never_reaches_the_probe() {
    let (_d, up) = outside_upstream();
    let got = classify_at_start(&SandboxPolicy::default(), Some(&up), &mut |_, _| {
        panic!("probe must not run")
    });
    assert!(matches!(got, Classification::AuditOnly(_)), "{got:?}");
}

// The three probe results only arise where the table yields `Candidate`.
#[cfg(target_os = "macos")]
mod candidate {
    use super::*;
    use crate::sandbox::ssh_bypass::probe::ProbeOutcome;

    fn with(outcome: ProbeOutcome) -> Classification {
        let (_d, up) = outside_upstream();
        classify_at_start(&restricted_policy(), Some(&up), &mut |profile, _| {
            assert!(
                profile.contains("(version 1)"),
                "probe gets the real profile"
            );
            outcome.classification()
        })
    }

    #[test]
    fn denied_is_boundary() {
        assert_eq!(with(ProbeOutcome::Denied), Classification::Boundary);
    }

    #[test]
    fn connected_is_probe_reachable() {
        assert_eq!(
            with(ProbeOutcome::Connected),
            Classification::AuditOnly(AuditReason::ProbeReachable)
        );
    }

    #[test]
    fn a_failed_spawn_is_inconclusive_never_boundary() {
        assert_eq!(
            with(ProbeOutcome::Inconclusive("spawn failed".into())),
            Classification::AuditOnly(AuditReason::ProbeInconclusive)
        );
    }
}

#[test]
fn status_maps_each_classification() {
    let b = status_for(&Classification::Boundary);
    assert_eq!((b.state, b.code), (SshProxyState::Boundary, None));

    let a = status_for(&Classification::AuditOnly(AuditReason::ProbeReachable));
    assert_eq!(a.state, SshProxyState::AuditOnly);
    assert_eq!(a.code.as_deref(), Some("probe_reachable"));

    // An unconfirmed candidate must never be persisted as a boundary.
    let c = status_for(&Classification::Candidate {
        canonical_upstream: "/x".into(),
    });
    assert_eq!(c.state, SshProxyState::AuditOnly);
    assert_eq!(c.code.as_deref(), Some("probe_inconclusive"));
}

#[test]
fn allowlist_explanation_names_the_root() {
    let e = explanation(&AuditReason::UpstreamInAllowlist {
        root: "/private/tmp".into(),
    });
    assert!(e.contains("/private/tmp"), "{e}");
}

#[test]
fn no_keys_records_off_without_probing() {
    let home = tempfile::tempdir().unwrap();
    let provisional = classify_before_seal(false, &restricted_policy());
    finalize(home.path(), provisional, true);
    let s = ssh_proxy_status::read(home.path()).unwrap();
    assert_eq!(s.state, SshProxyState::Off);
    assert!(s.at_unix_ms > 0);
}

fn boundary_at(at: u64) -> SshProxyStatus {
    SshProxyStatus {
        at_unix_ms: at,
        ..status_for(&Classification::Boundary)
    }
}

#[test]
fn boundary_names_the_proxy_as_the_only_route() {
    // The proxy runs now (Phase 2 part 2), but signs nothing until HITL is
    // wired, so the line must not promise mediated signatures either.
    let e = status_for(&Classification::Boundary).explanation;
    assert!(e.contains("only route"), "{e}");
    assert!(!e.contains("every signature"), "{e}");
}

#[test]
fn boundary_survives_an_enforcing_supervisor() {
    let s = settle(boundary_at(7), true);
    assert_eq!((s.state, s.code), (SshProxyState::Boundary, None));
}

// Probe denied, supervisor sandbox failed, advisory-only start allowed: the
// recorded state must not still say boundary.
#[test]
fn probe_denied_but_advisory_only_supervisor_is_audit_only() {
    let home = tempfile::tempdir().unwrap();
    let got = finalize(home.path(), boundary_at(7), false);
    assert_eq!(got.state, SshProxyState::AuditOnly);
    assert_eq!(got.code.as_deref(), Some("supervisor_not_enforcing"));
    assert_eq!(got.at_unix_ms, 7, "settling keeps the start's timestamp");
    let persisted = ssh_proxy_status::read(home.path()).unwrap();
    assert_eq!(persisted.state, SshProxyState::AuditOnly);
    assert_eq!(persisted.code.as_deref(), Some("supervisor_not_enforcing"));
}

#[test]
fn settling_never_upgrades_or_rewrites_other_states() {
    let a = status_for(&Classification::AuditOnly(AuditReason::ProbeReachable));
    for enforcing in [true, false] {
        let s = settle(a.clone(), enforcing);
        assert_eq!(s.code.as_deref(), Some("probe_reachable"));
    }
}

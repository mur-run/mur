//! #2541: which address family each loopback carve-out port is emitted with.
use super::*;
use crate::sandbox::LoopbackGrant;

/// #2541: the IPv6 family is opt-in per port, in both network arms. A port
/// granted v4 must not emit plain `tcp` (which admits `fe80::1%<any if>`).
#[test]
fn loopback_ports_are_ipv4_unless_granted_dual_stack() {
    for general in [vec![443], vec![]] {
        let mut policy = SandboxPolicy {
            net_allow_ports: Some(general.clone()),
            net_loopback_allowed: true,
            ..Default::default()
        };
        policy.allow_loopback_grants(&[LoopbackGrant::v4(54321), LoopbackGrant::dual_stack(8088)]);
        let sbpl = build_sbpl_profile(&policy);
        assert!(
            sbpl.contains("(allow network-outbound (remote tcp4 \"localhost:54321\"))"),
            "general={general:?}:\n{sbpl}"
        );
        assert!(
            !sbpl.contains("(remote tcp \"localhost:54321\")"),
            "general={general:?}: v4-only port leaked to IPv6:\n{sbpl}"
        );
        assert!(
            sbpl.contains("(allow network-outbound (remote tcp \"localhost:8088\"))"),
            "general={general:?}:\n{sbpl}"
        );
    }
}

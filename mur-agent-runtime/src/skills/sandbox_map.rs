//! TrustLevel -> Entitlements sandbox gate.
//!
//! Integration point: currently not wired because the hook chain's
//! `pre_tool_use` is never invoked from `TaskRunner::run_llm` (see
//! Reality Check in the M2 plan). M3 will wire this when the hook
//! chain gets tool-use visibility.
//!
//! Until then: `restrict_for_trust` is the policy function;
//! `TaskRunner::run_llm` logs `fired_skills` via tracing::info
//! as an observable seam.

use mur_common::agent::{
    Entitlements, NetworkEntitlement, NetworkOutboundMode, OutboundNetwork,
    ProcessesEntitlement, SpawnEntitlement, SpawnMode,
};
use mur_common::skill::types::TrustLevel;

/// Tighten `base` according to `trust`. Never widens.
pub fn restrict_for_trust(base: &Entitlements, trust: TrustLevel) -> Entitlements {
    let mut e = base.clone();
    match trust {
        TrustLevel::Sandboxed => {
            e.network.outbound = OutboundNetwork {
                mode: NetworkOutboundMode::Off,
                allow_hosts: vec![],
                protocols: vec![],
                resolve_dns: Default::default(),
            };
            e.processes = ProcessesEntitlement {
                spawn: SpawnEntitlement {
                    mode: SpawnMode::Allowlist,
                    allowed: vec![],
                },
            };
        }
        TrustLevel::Verified => {
            if matches!(e.network.outbound.mode, NetworkOutboundMode::Unrestricted) {
                e.network.outbound.mode = NetworkOutboundMode::Restricted;
            }
        }
        TrustLevel::Trusted => { /* pass-through */ }
    }
    e
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::agent::{
        FilesystemEntitlement, InboundNetwork, LimitsEntitlement, ResolveDnsConfig,
        SyscallsEntitlement,
    };

    fn base_open() -> Entitlements {
        Entitlements {
            network: NetworkEntitlement {
                inbound: InboundNetwork { ports: vec![] },
                outbound: OutboundNetwork {
                    mode: NetworkOutboundMode::Unrestricted,
                    allow_hosts: vec![],
                    protocols: vec!["tcp".into()],
                    resolve_dns: ResolveDnsConfig::default(),
                },
            },
            filesystem: FilesystemEntitlement::default(),
            processes: ProcessesEntitlement {
                spawn: SpawnEntitlement {
                    mode: SpawnMode::Unrestricted,
                    allowed: vec!["sh".into()],
                },
            },
            syscalls: SyscallsEntitlement::default(),
            limits: LimitsEntitlement::default(),
            llm: Default::default(),
        }
    }

    #[test]
    fn sandboxed_kills_network_and_spawn() {
        let e = restrict_for_trust(&base_open(), TrustLevel::Sandboxed);
        assert!(matches!(e.network.outbound.mode, NetworkOutboundMode::Off));
        assert!(matches!(e.processes.spawn.mode, SpawnMode::Allowlist));
        assert!(e.processes.spawn.allowed.is_empty());
    }

    #[test]
    fn verified_narrows_network_to_restricted() {
        let e = restrict_for_trust(&base_open(), TrustLevel::Verified);
        assert!(matches!(
            e.network.outbound.mode,
            NetworkOutboundMode::Restricted
        ));
    }

    #[test]
    fn trusted_is_identity() {
        let before = base_open();
        let after = restrict_for_trust(&before, TrustLevel::Trusted);
        assert!(matches!(
            after.network.outbound.mode,
            NetworkOutboundMode::Unrestricted
        ));
    }
}

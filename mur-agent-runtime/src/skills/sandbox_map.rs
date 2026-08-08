//! TrustLevel -> Entitlements sandbox gate.
//!
//! `restrict_for_trust` is the policy function — it exists and is tested.
//! The invocation site is in `TaskRunner::run_llm` where `fired_skills` are
//! now emitted via `Event::LlmCall.fired_skills`.
//!
//! Full enforcement (`pre_tool_use` gating) lands when `TaskRunner` gains
//! a tool-use loop — the hook chain's `on_prompt_submit` is now wired
//! (M2 deferred) so the pattern is established.

use mur_common::agent::{
    Entitlements, NetworkOutboundMode, OutboundNetwork, ProcessesEntitlement, SpawnEntitlement,
    SpawnMode,
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
                    allowed_dirs: vec![],
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
        FilesystemEntitlement, InboundNetwork, LimitsEntitlement, NetworkEntitlement,
        ResolveDnsConfig, SyscallsEntitlement,
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
                    mode: SpawnMode::Any,
                    allowed: vec!["sh".into()],
                    allowed_dirs: vec![],
                },
            },
            syscalls: SyscallsEntitlement::default(),
            limits: LimitsEntitlement::default(),
            llm: Default::default(),
            tools: vec![],
            fail_closed_on_sandbox_error: true,
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

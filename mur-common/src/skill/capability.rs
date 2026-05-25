use super::types::TrustLevel;
use serde::{Deserialize, Serialize};
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    FsReadAgentHome,
    FsWriteAgentHome,
    FsReadHost,
    FsWriteHost,
    NetworkOutbound,
    NetworkOutboundAllowlisted,
    SpawnAllowlisted,
    Spawn,
    Mcp,
    SkillReadOthers,
}

impl FromStr for Capability {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let v: serde_yaml_ng::Value = serde_yaml_ng::Value::String(s.to_string());
        serde_yaml_ng::from_value(v).map_err(|_| ())
    }
}

pub fn allowed_for(level: TrustLevel) -> &'static [Capability] {
    use Capability::*;
    match level {
        TrustLevel::Sandboxed => &[FsReadAgentHome, Mcp],
        TrustLevel::Verified => &[
            FsReadAgentHome,
            FsWriteAgentHome,
            NetworkOutboundAllowlisted,
            SpawnAllowlisted,
            Mcp,
        ],
        TrustLevel::Trusted => &[
            FsReadAgentHome,
            FsWriteAgentHome,
            FsReadHost,
            FsWriteHost,
            NetworkOutbound,
            NetworkOutboundAllowlisted,
            Spawn,
            SpawnAllowlisted,
            Mcp,
            SkillReadOthers,
        ],
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct CapabilityViolation {
    pub capability: Capability,
    pub trust_level: TrustLevel,
}

pub fn check_capabilities(
    declared: &[String],
    level: TrustLevel,
) -> Result<(), CapabilityViolation> {
    let allowed = allowed_for(level);
    for s in declared {
        let Ok(cap) = Capability::from_str(s) else {
            return Err(CapabilityViolation {
                capability: Capability::Mcp,
                trust_level: level,
            });
        };
        if !allowed.contains(&cap) {
            return Err(CapabilityViolation {
                capability: cap,
                trust_level: level,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandboxed_blocks_network() {
        let r = check_capabilities(&["network_outbound".into()], TrustLevel::Sandboxed);
        assert!(matches!(
            r,
            Err(CapabilityViolation {
                capability: Capability::NetworkOutbound,
                ..
            })
        ));
    }

    #[test]
    fn verified_allows_allowlisted_net() {
        let r = check_capabilities(
            &[
                "network_outbound_allowlisted".into(),
                "fs_write_agent_home".into(),
            ],
            TrustLevel::Verified,
        );
        assert!(r.is_ok());
    }

    #[test]
    fn trusted_allows_everything_declared() {
        let r = check_capabilities(
            &["spawn".into(), "fs_write_host".into()],
            TrustLevel::Trusted,
        );
        assert!(r.is_ok());
    }

    #[test]
    fn unknown_capability_rejected() {
        let r = check_capabilities(&["nuke_from_orbit".into()], TrustLevel::Trusted);
        assert!(r.is_err());
    }

    #[test]
    fn empty_declarations_always_ok() {
        assert!(check_capabilities(&[], TrustLevel::Sandboxed).is_ok());
    }
}

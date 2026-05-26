//! Skill type enums.

use serde::{Deserialize, Serialize};

/// Which host(s) may load a skill. See spec §2.3.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum HostId {
    MurAgent,
    MurCommander,
    /// Default when `hosts:` is omitted — backward compatible.
    #[default]
    All,
    #[serde(untagged)]
    Custom(String),
}

/// Three-tier skill trust model. Mirrors mur-commander `trust/level.rs`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord, Default,
)]
#[serde(rename_all = "kebab-case")]
pub enum TrustLevel {
    /// Peer transfer, agent-generated, untrusted registry.
    #[default]
    Sandboxed,
    /// Registry-verified checksum match, community-reviewed.
    Verified,
    /// Built-in, user-promoted, or trusted-publisher-signed.
    Trusted,
}

/// Top-level skill category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Category {
    Context,
    Workflow,
    Command,
    Meta,
}

/// Exactly one content mode is populated; see spec §3.2.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContentMode {
    Context,
    Workflow,
    Command,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Priority {
    Low,
    #[default]
    Normal,
    High,
    Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Ord, PartialOrd)]
#[serde(rename_all = "snake_case")]
pub enum TriggerKind {
    Command,
    Keyword,
    SessionStart,
    Manual,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_id_serialises_kebab_case() {
        let yaml = serde_yaml_ng::to_string(&HostId::MurAgent).unwrap();
        assert_eq!(yaml.trim(), "mur-agent");
    }

    #[test]
    fn trust_level_ordering_matches_spec() {
        assert!(TrustLevel::Sandboxed < TrustLevel::Verified);
        assert!(TrustLevel::Verified < TrustLevel::Trusted);
    }

    #[test]
    fn host_id_default_is_all() {
        assert_eq!(HostId::default(), HostId::All);
    }
}

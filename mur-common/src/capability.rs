//! A `Capability`: a standalone-installable bundle of MCP server(s) + skill
//! refs + external-program requirements + suggested entitlements. Reuses the
//! existing agent primitives; installed into an agent's profile.

use crate::agent::McpServerEntry;
use crate::deps::ProgramDep;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Capability {
    pub name: String,
    pub version: String,
    pub description: String,
    #[serde(default)]
    pub mcp_servers: Vec<McpServerEntry>,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub requires_programs: Vec<ProgramDep>,
    #[serde(default)]
    pub entitlements: CapabilityEntitlements,
}

/// Suggested entitlements a capability requests at install; each list is
/// unioned into the agent's `Entitlements` after consent.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CapabilityEntitlements {
    #[serde(default)]
    pub spawn_programs: Vec<String>,
    #[serde(default)]
    pub network_hosts: Vec<String>,
    #[serde(default)]
    pub filesystem_read: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_yaml_round_trips() {
        let yaml = "\
name: demo
version: 1.0.0
description: a demo capability
skills:
  - foo-skill
requires_programs:
  - name: vlc
    detect:
      command: vlc
    reason: needed for playback
entitlements:
  network_hosts:
    - 127.0.0.1
";
        let c: Capability = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(c.name, "demo");
        assert_eq!(c.skills, vec!["foo-skill"]);
        assert_eq!(c.requires_programs.len(), 1);
        assert_eq!(c.requires_programs[0].name, "vlc");
        assert_eq!(c.entitlements.network_hosts, vec!["127.0.0.1"]);
        let back = serde_yaml_ng::to_string(&c).unwrap();
        let c2: Capability = serde_yaml_ng::from_str(&back).unwrap();
        assert_eq!(c, c2);
    }
}

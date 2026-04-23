//! Entitlement warnings + category presets.
//! P0a declares; P0b enforces.

use mur_common::agent::{NetworkOutboundMode, SpawnMode};
use mur_common::{AgentProfile, PersonaCategory};

#[derive(Debug, Clone, PartialEq)]
pub enum WarningKind {
    UnrestrictedNetwork,
    EmptyFilesystemDeny,
    OpenProcessSpawn,
    HighMemoryLimit,
    OverBroadFilesystemWrite,
}

#[derive(Debug, Clone)]
pub struct Warning {
    pub kind: WarningKind,
    pub message: String,
}

pub fn detect_warnings(profile: &AgentProfile) -> Vec<Warning> {
    let mut warnings = vec![];
    if profile.entitlements.network.outbound.mode == NetworkOutboundMode::Unrestricted {
        warnings.push(Warning {
            kind: WarningKind::UnrestrictedNetwork,
            message: "network.outbound.mode=unrestricted — no outbound host filtering".to_string(),
        });
    }
    if profile.entitlements.filesystem.deny.is_empty() {
        warnings.push(Warning {
            kind: WarningKind::EmptyFilesystemDeny,
            message: "filesystem.deny is empty — consider adding ~/.ssh, ~/.aws, etc".to_string(),
        });
    }
    if profile.entitlements.processes.spawn.mode == SpawnMode::Any {
        warnings.push(Warning {
            kind: WarningKind::OpenProcessSpawn,
            message: "processes.spawn.mode=any — agent may spawn arbitrary binaries".to_string(),
        });
    }
    if profile.entitlements.limits.memory_mb > 2048 {
        warnings.push(Warning {
            kind: WarningKind::HighMemoryLimit,
            message: format!(
                "limits.memory_mb={} exceeds 2048 — review",
                profile.entitlements.limits.memory_mb
            ),
        });
    }
    for write in &profile.entitlements.filesystem.write {
        if write.trim_end_matches('/') == "~" || write.trim_end_matches('/') == "{{agent_home}}/.."
        {
            warnings.push(Warning {
                kind: WarningKind::OverBroadFilesystemWrite,
                message: format!("filesystem.write='{write}' is dangerously broad"),
            });
        }
    }
    warnings
}

#[derive(Debug, Clone)]
pub struct EntitlementPreset {
    pub network_mode: NetworkOutboundMode,
    pub network_hosts: Vec<String>,
    pub process_allowed: Vec<String>,
    pub filesystem_read_extras: Vec<String>,
    pub filesystem_write_extras: Vec<String>,
}

pub fn preset_for_category(cat: PersonaCategory) -> EntitlementPreset {
    match cat {
        PersonaCategory::Research => EntitlementPreset {
            network_mode: NetworkOutboundMode::Restricted,
            network_hosts: vec![],
            process_allowed: vec!["agent-browser".to_string(), "npx".to_string()],
            filesystem_read_extras: vec![],
            filesystem_write_extras: vec![],
        },
        PersonaCategory::Commerce => EntitlementPreset {
            network_mode: NetworkOutboundMode::Restricted,
            network_hosts: vec!["*.shopify.com".to_string(), "api.stripe.com".to_string()],
            process_allowed: vec!["agent-browser".to_string(), "npx".to_string()],
            filesystem_read_extras: vec![],
            filesystem_write_extras: vec!["~/Downloads/receipts".to_string()],
        },
        PersonaCategory::Notify => EntitlementPreset {
            network_mode: NetworkOutboundMode::Unrestricted,
            network_hosts: vec![],
            process_allowed: vec![],
            filesystem_read_extras: vec![],
            filesystem_write_extras: vec![],
        },
        PersonaCategory::Monitor => EntitlementPreset {
            network_mode: NetworkOutboundMode::Restricted,
            network_hosts: vec![],
            process_allowed: vec![],
            filesystem_read_extras: vec!["/var/log".to_string()],
            filesystem_write_extras: vec![],
        },
        PersonaCategory::Automation | PersonaCategory::Custom => EntitlementPreset {
            network_mode: NetworkOutboundMode::Restricted,
            network_hosts: vec![],
            process_allowed: vec![],
            filesystem_read_extras: vec![],
            filesystem_write_extras: vec![],
        },
    }
}

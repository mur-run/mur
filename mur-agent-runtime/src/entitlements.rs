//! Entitlement warnings + category presets.
//! P0a declares; P0b enforces.

use std::path::Path;

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

/// Whether a member may WRITE under `dir`, decided the way its own tool gate
/// will decide it (`fs_policy::check_write_entitlement`): `deny` is literal
/// and checked first; `write` is tried literally, then through one derived
/// worktree hop (#004). Exposed so a dispatcher (`mur fleet run`,
/// `parallel_jobs`) can ask BEFORE fan-out and not learn the answer from a
/// member that built in the wrong tree (#1607).
///
/// `dir` is canonicalized here; a path that does not exist is `Missing`, not
/// `NotGranted` — offering to grant a directory that is not there would be
/// accepted and still dropped by the sandbox at start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteVerdict {
    /// Under a `write` root (or a worktree of one) and not under `deny`.
    Allowed,
    /// Under an explicit `deny` root. Never grant past this.
    Denied,
    /// Outside every `write` root; a grant would make it `Allowed`.
    NotGranted,
    /// `dir` does not exist (or cannot be canonicalized).
    Missing,
}

pub fn write_verdict(fs: &mur_common::agent::FilesystemEntitlement, dir: &Path) -> WriteVerdict {
    let Ok(canonical) = std::fs::canonicalize(dir) else {
        return WriteVerdict::Missing;
    };
    if crate::tools::fs_policy::under_any(&fs.deny, &canonical) {
        return WriteVerdict::Denied;
    }
    if crate::tools::fs_policy::under_any_or_worktree(&fs.write, &canonical) {
        return WriteVerdict::Allowed;
    }
    WriteVerdict::NotGranted
}

#[cfg(test)]
mod write_verdict_tests {
    use super::*;
    use mur_common::agent::FilesystemEntitlement;

    fn fs(write: &[&str], deny: &[&str]) -> FilesystemEntitlement {
        FilesystemEntitlement {
            read: vec![],
            write: write.iter().map(|s| s.to_string()).collect(),
            deny: deny.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn verdict_follows_the_tool_gate_order() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("proj");
        let secret = proj.join("secret");
        std::fs::create_dir_all(&secret).unwrap();
        let root = proj.to_string_lossy().to_string();
        let sec = secret.to_string_lossy().to_string();

        assert_eq!(
            write_verdict(&fs(&[&root], &[]), &proj),
            WriteVerdict::Allowed
        );
        assert_eq!(
            write_verdict(&fs(&[], &[]), &proj),
            WriteVerdict::NotGranted
        );
        // deny beats write, and is literal
        assert_eq!(
            write_verdict(&fs(&[&root], &[&sec]), &secret),
            WriteVerdict::Denied
        );
        assert_eq!(
            write_verdict(&fs(&[&root], &[&sec]), &proj),
            WriteVerdict::Allowed
        );
        assert_eq!(
            write_verdict(&fs(&[&root], &[]), &proj.join("nope")),
            WriteVerdict::Missing
        );
    }
}

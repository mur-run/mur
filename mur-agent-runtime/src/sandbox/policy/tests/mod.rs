use super::*;
use mur_common::agent::{
    Entitlements, FilesystemEntitlement, InboundNetwork, NetworkEntitlement, NetworkOutboundMode,
    OutboundNetwork, ProcessesEntitlement, SpawnEntitlement, SpawnMode,
};

fn minimal_entitlements() -> Entitlements {
    Entitlements {
        network: NetworkEntitlement {
            inbound: InboundNetwork { ports: vec![] },
            outbound: OutboundNetwork {
                mode: NetworkOutboundMode::Restricted,
                allow_hosts: vec!["api.anthropic.com".to_string()],
                allow_ports: vec![],
                protocols: vec!["tcp".to_string()],
                resolve_dns: Default::default(),
            },
        },
        filesystem: FilesystemEntitlement {
            read: vec!["~/Documents".to_string()],
            write: vec!["~/Downloads".to_string()],
            deny: vec!["~/.ssh".to_string()],
        },
        processes: ProcessesEntitlement {
            spawn: SpawnEntitlement {
                mode: SpawnMode::Allowlist,
                allowed: vec![],
                allowed_dirs: vec![],
            },
        },
        syscalls: Default::default(),
        limits: Default::default(),
        llm: Default::default(),
        tools: vec![],
        fail_closed_on_sandbox_error: true,
    }
}

#[cfg(not(target_os = "windows"))]
fn make_fake_executable(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, b"#!/bin/sh\nexit 0\n").expect("write fake executable");
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).expect("chmod fake executable");
    path
}

mod fs_grants;
mod network;
mod resolve;

/// Verify SBPL profile generation — we only check the string content,
/// not apply it, so this is safe in the test process.
#[test]
#[cfg(target_os = "macos")]
fn macos_sbpl_contains_deny_for_ssh() {
    use mur_agent_runtime::sandbox::macos::build_sbpl_profile;
    use mur_agent_runtime::sandbox::SandboxPolicy;

    let mut policy = SandboxPolicy::default();
    policy.fs_deny.push(dirs::home_dir().unwrap().join(".ssh"));
    let sbpl = build_sbpl_profile(&policy);
    assert!(sbpl.contains("deny file-write*"), "SBPL must deny writes");
    assert!(sbpl.contains(".ssh"), "SBPL must mention the denied path");
}

/// Verify that the Landlock layer compiles and SandboxPolicy paths are all absolute.
/// Does NOT call restrict_self() to avoid locking the test process.
#[test]
#[cfg(target_os = "linux")]
fn linux_ruleset_paths_are_absolute() {
    use mur_agent_runtime::sandbox::policy::SandboxPolicy;
    use mur_common::agent::{
        Entitlements, FilesystemEntitlement, NetworkOutboundMode, OutboundNetwork,
        NetworkEntitlement, InboundNetwork, ProcessesEntitlement, SpawnEntitlement, SpawnMode,
    };
    use std::path::PathBuf;

    let ent = Entitlements {
        network: NetworkEntitlement {
            inbound: InboundNetwork { ports: vec![] },
            outbound: OutboundNetwork {
                mode: NetworkOutboundMode::Restricted,
                allow_hosts: vec!["api.anthropic.com".to_string()],
                protocols: vec!["tcp".to_string()],
                resolve_dns: Default::default(),
            },
        },
        filesystem: FilesystemEntitlement {
            read: vec![],
            write: vec![],
            deny: vec![],
        },
        processes: ProcessesEntitlement {
            spawn: SpawnEntitlement { mode: SpawnMode::Any, allowed: vec![] },
        },
        syscalls: Default::default(),
        limits: Default::default(),
        llm: Default::default(),
    };

    let agent_home = PathBuf::from("/tmp/b1_test_agent");
    std::fs::create_dir_all(&agent_home).unwrap();
    let policy = SandboxPolicy::from_entitlements(&ent, &agent_home);
    for p in &policy.fs_read {
        assert!(p.is_absolute(), "fs_read path must be absolute: {p:?}");
    }
    for p in &policy.fs_write {
        assert!(p.is_absolute(), "fs_write path must be absolute: {p:?}");
    }
}

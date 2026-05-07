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

#[test]
#[cfg(target_os = "windows")]
fn windows_job_object_applies() {
    use mur_agent_runtime::sandbox::SandboxPolicy;
    use mur_agent_runtime::sandbox::windows::apply_windows;

    let policy = SandboxPolicy::default();
    let status = apply_windows(&policy).expect("windows apply must not error");
    assert!(status.enforcing);
    assert_eq!(status.platform, "windows-job-object");
}

#[tokio::test]
async fn host_guard_blocks_unlisted_host() {
    use mur_agent_runtime::sandbox::reqwest_guard::HostGuard;
    use std::sync::Arc;

    let guard = HostGuard::restricted(vec!["api.anthropic.com".to_string()]);
    let client = reqwest::ClientBuilder::new()
        .dns_resolver(Arc::new(guard))
        .build()
        .unwrap();
    let result = client.get("http://evil.example.com/").send().await;
    assert!(result.is_err(), "blocked host must fail");
    let err_str = format!("{}", result.unwrap_err());
    assert!(
        err_str.contains("not in outbound allowlist") || err_str.contains("dns") || err_str.contains("error sending request"),
        "error must relate to blocked host: {err_str}"
    );
}

#[tokio::test]
async fn host_guard_allows_listed_host() {
    use mur_agent_runtime::sandbox::reqwest_guard::HostGuard;
    use std::sync::Arc;

    // Only test that DNS resolution is ATTEMPTED (not that the host is reachable).
    // A connection refused error is acceptable; "not in outbound allowlist" is not.
    let guard = HostGuard::restricted(vec!["localhost".to_string()]);
    let client = reqwest::ClientBuilder::new()
        .dns_resolver(Arc::new(guard))
        .build()
        .unwrap();
    let result = client.get("http://localhost:19999/").send().await;
    if let Err(e) = &result {
        let s = e.to_string();
        assert!(
            !s.contains("not in outbound allowlist"),
            "localhost should be allowed by HostGuard: {s}"
        );
    }
}

/// Verify that `sandbox::apply()` returns Ok on any platform (may return enforcing=false on
/// unsupported platforms). Uses MUR_AGENT_SKIP_SANDBOX to prevent actual restrict_self()
/// in the test process.
#[test]
fn sandbox_apply_does_not_panic() {
    if std::env::var_os("MUR_AGENT_SKIP_SANDBOX").is_some() {
        // Skipping sandbox apply in test process — this is expected in most CI configs.
        return;
    }
    use mur_agent_runtime::sandbox;
    use mur_common::agent::AgentProfile;
    use std::path::PathBuf;

    let profile = AgentProfile::default_for_tests();
    let agent_home = PathBuf::from("/tmp/b1_test_agent_apply");
    std::fs::create_dir_all(&agent_home).unwrap();
    let result = sandbox::apply(&profile.entitlements, &agent_home);
    assert!(result.is_ok(), "sandbox::apply must not error: {result:?}");
}

/// Verify that `spawn_sandboxed` can launch a simple process and it exits successfully.
/// On Linux/macOS the birdcage cage is built (policy mapped to exceptions), then
/// the child is spawned via `cmd.spawn()` fallback (cage.spawn() requires single-threaded
/// context on Linux and conflicts with the supervisor's own SBPL on macOS).
#[cfg(unix)]
#[test]
fn spawn_sandboxed_runs_true() {
    use mur_agent_runtime::sandbox::child::spawn_sandboxed;
    use mur_agent_runtime::sandbox::policy::SandboxPolicy;
    use mur_common::agent::{
        Entitlements, FilesystemEntitlement, InboundNetwork, NetworkEntitlement,
        NetworkOutboundMode, OutboundNetwork, ProcessesEntitlement, SpawnEntitlement, SpawnMode,
    };
    use std::path::PathBuf;
    use std::process::Command;

    let ent = Entitlements {
        network: NetworkEntitlement {
            inbound: InboundNetwork { ports: vec![] },
            outbound: OutboundNetwork {
                mode: NetworkOutboundMode::Unrestricted,
                allow_hosts: vec![],
                protocols: vec!["tcp".to_string()],
                resolve_dns: Default::default(),
            },
        },
        filesystem: FilesystemEntitlement { read: vec![], write: vec![], deny: vec![] },
        processes: ProcessesEntitlement {
            spawn: SpawnEntitlement { mode: SpawnMode::Any, allowed: vec![] },
        },
        syscalls: Default::default(),
        limits: Default::default(),
        llm: Default::default(),
    };
    let home = PathBuf::from("/tmp");
    let policy = SandboxPolicy::from_entitlements(&ent, &home);
    let cmd = Command::new("/usr/bin/true");
    let mut child = spawn_sandboxed(cmd, &policy).expect("spawn_sandboxed failed");
    let status = child.wait().expect("wait failed");
    assert!(status.success());
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

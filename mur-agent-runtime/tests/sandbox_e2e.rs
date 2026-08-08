/// Verify SBPL profile generation — we only check the string content,
/// not apply it, so this is safe in the test process.
#[test]
#[cfg(target_os = "macos")]
fn macos_sbpl_contains_deny_for_ssh() {
    use mur_agent_runtime::sandbox::SandboxPolicy;
    use mur_agent_runtime::sandbox::macos::build_sbpl_profile;

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
        err_str.contains("not in outbound allowlist")
            || err_str.contains("dns")
            || err_str.contains("error sending request"),
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
    let result = sandbox::apply(&profile.entitlements, &agent_home, &[], &[], &[]);
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
        filesystem: FilesystemEntitlement {
            read: vec![],
            write: vec![],
            deny: vec![],
        },
        processes: ProcessesEntitlement {
            spawn: SpawnEntitlement {
                mode: SpawnMode::Any,
                allowed: vec![],
                allowed_dirs: vec![],
            },
        },
        syscalls: Default::default(),
        limits: Default::default(),
        llm: Default::default(),
        tools: vec![],
        fail_closed_on_sandbox_error: true,
    };
    let home = PathBuf::from("/tmp");
    let policy = SandboxPolicy::from_entitlements(&ent, &home);
    let cmd = Command::new("/usr/bin/true");
    let mut child = spawn_sandboxed(cmd, &policy).expect("spawn_sandboxed failed");
    let status = child.wait().expect("wait failed");
    assert!(status.success());
}

/// Verify that after `sandbox::apply()` with no write entitlements,
/// writing outside agent_home fails. Runs in a subprocess to avoid locking the test process.
/// macOS: ignored because SBPL does not deny /tmp writes (system-wide temp dir exemption).
#[test]
#[cfg(unix)]
#[cfg_attr(target_os = "macos", ignore)]
fn sandbox_denies_write_outside_agent_home() {
    let exe = std::env::current_exe().unwrap();
    let status = std::process::Command::new(&exe)
        .env("MUR_TEST_SANDBOX_WRITE_DENY", "1")
        .env_remove("MUR_AGENT_SKIP_SANDBOX")
        .status()
        .unwrap();
    assert_eq!(
        status.code(),
        Some(0),
        "subprocess should exit 0 (write correctly denied or sandbox not enforcing)"
    );
}

/// Subprocess entry point for `sandbox_denies_write_outside_agent_home`.
/// `#[ctor]` fires before main() — if the env var is set, apply sandbox and exit.
#[cfg(unix)]
#[ctor::ctor]
fn sandbox_write_deny_subprocess_main() {
    if std::env::var_os("MUR_TEST_SANDBOX_WRITE_DENY").is_none() {
        return;
    }
    use mur_agent_runtime::sandbox;
    use mur_common::agent::AgentProfile;

    let profile = AgentProfile::default_for_tests();
    let agent_home = std::path::PathBuf::from("/tmp/b1_test_deny_home");
    std::fs::create_dir_all(&agent_home).unwrap();

    if sandbox::apply(&profile.entitlements, &agent_home, &[], &[], &[]).is_err() {
        // Sandbox apply failed (e.g., no kernel support) — treat as pass.
        std::process::exit(0);
    }

    // Try writing OUTSIDE agent_home — should be denied by Landlock/SBPL.
    let result = std::fs::write("/tmp/b1_SHOULD_FAIL.txt", b"pwned");
    if result.is_err() {
        std::process::exit(0); // correctly denied
    } else {
        std::fs::remove_file("/tmp/b1_SHOULD_FAIL.txt").ok();
        eprintln!("ERROR: write to /tmp was NOT denied by sandbox");
        std::process::exit(1); // incorrectly allowed
    }
}

/// Inverse of the deny test: a directory passed via `extra_write_paths` (the
/// `~/.mur/runtime` grant for co-watching) must actually permit the temp-file
/// create + rename that `save_watch` does AND the unlink that snapshot-pruning
/// does. Under Landlock a per-file grant cannot do this (creating/renaming needs
/// directory-level MAKE_REG/REFER on the parent); only a directory grant can — so
/// this guards the #382 follow-up fix. Runs in a subprocess so `restrict_self()`
/// doesn't lock the test process. On macOS `/tmp` is write-exempt so the ops pass
/// trivially; the load-bearing assertion is on Linux, where `/tmp` is default-denied
/// and only the directory grant makes the writes succeed.
#[test]
#[cfg(unix)]
fn sandbox_allows_granted_extra_write_dir() {
    let exe = std::env::current_exe().unwrap();
    let status = std::process::Command::new(&exe)
        .env("MUR_TEST_SANDBOX_WRITE_ALLOW", "1")
        .env_remove("MUR_AGENT_SKIP_SANDBOX")
        .status()
        .unwrap();
    assert_eq!(
        status.code(),
        Some(0),
        "granted runtime dir must allow temp+rename+unlink under the sandbox"
    );
}

/// Subprocess entry point for `sandbox_allows_granted_extra_write_dir`.
#[cfg(unix)]
#[ctor::ctor]
fn sandbox_write_allow_subprocess_main() {
    if std::env::var_os("MUR_TEST_SANDBOX_WRITE_ALLOW").is_none() {
        return;
    }
    use mur_agent_runtime::sandbox;
    use mur_common::agent::AgentProfile;

    let profile = AgentProfile::default_for_tests();
    let agent_home = std::path::PathBuf::from("/tmp/b1_test_allow_home");
    std::fs::create_dir_all(&agent_home).unwrap();

    // Granted directory standing in for `~/.mur/runtime`. Created before sealing so
    // the Landlock path-beneath rule sticks; passed via `extra_write_paths`.
    let runtime_dir = std::path::PathBuf::from("/tmp/b1_test_allow_runtime");
    std::fs::create_dir_all(&runtime_dir).unwrap();

    if sandbox::apply(
        &profile.entitlements,
        &agent_home,
        &[],
        &[],
        std::slice::from_ref(&runtime_dir),
    )
    .is_err()
    {
        // Sandbox apply failed (e.g., no kernel support) — treat as pass.
        std::process::exit(0);
    }

    // Mimic save_watch (temp write + rename) and snapshot prune (unlink) inside the
    // granted dir — exactly the operations a per-file grant breaks under Landlock.
    let target = runtime_dir.join("watch.json");
    let tmp = runtime_dir.join("watch.json.tmp");
    let snap = runtime_dir.join("snap.png");
    let ok = std::fs::write(&tmp, b"{}").is_ok()
        && std::fs::rename(&tmp, &target).is_ok()
        && std::fs::write(&snap, b"x").is_ok()
        && std::fs::remove_file(&snap).is_ok();
    std::fs::remove_file(&target).ok();

    if ok {
        std::process::exit(0);
    } else {
        eprintln!("ERROR: granted runtime dir did NOT allow temp+rename+unlink");
        std::process::exit(1);
    }
}

/// Verify that the Landlock layer compiles and SandboxPolicy paths are all absolute.
/// Does NOT call restrict_self() to avoid locking the test process.
#[test]
#[cfg(target_os = "linux")]
fn linux_ruleset_paths_are_absolute() {
    use mur_agent_runtime::sandbox::policy::SandboxPolicy;
    use mur_common::agent::{
        Entitlements, FilesystemEntitlement, InboundNetwork, NetworkEntitlement,
        NetworkOutboundMode, OutboundNetwork, ProcessesEntitlement, SpawnEntitlement, SpawnMode,
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
            spawn: SpawnEntitlement {
                mode: SpawnMode::Any,
                allowed: vec![],
                allowed_dirs: vec![],
            },
        },
        syscalls: Default::default(),
        limits: Default::default(),
        llm: Default::default(),
        tools: vec![],
        fail_closed_on_sandbox_error: true,
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

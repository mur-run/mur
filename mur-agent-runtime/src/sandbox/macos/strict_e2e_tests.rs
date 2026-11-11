//! #2343: `strict` built end to end through `SandboxPolicy::from_entitlements`,
//! not a hand-built policy with an empty `fs_exec`.
use super::*;
use mur_common::agent::{Entitlements, SpawnMode};
use std::path::{Path, PathBuf};

fn policy_for(mode: SpawnMode, allowed: &[&str]) -> SandboxPolicy {
    let mut ent: Entitlements = serde_yaml_ng::from_str(
        "network: {inbound: {ports: []}, outbound: {mode: restricted, allow_hosts: [], allow_ports: [], protocols: [tcp]}}\n\
         filesystem: {read: [], write: [], deny: []}\n\
         processes: {spawn: {mode: strict, allowed: [], allowed_dirs: []}}\n",
    )
    .expect("entitlements yaml");
    ent.processes.spawn.mode = mode;
    ent.processes.spawn.allowed = allowed.iter().map(|s| s.to_string()).collect();
    SandboxPolicy::from_entitlements(&ent, &PathBuf::from("/tmp/agent_home_2343"))
}

#[test]
fn strict_with_empty_allowlist_fences_system_binaries() {
    let policy = strict_policy(&[]);
    assert!(
        exec_permitted(&policy, Path::new("/bin/bash")),
        "shell must stay runnable"
    );
    let leaked: Vec<&str> = [
        "/usr/bin/git",
        "/usr/bin/curl",
        "/usr/bin/ssh",
        "/bin/ls",
        "/usr/bin/env",
    ]
    .into_iter()
    .filter(|p| exec_permitted(&policy, Path::new(p)))
    .collect();
    assert!(
        leaked.is_empty(),
        "strict leaked system binaries: {leaked:?}"
    );
}

#[test]
fn strict_allowing_ssh_opens_ssh_but_not_its_directory() {
    let policy = strict_policy(&["ssh"]);
    assert!(exec_permitted(&policy, Path::new("/usr/bin/ssh")));
    let leaked: Vec<&str> = ["/usr/bin/git", "/usr/bin/scp", "/bin/ls"]
        .into_iter()
        .filter(|p| exec_permitted(&policy, Path::new(p)))
        .collect();
    assert!(leaked.is_empty(), "allowing ssh widened to: {leaked:?}");
}

#[test]
fn strict_fs_exec_holds_no_system_dir() {
    let policy = strict_policy(&[]);
    assert!(
        policy.fs_exec.is_empty(),
        "strict must not grant whole exec dirs: {:?}",
        policy.fs_exec
    );
}

/// Landlock has no `path-literal`: it installs `exec_grants()` as `Execute`
/// rules. Under strict that list must be the literals and prefixes, never a
/// system exec directory (#2343).
#[test]
fn strict_exec_grants_for_landlock_are_literals_only() {
    let policy = strict_policy(&["ssh"]);
    let grants = policy.exec_grants();
    assert!(grants.iter().any(|p| p.ends_with("bash")), "{grants:?}");
    assert!(grants.iter().any(|p| p.ends_with("ssh")), "{grants:?}");
    for dir in ["/bin", "/usr/bin", "/usr/local/bin"] {
        assert!(
            !grants.contains(&PathBuf::from(dir)),
            "strict Landlock exec grants include {dir}: {grants:?}"
        );
    }
}

fn strict_policy(allowed: &[&str]) -> SandboxPolicy {
    policy_for(SpawnMode::Strict, allowed)
}

/// Out of scope for #2343: the other fenced modes keep the system exec dirs.
#[test]
fn allowlist_mode_keeps_system_exec_dirs() {
    let policy = policy_for(SpawnMode::Allowlist, &[]);
    assert!(policy.fs_exec.contains(&PathBuf::from("/usr/bin")));
    assert!(exec_permitted(&policy, Path::new("/usr/bin/git")));
    assert_eq!(policy.exec_grants(), policy.fs_exec);
}

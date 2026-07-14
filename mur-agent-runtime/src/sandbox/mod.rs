pub mod child;
pub mod egress_proxy;
pub mod policy;
pub mod reqwest_guard;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "windows")]
pub mod windows;

pub use policy::SandboxPolicy;

use mur_common::agent::Entitlements;
use std::path::Path;
use std::sync::OnceLock;

static SANDBOX_STATUS: OnceLock<SandboxStatus> = OnceLock::new();

/// Returns the `SandboxStatus` from the most recent `apply()` call,
/// or `None` if `apply()` has not been called.
pub fn last_status() -> Option<&'static SandboxStatus> {
    SANDBOX_STATUS.get()
}

#[derive(Debug, Clone)]
pub struct SandboxStatus {
    pub platform: String,
    pub effective_abi: Option<u32>,
    pub enforcing: bool,
}

/// Apply the kernel sandbox derived from `entitlements` to the current process.
/// Must be called once, early in `supervisor::entrypoint()`, after profile load.
pub fn apply(
    entitlements: &Entitlements,
    agent_home: &Path,
    extra_ports: &[u16],
    loopback_ports: &[u16],
    extra_write_paths: &[std::path::PathBuf],
) -> anyhow::Result<SandboxStatus> {
    let mut policy = SandboxPolicy::from_entitlements(entitlements, agent_home);
    // An agent must always be able to reach its own configured local LLM.
    policy.allow_extra_ports(extra_ports);
    // …and the pre-seal loopback egress proxy its MCP children dial.
    policy.allow_loopback_ports(loopback_ports);
    // The proxy lives IN this process: when it exists, the self profile must
    // keep general TCP open for its upstream dials or every granted child
    // egress dies with EPERM after CONNECT ALLOW (ProxyOnly regression).
    // Child profiles are built separately and stay strict.
    if !loopback_ports.is_empty() {
        policy.allow_in_process_proxy_upstream();
    }
    // …and to write the shared runtime media state it owns (co-watching:
    // watch.json + VLC snapshot dir), which lives outside agent_home.
    policy.allow_extra_write_paths(extra_write_paths);
    let status = apply_policy(&policy)?;
    // Store for attestation. OnceLock: if called twice, second call is ignored.
    let _ = SANDBOX_STATUS.set(status.clone());
    Ok(status)
}

#[cfg(target_os = "linux")]
fn apply_policy(policy: &SandboxPolicy) -> anyhow::Result<SandboxStatus> {
    linux::apply_linux(policy)
}

#[cfg(target_os = "macos")]
fn apply_policy(policy: &SandboxPolicy) -> anyhow::Result<SandboxStatus> {
    macos::apply_macos(policy)
}

#[cfg(target_os = "windows")]
fn apply_policy(policy: &SandboxPolicy) -> anyhow::Result<SandboxStatus> {
    windows::apply_windows(policy)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn apply_policy(_policy: &SandboxPolicy) -> anyhow::Result<SandboxStatus> {
    Ok(SandboxStatus {
        platform: "unsupported".to_string(),
        effective_abi: None,
        enforcing: false,
    })
}

/// Sandbox error type — surfaced to the LLM via `HookError::Sandboxed` (wired in Task 6).
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct SandboxedError {
    pub path: String,
    pub op: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_status_api_is_accessible() {
        // last_status() returns None before apply() or Some after.
        // This test just verifies the API compiles and is callable.
        let _ = last_status();
    }
}

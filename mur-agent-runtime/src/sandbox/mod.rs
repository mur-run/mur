pub mod child;
pub mod policy;
pub mod reqwest_guard;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "windows")]
mod windows;

pub use policy::SandboxPolicy;

use std::path::Path;
use std::sync::OnceLock;
use mur_common::agent::Entitlements;

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
pub fn apply(entitlements: &Entitlements, agent_home: &Path) -> anyhow::Result<SandboxStatus> {
    let policy = SandboxPolicy::from_entitlements(entitlements, agent_home);
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

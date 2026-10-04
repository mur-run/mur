pub mod child;
pub mod egress_proxy;
pub mod launch_chain;
pub mod policy;
pub mod reqwest_guard;
pub mod search_dirs;

// Unconditional: `partition_write_grants` is shared with sandbox::policy on every
// platform; only the apply path inside is linux-gated.
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
    /// Filesystem grants the policy discarded while being built, so they never
    /// reached the kernel however they read in `profile.yaml`.
    ///
    /// Platform backends leave this empty — they receive an already-built
    /// policy and never see what was discarded producing it. [`apply`] fills it
    /// from the policy immediately after they return.
    pub dropped: Vec<mur_common::agent::DroppedGrant>,
}

/// Env var set to `1` on MCP children spawned while the seal enforces. Tools
/// that bring their own sandbox (Chromium) read it to stand theirs down, since
/// a nested one cannot start. Must match `mur_browser::chromium::SEALED_ENV`.
pub const SEALED_ENV: &str = "MUR_SEALED";

/// The env pair telling a child it runs inside an enforcing seal, if it does.
/// An advisory-only run (apply failed, fail-open) or no apply at all yields
/// nothing, so the child keeps its own sandbox there.
pub fn sealed_child_env(status: Option<&SandboxStatus>) -> Option<(&'static str, &'static str)> {
    status.filter(|s| s.enforcing).map(|_| (SEALED_ENV, "1"))
}

/// True when the current process already runs under a macOS seatbelt profile.
///
/// macOS refuses a second `sandbox_init` inside a sandboxed process (`EPERM`),
/// so tests that spawn a child which seals itself cannot pass from a sealed
/// shell such as a MUR agent session (#1697). They call this to skip loudly.
/// Asks the kernel rather than trusting an env marker: an inherited or missing
/// variable would either hide a real failure or miss the seal. Always `false`
/// off macOS, where nested sandboxes are permitted.
pub fn current_process_sealed() -> bool {
    #[cfg(target_os = "macos")]
    {
        macos::current_process_sealed()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
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
    let dropped = policy.dropped.clone();
    let mut status = apply_policy(&policy)?;
    status.dropped = dropped;
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
        dropped: Vec::new(),
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

    fn status(enforcing: bool) -> SandboxStatus {
        SandboxStatus {
            platform: "test".into(),
            effective_abi: None,
            enforcing,
            dropped: vec![],
        }
    }

    #[test]
    fn children_hear_about_the_seal_only_when_it_enforces() {
        assert_eq!(
            sealed_child_env(Some(&status(true))),
            Some((SEALED_ENV, "1"))
        );
        assert_eq!(sealed_child_env(Some(&status(false))), None);
        assert_eq!(sealed_child_env(None), None);
    }
}

//! Start-time ssh-agent proxy classification (plan Phase 1, Tasks 1.3–1.5).
//!
//! Decides once per start whether MUR may call the proxy a **boundary** for
//! this agent or only **audit-only**, records it where `mur agent status`
//! reads it, and logs it. The result never gates startup.
//!
//! Must run BEFORE the seal: macOS refuses a nested `sandbox_init`, so a probe
//! child spawned from a sealed supervisor could never install the profile and
//! would always come back inconclusive.

use crate::sandbox::SandboxPolicy;
use crate::sandbox::ssh_bypass::{AuditReason, Classification, Inputs, Platform, classify};
use mur_common::ssh_proxy_status::{self, SshProxyState, SshProxyStatus};
use std::path::{Path, PathBuf};

/// The upstream ssh-agent socket the agent itself would inherit. Read from
/// the runtime's own environment: that is the socket a child of this process
/// would see, which is exactly the one the classification is about.
pub(super) const UPSTREAM_ENV: &str = "SSH_AUTH_SOCK";

/// Classify this start, without persisting it. Must run before the seal (see
/// the module doc); the result is provisional until [`finalize`] has seen
/// whether the supervisor's own sandbox actually installed.
pub(super) fn classify_before_seal(has_ssh_keys: bool, policy: &SandboxPolicy) -> SshProxyStatus {
    let status = if has_ssh_keys {
        let upstream = std::env::var_os(UPSTREAM_ENV).map(PathBuf::from);
        let class = classify_at_start(policy, upstream.as_deref(), &mut live_probe);
        status_for(&class)
    } else {
        SshProxyStatus {
            state: SshProxyState::Off,
            code: None,
            explanation: "no entitlements.ssh keys; the proxy has nothing to guard".into(),
            at_unix_ms: 0,
        }
    };
    SshProxyStatus {
        at_unix_ms: now_unix_ms(),
        ..status
    }
}

/// Settle the classification against the sandbox the supervisor really got,
/// then log and persist it. The probe proves its child was confined, not that
/// this process is: when the seal failed or is advisory-only, a boundary is
/// downgraded. Best-effort: a write failure is logged, never fatal. Returns
/// what was recorded so the caller can put the same facts on the channel.
pub(super) fn finalize(
    agent_home: &Path,
    provisional: SshProxyStatus,
    supervisor_enforcing: bool,
) -> SshProxyStatus {
    let status = settle(provisional, supervisor_enforcing);
    log_status(&status);
    if let Err(e) = ssh_proxy_status::write(agent_home, &status) {
        tracing::warn!(error = %e, "could not record the ssh-agent proxy classification");
    }
    status
}

/// The pure part of [`finalize`]: never keep `boundary` for an unconfined
/// supervisor. Other states already claim nothing, so they pass through.
pub(super) fn settle(s: SshProxyStatus, supervisor_enforcing: bool) -> SshProxyStatus {
    if s.state != SshProxyState::Boundary || supervisor_enforcing {
        return s;
    }
    let r = AuditReason::SupervisorNotEnforcing;
    SshProxyStatus {
        state: SshProxyState::AuditOnly,
        code: Some(r.as_str().to_string()),
        explanation: explanation(&r),
        ..s
    }
}

/// Run the static table and, on `Candidate` only, confirm it with `probe`.
/// `probe` gets the agent's SBPL profile and the canonical upstream.
pub(super) fn classify_at_start(
    policy: &SandboxPolicy,
    upstream: Option<&Path>,
    probe: &mut dyn FnMut(&str, &Path) -> Classification,
) -> Classification {
    let Some(upstream) = upstream else {
        // Unset upstream is a row in the table, not an error.
        return Classification::AuditOnly(AuditReason::UpstreamUnresolved);
    };
    let (roots, patterns) = allow_rules(policy);
    let class = classify(Inputs {
        platform: current_platform(),
        net_allow_ports: policy.net_allow_ports.as_deref(),
        upstream,
        allow_roots: &roots,
        allow_patterns: patterns,
    });
    match class {
        Classification::Candidate { canonical_upstream } => {
            probe(&sbpl_profile(policy), &canonical_upstream)
        }
        other => other,
    }
}

fn current_platform() -> Platform {
    if cfg!(target_os = "macos") {
        Platform::MacOs
    } else if cfg!(target_os = "linux") {
        Platform::Linux
    } else if cfg!(target_os = "windows") {
        Platform::Windows
    } else {
        Platform::Other
    }
}

#[cfg(target_os = "macos")]
fn allow_rules(policy: &SandboxPolicy) -> (Vec<PathBuf>, &'static [&'static str]) {
    crate::sandbox::macos::unix_socket_allow_rules(policy)
}

/// Off macOS the table answers before it reads the allowlist.
#[cfg(not(target_os = "macos"))]
fn allow_rules(_policy: &SandboxPolicy) -> (Vec<PathBuf>, &'static [&'static str]) {
    (Vec::new(), &[])
}

#[cfg(target_os = "macos")]
fn sbpl_profile(policy: &SandboxPolicy) -> String {
    crate::sandbox::macos::build_sbpl_profile(policy)
}

#[cfg(not(target_os = "macos"))]
fn sbpl_profile(_policy: &SandboxPolicy) -> String {
    String::new()
}

/// The real probe: this runtime binary, re-run as the probe child.
#[cfg(unix)]
fn live_probe(profile: &str, upstream: &Path) -> Classification {
    use crate::sandbox::ssh_bypass::probe::{PROBE_TIMEOUT, ProbeOutcome, run_probe};
    let outcome = match std::env::current_exe() {
        Ok(exe) => run_probe(&exe, profile, upstream, PROBE_TIMEOUT),
        Err(e) => ProbeOutcome::Inconclusive(format!("no current_exe: {e}")),
    };
    if let ProbeOutcome::Inconclusive(why) = &outcome {
        tracing::warn!(reason = %why, "ssh-agent bypass probe was inconclusive");
    }
    outcome.classification()
}

#[cfg(not(unix))]
fn live_probe(_profile: &str, _upstream: &Path) -> Classification {
    Classification::AuditOnly(AuditReason::ProbeInconclusive)
}

/// Map a classification to what is persisted. `Candidate` cannot reach here
/// from `classify_at_start`; if it ever does it is unconfirmed, so audit-only.
pub(super) fn status_for(class: &Classification) -> SshProxyStatus {
    let (state, code, explanation) = match class {
        Classification::Boundary => (
            SshProxyState::Boundary,
            None,
            "a probe under this agent's sandbox could not connect(2) to the upstream \
             ssh-agent, so a proxy could be a boundary here; no proxy is active yet"
                .to_string(),
        ),
        Classification::AuditOnly(r) => (
            SshProxyState::AuditOnly,
            Some(r.as_str().to_string()),
            explanation(r),
        ),
        Classification::Candidate { .. } => (
            SshProxyState::AuditOnly,
            Some(AuditReason::ProbeInconclusive.as_str().to_string()),
            explanation(&AuditReason::ProbeInconclusive),
        ),
    };
    SshProxyStatus {
        state,
        code,
        explanation,
        at_unix_ms: 0,
    }
}

/// The one table of explanations, one per reason code.
pub(super) fn explanation(reason: &AuditReason) -> String {
    match reason {
        AuditReason::PlatformNoFsSandbox => {
            "no filesystem sandbox on this platform can stop the agent dialing the upstream ssh-agent".into()
        }
        AuditReason::PlatformNoUnixSocketControl => {
            "Landlock does not gate connect(2) on unix sockets, so the agent can dial the upstream ssh-agent".into()
        }
        AuditReason::UnrestrictedNetwork => {
            "network is unrestricted, so the agent can dial the upstream ssh-agent; restrict entitlements.network to make the proxy a boundary".into()
        }
        AuditReason::UpstreamUnresolved => {
            "SSH_AUTH_SOCK is unset or does not resolve, so MUR cannot tell whether the agent can reach it".into()
        }
        AuditReason::UpstreamInAllowlist { root } => format!(
            "the upstream ssh-agent socket is under {}, which this agent's sandbox allows",
            root.display()
        ),
        AuditReason::UpstreamMatchesAllowPattern => {
            "the upstream ssh-agent socket matches MUR's allowed short-socket pattern".into()
        }
        AuditReason::ProbeReachable => {
            "a probe under this agent's own sandbox connected to the upstream ssh-agent".into()
        }
        AuditReason::ProbeInconclusive => {
            "the sandbox probe gave no clean answer, so MUR does not claim a boundary".into()
        }
        AuditReason::SupervisorNotEnforcing => {
            "the probe was confined, but this agent's own sandbox is not enforcing \
             (advisory-only start), so the agent can dial the upstream ssh-agent"
                .into()
        }
    }
}

fn log_status(s: &SshProxyStatus) {
    let code = s.code.as_deref().unwrap_or("-");
    match (&s.state, code) {
        // Contradicts what the static table predicted: loudest.
        (SshProxyState::AuditOnly, "probe_reachable") => {
            tracing::error!(code, "ssh-agent proxy is audit-only: {}", s.explanation)
        }
        (SshProxyState::AuditOnly, _) => {
            tracing::warn!(code, "ssh-agent proxy is audit-only: {}", s.explanation)
        }
        _ => tracing::info!(state = ?s.state, "ssh-agent proxy: {}", s.explanation),
    }
}

fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
#[path = "ssh_proxy_class_tests.rs"]
mod tests;

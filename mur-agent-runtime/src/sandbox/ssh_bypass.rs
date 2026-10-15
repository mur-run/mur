//! Static bypass classification for the ssh-agent proxy.
//!
//! At agent start the runtime decides whether the proxy is a **boundary** for
//! this agent (the agent cannot `connect(2)` to the upstream ssh-agent socket
//! itself) or merely **audit-only** (it can, so the proxy's key and host
//! filters only bind an agent that cooperates). The classification never
//! changes proxy behaviour, only what MUR claims about it.
//!
//! This module is the static half: a pure function over its inputs, so every
//! row is testable on every host. A [`Classification::Candidate`] result is a
//! prediction, not a claim — "boundary" is only claimed after the live probe
//! confirms it. Every uncertain case resolves to audit-only, because the
//! dangerous error is claiming isolation that does not exist.

use regex::Regex;
use std::path::{Path, PathBuf};

/// The platform the agent runs on, passed in rather than read from `cfg!`
/// so the Windows and Linux rows are testable on any host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    MacOs,
    Linux,
    Windows,
    /// Any platform without a sandbox backend that controls unix-socket
    /// connects. Classified like Windows: nothing stops the agent.
    Other,
}

/// Why the proxy is audit-only for this agent. The `as_str` codes are the
/// stable identifiers surfaced in logs, the channel, and `mur agent status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuditReason {
    /// No filesystem sandbox that could gate the socket.
    PlatformNoFsSandbox,
    /// Landlock does not mediate `connect(2)` on pathname unix sockets
    /// (confirmed on a real kernel, #1893).
    PlatformNoUnixSocketControl,
    /// `net_allow_ports == None`: the profile has no outbound network deny.
    UnrestrictedNetwork,
    /// The upstream path could not be canonicalised, so it cannot be compared
    /// against the allowlist the kernel checks (by resolved path).
    UpstreamUnresolved,
    /// The resolved upstream lies under an allowlisted unix-socket root.
    UpstreamInAllowlist { root: PathBuf },
    /// The resolved upstream matches the allowlisted MUR short-socket pattern.
    /// Not a row in the design table; kept so no allow rule is ever skipped.
    UpstreamMatchesAllowPattern,
}

impl AuditReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::PlatformNoFsSandbox => "platform_no_fs_sandbox",
            Self::PlatformNoUnixSocketControl => "platform_no_unix_socket_control",
            Self::UnrestrictedNetwork => "unrestricted_network",
            Self::UpstreamUnresolved => "upstream_unresolved",
            Self::UpstreamInAllowlist { .. } => "upstream_in_allowlist",
            Self::UpstreamMatchesAllowPattern => "upstream_matches_allow_pattern",
        }
    }
}

/// Result of the static table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Classification {
    /// The agent can reach the upstream directly.
    AuditOnly(AuditReason),
    /// macOS, restricted network, upstream outside every allow rule. Still
    /// needs the live probe before MUR may claim "boundary".
    Candidate { canonical_upstream: PathBuf },
}

/// Inputs to the static table. The allowlist is the effective one the
/// sandbox emits (`unix_socket_allow_paths` on macOS), never a re-listed copy:
/// a duplicate would drift in the dangerous direction when that list changes.
#[derive(Debug, Clone, Copy)]
pub struct Inputs<'a> {
    pub platform: Platform,
    /// `SandboxPolicy.net_allow_ports`; `None` means unrestricted.
    pub net_allow_ports: Option<&'a [u16]>,
    /// The upstream socket path as resolved by the CLI (not yet canonical).
    pub upstream: &'a Path,
    /// Unix-socket `subpath` roots the sandbox allows.
    pub allow_roots: &'a [PathBuf],
    /// Unix-socket `path-regex` patterns the sandbox allows.
    pub allow_patterns: &'a [&'a str],
}

/// Evaluate the static table in order; first match wins.
pub fn classify(inputs: Inputs<'_>) -> Classification {
    use AuditReason::*;
    match inputs.platform {
        Platform::Windows | Platform::Other => {
            return Classification::AuditOnly(PlatformNoFsSandbox);
        }
        Platform::Linux => return Classification::AuditOnly(PlatformNoUnixSocketControl),
        Platform::MacOs => {}
    }
    if inputs.net_allow_ports.is_none() {
        return Classification::AuditOnly(UnrestrictedNetwork);
    }
    // The kernel matches the resolved path, so the comparison must too:
    // comparing the link location would let a symlink placed outside the
    // allowlist hide a target inside it.
    let Ok(canonical) = std::fs::canonicalize(inputs.upstream) else {
        return Classification::AuditOnly(UpstreamUnresolved);
    };
    if let Some(root) = inputs.allow_roots.iter().find(|r| canonical.starts_with(r)) {
        return Classification::AuditOnly(UpstreamInAllowlist { root: root.clone() });
    }
    let text = canonical.to_string_lossy();
    for pattern in inputs.allow_patterns {
        // An unparseable pattern cannot be ruled out, so it counts as a match.
        let matches = match Regex::new(pattern) {
            Ok(re) => re.is_match(&text),
            Err(_) => true,
        };
        if matches {
            return Classification::AuditOnly(UpstreamMatchesAllowPattern);
        }
    }
    Classification::Candidate {
        canonical_upstream: canonical,
    }
}

#[cfg(test)]
mod tests;

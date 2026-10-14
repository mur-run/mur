//! Static half of ssh-agent proxy bypass detection.
//!
//! Answers one question, conservatively: can this agent `connect(2)` to the
//! upstream ssh-agent socket the proxy itself dials? If it can, the proxy is
//! an audit point, not a boundary. Every uncertain case resolves to
//! [`Classification::AuditOnly`]; only a macOS agent with restricted network
//! and an upstream outside every unix-socket allowlist root comes out as
//! [`Classification::Candidate`], which the live probe must still confirm
//! before anything claims "boundary".
//!
//! Pure: the platform, network mode, and allowlist are all inputs, so the
//! table is testable row by row on every OS. The only I/O is
//! `canonicalize`, because SBPL matches the resolved path and so must we.

use std::path::{Path, PathBuf};

/// The OS whose sandbox the agent runs under. Supplied by the caller rather
/// than read here so every row of the table is testable on any host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    MacOs,
    Linux,
    Windows,
    /// Anything without a sandbox model for this question (e.g. FreeBSD).
    Other,
}

/// Why a classification is audit-only. `code()` is the stable string shown in
/// `mur agent status` and the channel event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reason {
    /// No filesystem sandbox that could hide the socket.
    PlatformNoFsSandbox,
    /// Landlock does not mediate pathname unix `connect` (verified, #1893).
    PlatformNoUnixSocketControl,
    /// `net_allow_ports == None`: no `network-outbound` deny at all.
    UnrestrictedNetwork,
    /// The upstream path could not be resolved, so it cannot be compared.
    UpstreamUnresolved,
    /// The resolved upstream sits under this allowlist root.
    UpstreamInAllowlist(PathBuf),
}

impl Reason {
    pub fn code(&self) -> &'static str {
        match self {
            Reason::PlatformNoFsSandbox => "platform_no_fs_sandbox",
            Reason::PlatformNoUnixSocketControl => "platform_no_unix_socket_control",
            Reason::UnrestrictedNetwork => "unrestricted_network",
            Reason::UpstreamUnresolved => "upstream_unresolved",
            Reason::UpstreamInAllowlist(_) => "upstream_in_allowlist",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Classification {
    AuditOnly(Reason),
    /// Predicted unreachable; not a boundary until the live probe says so.
    /// Carries the canonical upstream path the probe must dial.
    Candidate {
        upstream: PathBuf,
    },
}

/// Inputs to the static table. `allowlist` must be the sandbox's real
/// effective unix-socket allowlist (`macos::unix_socket_allow_paths`), never a
/// re-listed copy: a copy drifts in the dangerous direction.
pub struct Inputs<'a> {
    pub platform: Platform,
    pub upstream: &'a Path,
    pub net_allow_ports: Option<&'a [u16]>,
    pub allowlist: &'a [PathBuf],
}

/// Evaluate the static table; first match wins.
pub fn classify(inputs: &Inputs<'_>) -> Classification {
    match inputs.platform {
        Platform::Windows | Platform::Other => {
            return Classification::AuditOnly(Reason::PlatformNoFsSandbox);
        }
        Platform::Linux => {
            return Classification::AuditOnly(Reason::PlatformNoUnixSocketControl);
        }
        Platform::MacOs => {}
    }
    if inputs.net_allow_ports.is_none() {
        return Classification::AuditOnly(Reason::UnrestrictedNetwork);
    }
    let Ok(upstream) = inputs.upstream.canonicalize() else {
        return Classification::AuditOnly(Reason::UpstreamUnresolved);
    };
    for root in inputs.allowlist {
        if upstream.starts_with(root) {
            return Classification::AuditOnly(Reason::UpstreamInAllowlist(root.clone()));
        }
        // A root reached through a symlink (`/tmp` → `/private/tmp`) must
        // also be compared resolved, or a resolved upstream would slip past.
        if let Ok(resolved_root) = root.canonicalize()
            && upstream.starts_with(&resolved_root)
        {
            return Classification::AuditOnly(Reason::UpstreamInAllowlist(root.clone()));
        }
    }
    Classification::Candidate { upstream }
}

#[cfg(test)]
mod tests;

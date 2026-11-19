//! The agent's unix socket, bound BEFORE the seal (#2422).
//!
//! On Linux the Landlock seal only lets a bind through inside its write
//! grants. Two socket paths fall outside every grant: the long-path fallback
//! `/tmp/mur-<uuid>.sock` (see [`crate::socket_path`]) and a configured
//! `transport.socket.bind` outside `agent_home`. Binding here, while the
//! process is still unconfined, needs no new grant; the seal does not revoke
//! an already-bound socket. A bind that fails here fails the start, loudly,
//! instead of a `ready` agent with no socket.

use crate::profile::Profile;
use std::path::PathBuf;

/// A socket bound before the seal, served after it.
pub(super) struct PreBoundSocket {
    /// The path clients dial and the lock file records.
    pub(super) canonical: PathBuf,
    /// Where the listener really lives (differs on the long-path fallback).
    pub(super) bind_path: PathBuf,
    pub(super) listener: std::os::unix::net::UnixListener,
}

/// Whether the profile asks for a unix socket transport.
pub(super) fn socket_enabled(profile: &Profile) -> bool {
    let s = &profile.inner.transport.socket;
    s.enabled && s.bind.starts_with("unix://")
}

/// Resolve and bind the profile's unix socket. `Ok(None)` when disabled.
pub(super) fn bind_before_seal(profile: &Profile) -> anyhow::Result<Option<PreBoundSocket>> {
    if !socket_enabled(profile) {
        return Ok(None);
    }
    let canonical = PathBuf::from(
        profile
            .inner
            .transport
            .socket
            .bind
            .trim_start_matches("unix://"),
    );
    let res = crate::socket_path::resolve_bind_target(&canonical, &profile.inner.id)?;
    let listener = crate::transport::unix_socket::bind_unix_std(&res.bind_path).map_err(|e| {
        anyhow::anyhow!(
            "cannot bind the agent's unix socket at {} (transport.socket.bind = {}): {e}",
            res.bind_path.display(),
            profile.inner.transport.socket.bind
        )
    })?;
    Ok(Some(PreBoundSocket {
        canonical,
        bind_path: res.bind_path,
        listener,
    }))
}

//! Shared helpers for the MUR mobile pairing / LAN transport (P1).
//!
//! Used by both the daemon endpoint (`mur-daemon::mobile_server`) and the
//! `mur agent pair` CLI so the QR a user scans always matches the token, port,
//! and paths the daemon actually serves. The wire protocol itself lives in
//! `mur_common::mobile`. Design:
//! `docs/superpowers/specs/2026-06-05-mur-voice-mobile-app-design.md`.

use anyhow::{Context, Result};
use std::net::{IpAddr, UdpSocket};
use std::path::{Path, PathBuf};

/// Default LAN port for the mobile WebSocket endpoint (distinct from the
/// signal server's 9421). Override with `MUR_MOBILE_PORT`.
pub const DEFAULT_MOBILE_PORT: u16 = 9430;
/// Default bind address — `0.0.0.0` so a phone on the LAN can reach it.
/// Override with `MUR_MOBILE_BIND`.
pub const DEFAULT_MOBILE_BIND: &str = "0.0.0.0";
/// Agent the phone talks to when none is named (the concierge).
pub const DEFAULT_MOBILE_AGENT: &str = "mur";

const PAIR_TOKEN_FILE: &str = "mobile/pair-token";
const PAIRED_DEVICES_FILE: &str = "mobile/paired.json";

/// Effective mobile port, honouring the `MUR_MOBILE_PORT` override.
pub fn mobile_port() -> u16 {
    std::env::var("MUR_MOBILE_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_MOBILE_PORT)
}

/// Effective bind address, honouring the `MUR_MOBILE_BIND` override.
pub fn mobile_bind() -> String {
    std::env::var("MUR_MOBILE_BIND").unwrap_or_else(|_| DEFAULT_MOBILE_BIND.to_string())
}

/// Path to the one-time pairing token under `<home>/mobile/pair-token`.
pub fn pair_token_path(home: &Path) -> PathBuf {
    home.join(PAIR_TOKEN_FILE)
}

/// Path to the paired-device store under `<home>/mobile/paired.json`.
pub fn paired_devices_path(home: &Path) -> PathBuf {
    home.join(PAIRED_DEVICES_FILE)
}

/// Read the pairing token, generating and persisting one (mode 0600) on first
/// use. Both the daemon and the `pair` CLI call this so they agree.
pub fn ensure_pair_token(home: &Path) -> Result<String> {
    let path = pair_token_path(home);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if path.exists() {
        let token = std::fs::read_to_string(&path)
            .with_context(|| format!("read {}", path.display()))?;
        return Ok(token.trim().to_owned());
    }
    let token = uuid::Uuid::new_v4().to_string();
    std::fs::write(&path, &token).with_context(|| format!("write {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(token)
}

/// Best-effort primary LAN IP of this host. No traffic is sent — we open a UDP
/// socket "connected" to a public address and read which local interface the
/// OS would route through. Returns `None` if there is no usable route.
pub fn lan_ip() -> Option<IpAddr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    socket.local_addr().ok().map(|addr| addr.ip())
}

/// Build the pairing URI encoded into the QR. The phone parses host/port/token
/// and calls `connect_lan`. Token (uuid) and agent slug are URL-safe.
pub fn pairing_uri(host: &str, port: u16, token: &str, agent: &str) -> String {
    format!("mur-pair://{host}:{port}/?token={token}&agent={agent}")
}

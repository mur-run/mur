//! An agent's request for SSH access to one host (SSH setup journey, K1/K2).
//!
//! The agent writes it with the `ssh_request` tool; murmur, the Hub and the
//! CLI read it outside the sandbox and turn it into a card the user must
//! approve. A request is data: nothing in it grants anything.
//!
//! The agent home is writable by the agent, so it can also write these files
//! with `bash` and skip the tool. [`read_pending`] is therefore the
//! enforcement point, and no path is ever built from a request field except
//! the id, which the reader recomputes.

use std::net::{Ipv4Addr, Ipv6Addr};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use mur_common::agent::{ProcessesEntitlement, SpawnMode};

mod pinned;
mod store;
pub use store::{PendingRequest, Written, read_pending, remove, requests_dir, write};

/// Version of the request file. A reader drops a file whose schema it does not know.
pub const SCHEMA: u32 = 1;
/// Directory under the agent home holding one file per requested host.
pub const REQUESTS_DIR: &str = "ssh-requests";
/// Port used when the agent names none.
pub const DEFAULT_PORT: u16 = 22;
/// Longest reason kept, in characters.
pub const MAX_REASON_CHARS: usize = 500;
/// Largest request file read, in bytes.
pub const MAX_FILE_BYTES: u64 = 4 * 1024;
/// A request older than this is not listed.
pub const EXPIRY_SECS: i64 = 7 * 24 * 60 * 60;
/// Requests listed per agent, newest first.
pub const MAX_PENDING: usize = 8;
/// The binary an agent needs to exec to use an SSH grant.
pub const SSH_BINARY: &str = "ssh";

const MAX_HOST_LEN: usize = 253;
const MAX_LABEL_LEN: usize = 63;
const MAX_USER_LEN: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RequestError {
    #[error("invalid host {0:?}: expected a DNS name, an IPv4 address or an IPv6 address")]
    Host(String),
    #[error("invalid port {0}: expected 1-65535")]
    Port(u32),
    #[error("invalid login user {0:?}: expected a plain login name")]
    User(String),
    #[error("a reason is required")]
    EmptyReason,
}

/// A validated `host:port`. Only [`RequestHost::parse`] builds one.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RequestHost {
    host: String,
    port: u16,
}

impl RequestHost {
    pub fn parse(host: &str, port: Option<u16>) -> Result<Self, RequestError> {
        let port = port.unwrap_or(DEFAULT_PORT);
        if port == 0 {
            return Err(RequestError::Port(0));
        }
        let host = canonical_host(host).ok_or_else(|| RequestError::Host(host.to_string()))?;
        Ok(Self { host, port })
    }

    /// Lowercased; an IPv6 address in brackets.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// What `ssh` and `ssh-keyscan` take: an IPv6 address without brackets.
    pub fn connect_host(&self) -> &str {
        self.host
            .strip_prefix('[')
            .and_then(|h| h.strip_suffix(']'))
            .unwrap_or(&self.host)
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// `host:port`, the port always explicit.
    pub fn canonical(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    /// Lowercase hex SHA-256 of [`Self::canonical`]: the request's file name.
    pub fn id(&self) -> String {
        hex::encode(Sha256::digest(self.canonical().as_bytes()))
    }

    /// The name `known_hosts` stores this host under.
    pub fn known_hosts_name(&self) -> String {
        if self.port == DEFAULT_PORT {
            self.connect_host().to_string()
        } else {
            format!("[{}]:{}", self.connect_host(), self.port)
        }
    }
}

fn canonical_host(raw: &str) -> Option<String> {
    if let Some(inner) = raw.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
        return inner.parse::<Ipv6Addr>().ok().map(|a| format!("[{a}]"));
    }
    if let Ok(a) = raw.parse::<Ipv4Addr>() {
        return Some(a.to_string());
    }
    if let Ok(a) = raw.parse::<Ipv6Addr>() {
        return Some(format!("[{a}]"));
    }
    let h = raw.to_ascii_lowercase();
    if h.is_empty() || h.len() > MAX_HOST_LEN {
        return None;
    }
    let label_ok = |l: &str| {
        !l.is_empty()
            && l.len() <= MAX_LABEL_LEN
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    };
    let labels: Vec<&str> = h.split('.').collect();
    if !labels.iter().all(|l| label_ok(l)) {
        return None;
    }
    // `999.1.1.1` is not an address and must not pass as a name either.
    if labels
        .last()
        .is_some_and(|l| l.bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }
    Some(h)
}

/// A plain login name: a letter or `_`, then letters, digits, `.`, `_`, `-`.
pub fn validate_user(user: &str) -> Result<(), RequestError> {
    let mut b = user.bytes();
    let first = b
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == b'_');
    let rest = b.all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'));
    if first && rest && user.len() <= MAX_USER_LEN {
        Ok(())
    } else {
        Err(RequestError::User(user.to_string()))
    }
}

/// The agent's words as shown on the card: whitespace collapsed, control
/// characters removed, cut at [`MAX_REASON_CHARS`].
pub fn sanitize_reason(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .filter(|c| !c.is_control())
        .collect();
    cleaned
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(MAX_REASON_CHARS)
        .collect()
}

/// Whether `processes` already lets the agent exec `ssh`.
pub fn ssh_spawnable(p: &ProcessesEntitlement) -> bool {
    match p.spawn.mode {
        SpawnMode::Any => true,
        SpawnMode::None => false,
        SpawnMode::Allowlist | SpawnMode::Strict => p.spawn.allowed.iter().any(|b| b == SSH_BINARY),
    }
}

/// One request file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SshRequest {
    pub schema: u32,
    pub host: String,
    pub port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    pub reason: String,
    pub requested_at: DateTime<Utc>,
}

impl SshRequest {
    pub fn new(
        target: &RequestHost,
        user: Option<&str>,
        reason: &str,
        now: DateTime<Utc>,
    ) -> Result<Self, RequestError> {
        if let Some(u) = user {
            validate_user(u)?;
        }
        let reason = sanitize_reason(reason);
        if reason.is_empty() {
            return Err(RequestError::EmptyReason);
        }
        Ok(Self {
            schema: SCHEMA,
            host: target.host().to_string(),
            port: target.port(),
            user: user.map(str::to_string),
            reason,
            requested_at: now,
        })
    }
}

#[cfg(test)]
mod tests;

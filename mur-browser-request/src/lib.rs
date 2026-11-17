//! An agent's request for a website in its browser (browser setup journey,
//! Q1, mur-run/mur#2360).
//!
//! The agent writes it with the `browser_request` tool; murmur, the Hub and
//! the CLI read it outside the sandbox and turn it into a card. A request is
//! data: it names a site, a reason and whether login is needed, never a
//! profile — profile choice belongs to the user on the card. Nothing in it
//! grants, binds, opens or logs in to anything.
//!
//! The agent home is writable by the agent, so it can also write these files
//! with `bash` and skip the tool. [`read_pending`] is therefore the
//! enforcement point, and no path is ever built from a request field except
//! the id, which the reader recomputes.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

mod store;
pub use store::{PendingRequest, Written, read_pending, remove, requests_dir, write};

/// Version of the request file. A reader drops a file whose schema it does not know.
pub const SCHEMA: u32 = 1;
/// Directory under the agent home holding one file per requested origin.
pub const REQUESTS_DIR: &str = "browser-requests";
/// Longest reason kept, in characters.
pub const MAX_REASON_CHARS: usize = 500;
/// Largest request file read, in bytes.
pub const MAX_FILE_BYTES: u64 = 4 * 1024;
/// A request older than this is not listed.
pub const EXPIRY_SECS: i64 = 7 * 24 * 60 * 60;
/// How far past now a `requested_at` may be and still be listed: clock
/// skew, not a way to make a request never expire and always sort first.
pub const MAX_FUTURE_SKEW_SECS: i64 = 5 * 60;
/// Requests listed per agent, newest first.
pub const MAX_PENDING: usize = 8;
/// Longest `site` input accepted, in bytes, before parsing.
pub const MAX_SITE_LEN: usize = 2048;
/// Scheme assumed when the agent names a bare host.
pub const DEFAULT_SCHEME: &str = "https";
/// Characters of a refused input echoed back in the error.
const MAX_ECHO_CHARS: usize = 80;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RequestError {
    #[error(
        "invalid site {0:?}: expected an http(s) URL or a host name, without a user name or password"
    )]
    Site(String),
    #[error("a reason is required")]
    EmptyReason,
}

/// A validated http(s) origin. Only [`Site::parse`] builds one.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Site {
    scheme: String,
    /// Lowercased; an IPv6 address in brackets.
    host: String,
    /// `None` when it is the scheme's default.
    port: Option<u16>,
}

impl Site {
    /// A URL or a bare host (`host`, `host:port`, `host/path`). Path, query
    /// and fragment are dropped (D1).
    pub fn parse(raw: &str) -> Result<Self, RequestError> {
        let raw = raw.trim();
        let err = || RequestError::Site(raw.chars().take(MAX_ECHO_CHARS).collect());
        if raw.is_empty() || raw.len() > MAX_SITE_LEN {
            return Err(err());
        }
        let full = if raw.contains("://") {
            raw.to_string()
        } else {
            format!("{DEFAULT_SCHEME}://{raw}")
        };
        let u = url::Url::parse(&full).map_err(|_| err())?;
        if !matches!(u.scheme(), "http" | "https") {
            return Err(err());
        }
        if !u.username().is_empty() || u.password().is_some() {
            return Err(err());
        }
        let host = match u.host().ok_or_else(err)? {
            url::Host::Domain(d) => d.to_ascii_lowercase(),
            url::Host::Ipv4(a) => a.to_string(),
            url::Host::Ipv6(a) => format!("[{a}]"),
        };
        if host.is_empty() {
            return Err(err());
        }
        Ok(Self {
            scheme: u.scheme().to_string(),
            host,
            port: u.port(),
        })
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn port(&self) -> Option<u16> {
        self.port
    }

    /// `scheme://host[:port]`, the port only when not the default.
    pub fn origin(&self) -> String {
        match self.port {
            Some(p) => format!("{}://{}:{p}", self.scheme, self.host),
            None => format!("{}://{}", self.scheme, self.host),
        }
    }

    /// Lowercase hex SHA-256 of [`Self::origin`]: the request's file name.
    pub fn id(&self) -> String {
        hex::encode(Sha256::digest(self.origin().as_bytes()))
    }
}

/// The agent's words as shown on the card: whitespace collapsed, control
/// characters removed, cut at [`MAX_REASON_CHARS`]. Same rule as
/// `mur_ssh_request::sanitize_reason` (D7).
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

/// One request file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserRequest {
    pub schema: u32,
    /// [`Site::origin`].
    pub site: String,
    pub login: bool,
    pub reason: String,
    pub requested_at: DateTime<Utc>,
}

impl BrowserRequest {
    pub fn new(
        site: &Site,
        login: bool,
        reason: &str,
        now: DateTime<Utc>,
    ) -> Result<Self, RequestError> {
        let reason = sanitize_reason(reason);
        if reason.is_empty() {
            return Err(RequestError::EmptyReason);
        }
        Ok(Self {
            schema: SCHEMA,
            site: site.origin(),
            login,
            reason,
            requested_at: now,
        })
    }
}

#[cfg(test)]
mod tests;

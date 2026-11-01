//! CLI-only lookups: a hostname to its host keys, a key comment to its
//! fingerprint. The runtime never does either (spec "Fingerprints only"); the
//! results are shown to the user and written only after confirmation.

use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use mur_common::agent::{SshKnownHost, fingerprint_of_base64, is_fingerprint};

/// Host-key scanner. Ships with OpenSSH on macOS and every Linux distro.
pub const KEYSCAN_BIN: &str = "ssh-keyscan";
/// Lists the keys held by the user's ssh-agent.
pub const SSH_ADD_BIN: &str = "ssh-add";
/// Per-host connect timeout handed to `ssh-keyscan -T`.
pub const KEYSCAN_TIMEOUT: Duration = Duration::from_secs(10);
/// Port `known_hosts` leaves implicit.
pub const DEFAULT_SSH_PORT: u16 = 22;

/// One host key a scan returned, with its fingerprint.
#[derive(Debug, Clone, PartialEq)]
pub struct ScannedKey {
    pub fingerprint: String,
    pub entry: SshKnownHost,
}

/// Reject a host that could be read as an option, or that is not one token.
pub fn validate_host(host: &str) -> Result<()> {
    if host.is_empty()
        || host.starts_with('-')
        || host
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == ',')
    {
        bail!("invalid host '{host}': expected a single hostname or address");
    }
    Ok(())
}

/// Scan `host:port` once and return every host key it offers.
pub fn scan_host(host: &str, port: u16) -> Result<Vec<ScannedKey>> {
    validate_host(host)?;
    let out = Command::new(KEYSCAN_BIN)
        .arg("-T")
        .arg(KEYSCAN_TIMEOUT.as_secs().to_string())
        .arg("-p")
        .arg(port.to_string())
        .arg(host)
        .output()
        .with_context(|| format!("run {KEYSCAN_BIN}"))?;
    let keys = parse_keyscan(&String::from_utf8_lossy(&out.stdout));
    if keys.is_empty() {
        bail!(
            "{KEYSCAN_BIN} returned no host keys for {host}:{port} — is the host reachable? \
             (or pass --host-key <fingerprint> you obtained out of band)"
        );
    }
    Ok(keys)
}

/// `ssh-keyscan` stdout → keys. Comment lines and anything malformed are
/// skipped; duplicates (same fingerprint) are kept once.
pub fn parse_keyscan(stdout: &str) -> Vec<ScannedKey> {
    let mut keys: Vec<ScannedKey> = Vec::new();
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut f = line.split_whitespace();
        let (Some(host), Some(key_type), Some(b64)) = (f.next(), f.next(), f.next()) else {
            continue;
        };
        let Some(fingerprint) = fingerprint_of_base64(b64) else {
            continue;
        };
        if keys.iter().any(|k| k.fingerprint == fingerprint) {
            continue;
        }
        keys.push(ScannedKey {
            fingerprint,
            entry: SshKnownHost {
                host: host.to_string(),
                key_type: key_type.to_string(),
                key_base64: b64.to_string(),
            },
        });
    }
    keys
}

/// A `--key` value: a fingerprint as given, or a comment resolved to one.
#[derive(Debug, Clone, PartialEq)]
pub enum KeyRef {
    Fingerprint(String),
    /// Looked up in the user's ssh-agent; the comment is never written.
    Resolved {
        fingerprint: String,
        comment: String,
    },
}

impl KeyRef {
    pub fn fingerprint(&self) -> &str {
        match self {
            Self::Fingerprint(fp)
            | Self::Resolved {
                fingerprint: fp, ..
            } => fp,
        }
    }
}

/// Resolve `--key`. A fingerprint is taken as is; anything else must match
/// exactly one key comment in `ssh-add -L`.
pub fn resolve_key(arg: &str) -> Result<KeyRef> {
    if is_fingerprint(arg) {
        return Ok(KeyRef::Fingerprint(arg.to_string()));
    }
    if arg.starts_with(mur_common::agent::FINGERPRINT_PREFIX) {
        bail!("'{arg}' looks like a fingerprint but is malformed (expected SHA256: + 43 chars)");
    }
    let out = Command::new(SSH_ADD_BIN)
        .arg("-L")
        .output()
        .with_context(|| format!("run {SSH_ADD_BIN} -L"))?;
    let listed = String::from_utf8_lossy(&out.stdout);
    match_comment(&listed, arg)
}

/// `ssh-add -L` stdout → the single key whose comment is exactly `comment`.
pub fn match_comment(listed: &str, comment: &str) -> Result<KeyRef> {
    let hits: Vec<String> = listed
        .lines()
        .filter_map(|l| {
            let mut f = l.splitn(3, ' ');
            let (_ty, b64, c) = (f.next()?, f.next()?, f.next().unwrap_or("").trim());
            (c == comment).then(|| fingerprint_of_base64(b64)).flatten()
        })
        .collect();
    match hits.as_slice() {
        [fp] => Ok(KeyRef::Resolved {
            fingerprint: fp.clone(),
            comment: comment.to_string(),
        }),
        [] => bail!(
            "no key in your ssh-agent has the comment '{comment}' \
             (`{SSH_ADD_BIN} -l` lists them; or pass the SHA256: fingerprint)"
        ),
        many => bail!(
            "{} keys in your ssh-agent share the comment '{comment}'; pass the fingerprint instead:\n  {}",
            many.len(),
            many.join("\n  ")
        ),
    }
}

#[cfg(test)]
#[path = "resolve_tests.rs"]
mod tests;

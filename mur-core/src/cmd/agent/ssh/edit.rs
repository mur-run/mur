//! Pure edits to `entitlements.ssh` and the `known_hosts` they imply.
//!
//! No I/O here: `mod.rs` loads the profile, applies one of these, and writes
//! profile, pin and `known_hosts` as one unit.

use anyhow::{Result, bail};
use mur_common::agent::{SshEntitlement, SshHostRule, SshKeyRule, SshKnownHost};

/// One `(key, host)` grant, fully resolved to fingerprints.
#[derive(Debug, Clone, PartialEq)]
pub struct Grant {
    pub key_fp: String,
    pub key_label: Option<String>,
    /// `None`: the key is listed but no host is added (it signs for nothing).
    pub host: Option<HostGrant>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HostGrant {
    pub host_key_fp: String,
    pub label: Option<String>,
    pub known_host: Option<SshKnownHost>,
}

/// What `allow` changed, for the summary line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllowOutcome {
    Added,
    /// The pair and its `known_hosts` name were already allowed; labels
    /// refreshed.
    Updated,
}

/// Add or refresh one grant. A label passed as `None` keeps the stored one.
pub fn allow(ent: &mut SshEntitlement, g: &Grant) -> AllowOutcome {
    let mut outcome = AllowOutcome::Updated;
    let key = match ent.keys.iter().position(|k| k.fingerprint == g.key_fp) {
        Some(i) => &mut ent.keys[i],
        None => {
            outcome = AllowOutcome::Added;
            ent.keys.push(SshKeyRule {
                fingerprint: g.key_fp.clone(),
                label: None,
                allow_hosts: Vec::new(),
            });
            ent.keys.last_mut().expect("just pushed")
        }
    };
    if g.key_label.is_some() {
        key.label = g.key_label.clone();
    }
    let Some(h) = &g.host else {
        return outcome;
    };
    match key
        .allow_hosts
        .iter_mut()
        .find(|r| r.host_key == h.host_key_fp)
    {
        Some(r) => {
            if h.label.is_some() {
                r.label = h.label.clone();
            }
            // Added, never replaced: the same host key under another name
            // (an alias, another port) must not drop the name already
            // confirmed, or connections that worked fail host key checking.
            if let Some(kh) = &h.known_host
                && r.confirm_known_host(kh.clone())
            {
                outcome = AllowOutcome::Added;
            }
        }
        None => {
            outcome = AllowOutcome::Added;
            key.allow_hosts.push(SshHostRule {
                host_key: h.host_key_fp.clone(),
                label: h.label.clone(),
                known_hosts: h.known_host.iter().cloned().collect(),
            });
        }
    }
    outcome
}

/// Which grants a `revoke` takes back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevokeScope {
    /// One `(key, host)` pair. The key stays listed.
    Pair { key_fp: String, host_key_fp: String },
    /// The key and every host under it. Batch: confirmed first.
    Key { key_fp: String },
    /// This host under every key. Batch: confirmed first.
    Host { host_key_fp: String },
}

impl RevokeScope {
    pub fn new(key: Option<String>, host_key: Option<String>) -> Result<Self> {
        Ok(match (key, host_key) {
            (Some(key_fp), Some(host_key_fp)) => Self::Pair {
                key_fp,
                host_key_fp,
            },
            (Some(key_fp), None) => Self::Key { key_fp },
            (None, Some(host_key_fp)) => Self::Host { host_key_fp },
            (None, None) => bail!("revoke needs --key, --host-key, or both"),
        })
    }

    /// Whether more than one pair may go, which needs confirmation.
    pub fn is_batch(&self) -> bool {
        !matches!(self, Self::Pair { .. })
    }
}

/// One `(key, host)` pair that a revoke removed, for the summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Removed {
    pub key_fp: String,
    /// `None`: a key that had no hosts was dropped.
    pub host_key_fp: Option<String>,
}

/// What `scope` would remove, without changing anything (the preview).
pub fn revoke_preview(ent: &SshEntitlement, scope: &RevokeScope) -> Vec<Removed> {
    let mut copy = ent.clone();
    revoke_in_place(&mut copy, scope)
}

/// Remove what `scope` names. A scope that matches nothing is an error, so a
/// typo never looks like a successful revoke.
pub fn revoke(ent: &mut SshEntitlement, scope: &RevokeScope) -> Result<Vec<Removed>> {
    let removed = revoke_in_place(ent, scope);
    if removed.is_empty() {
        bail!("nothing matched; see `mur agent ssh list <agent>` for the stored fingerprints");
    }
    Ok(removed)
}

fn revoke_in_place(ent: &mut SshEntitlement, scope: &RevokeScope) -> Vec<Removed> {
    let mut removed = Vec::new();
    let pair = |k: &SshKeyRule, h: &SshHostRule| Removed {
        key_fp: k.fingerprint.clone(),
        host_key_fp: Some(h.host_key.clone()),
    };
    match scope {
        RevokeScope::Pair {
            key_fp,
            host_key_fp,
        } => {
            if let Some(k) = ent.keys.iter_mut().find(|k| &k.fingerprint == key_fp) {
                let snapshot = k.clone();
                k.allow_hosts.retain(|h| {
                    let hit = &h.host_key == host_key_fp;
                    if hit {
                        removed.push(pair(&snapshot, h));
                    }
                    !hit
                });
            }
        }
        RevokeScope::Key { key_fp } => {
            ent.keys.retain(|k| {
                if &k.fingerprint != key_fp {
                    return true;
                }
                if k.allow_hosts.is_empty() {
                    removed.push(Removed {
                        key_fp: k.fingerprint.clone(),
                        host_key_fp: None,
                    });
                }
                removed.extend(k.allow_hosts.iter().map(|h| pair(k, h)));
                false
            });
        }
        RevokeScope::Host { host_key_fp } => {
            for k in &mut ent.keys {
                let snapshot = k.clone();
                k.allow_hosts.retain(|h| {
                    let hit = &h.host_key == host_key_fp;
                    if hit {
                        removed.push(pair(&snapshot, h));
                    }
                    !hit
                });
            }
        }
    }
    removed
}

/// Header of the generated file, so a reader knows edits do not stick.
pub const KNOWN_HOSTS_HEADER: &str = "# Generated by `mur agent ssh allow|revoke` from confirmed host keys; edits are overwritten.\n";

/// `<agent home>/ssh/known_hosts` for `ent`: one line per confirmed
/// `(name, host key)`, deduplicated across keys. Empty (no header) when no rule carries an entry, so revoking
/// the last host leaves nothing for ssh to trust.
pub fn render_known_hosts(ent: &SshEntitlement) -> String {
    let mut lines: Vec<String> = Vec::new();
    for kh in ent
        .keys
        .iter()
        .flat_map(|k| &k.allow_hosts)
        .flat_map(|h| &h.known_hosts)
    {
        let line = format!("{} {} {}", kh.host, kh.key_type, kh.key_base64);
        if !lines.contains(&line) {
            lines.push(line);
        }
    }
    if lines.is_empty() {
        return String::new();
    }
    let mut out = KNOWN_HOSTS_HEADER.to_string();
    for l in lines {
        out.push_str(&l);
        out.push('\n');
    }
    out
}

#[cfg(test)]
#[path = "edit_tests.rs"]
mod tests;

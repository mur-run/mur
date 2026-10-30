//! Key → host-key policy (plan Task 2.2). Built once at start from the
//! `entitlements.ssh` block the supervisor loaded after the pin check; never
//! re-read while running (spec §"Location and tamper resistance").

use std::collections::HashMap;

use mur_common::agent::SshEntitlement;

use super::refusal::{Refusal, RefusalCode};

/// Whether the entitlements the policy came from are covered by a pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinTrust {
    /// The pin existed and matched.
    Pinned,
    /// No pin was applied (agent outside `<mur_home>/agents`) or one was only
    /// just written on first use. The file was not verified against anything.
    Unpinned,
}

/// One listed key: its display label and the host-key fingerprints it may
/// sign for.
#[derive(Debug, Clone)]
pub struct KeyEntry {
    label: Option<String>,
    hosts: Vec<String>,
}

/// What the proxy enforces. Default deny throughout.
#[derive(Debug, Clone)]
pub enum Policy {
    /// Normal operation: only listed keys, only listed host keys per key.
    Enforce(HashMap<String, KeyEntry>),
    /// Every request is refused with this code (`policy_invalid`,
    /// `policy_unpinned`). Listing shows no keys.
    RefuseAll(RefusalCode),
}

/// A sign request that passed policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Allowed {
    pub key_fp: String,
    pub key_label: Option<String>,
    pub host_key_fp: String,
}

impl Policy {
    /// `unpinned_is_untrusted` is the platform rule: on Linux the agent can
    /// rewrite its own `profile.yaml`, so only a pinned copy is trusted. On
    /// macOS the kernel denies that write (`SELF_PROTECTED_AGENT_FILES`).
    pub fn build(ent: &SshEntitlement, trust: PinTrust, unpinned_is_untrusted: bool) -> Self {
        if trust == PinTrust::Unpinned && unpinned_is_untrusted {
            return Self::RefuseAll(RefusalCode::PolicyUnpinned);
        }
        let mut keys: HashMap<String, KeyEntry> = HashMap::new();
        for k in &ent.keys {
            if !is_fingerprint(&k.fingerprint)
                || k.allow_hosts.iter().any(|h| !is_fingerprint(&h.host_key))
            {
                return Self::RefuseAll(RefusalCode::PolicyInvalid);
            }
            let e = keys.entry(k.fingerprint.clone()).or_insert(KeyEntry {
                label: k.label.clone(),
                hosts: Vec::new(),
            });
            e.hosts
                .extend(k.allow_hosts.iter().map(|h| h.host_key.clone()));
        }
        Self::Enforce(keys)
    }

    /// The platform rule for this build: only Linux distrusts an unpinned
    /// policy (spec Load-time vs sign-time, "Agent has no entitlement pin").
    pub const UNPINNED_IS_UNTRUSTED: bool = cfg!(target_os = "linux");

    /// Is `key_fp` shown to the client by `REQUEST_IDENTITIES`?
    pub fn lists(&self, key_fp: &str) -> bool {
        matches!(self, Self::Enforce(keys) if keys.contains_key(key_fp))
    }

    /// The display label of a listed key, for refusals raised before the
    /// policy lookup.
    pub fn label(&self, key_fp: &str) -> Option<String> {
        match self {
            Self::Enforce(keys) => keys.get(key_fp).and_then(|e| e.label.clone()),
            Self::RefuseAll(_) => None,
        }
    }

    /// Listed key fingerprints, for the start-time "missing upstream" warning.
    pub fn key_fps(&self) -> Vec<&str> {
        match self {
            Self::Enforce(keys) => keys.keys().map(String::as_str).collect(),
            Self::RefuseAll(_) => Vec::new(),
        }
    }

    /// The policy check for one signature. `host_key_fp` is `None` when the
    /// connection has no verified `session-bind`.
    pub fn check_sign(&self, key_fp: &str, host_key_fp: Option<&str>) -> Result<Allowed, Refusal> {
        let keys = match self {
            Self::Enforce(keys) => keys,
            Self::RefuseAll(code) => return Err(Refusal::new(*code).key(key_fp, None)),
        };
        let Some(entry) = keys.get(key_fp) else {
            return Err(Refusal::new(RefusalCode::KeyNotAllowed).key(key_fp, None));
        };
        let refusal = |code| Refusal::new(code).key(key_fp, entry.label.clone());
        let Some(host) = host_key_fp else {
            return Err(refusal(RefusalCode::NoSessionBind));
        };
        if !entry.hosts.iter().any(|h| h == host) {
            return Err(refusal(RefusalCode::HostNotAllowed).host(host));
        }
        Ok(Allowed {
            key_fp: key_fp.to_string(),
            key_label: entry.label.clone(),
            host_key_fp: host.to_string(),
        })
    }
}

/// `SHA256:` followed by the 43-character unpadded base64 of a SHA-256.
fn is_fingerprint(s: &str) -> bool {
    use base64::Engine as _;
    s.strip_prefix("SHA256:").is_some_and(|b| {
        b.len() == 43
            && base64::engine::general_purpose::STANDARD_NO_PAD
                .decode(b)
                .is_ok_and(|d| d.len() == 32)
    })
}

//! Revocations list consumer — spec §7.4.1.
//!
//! v1 ships the consumer-side types, monotonicity check, expiry check, and
//! local cache. DSSE signature verification against the mur root key is
//! deferred to V2 (no root key material exists in v1). The format is
//! forward-compatible: a v1 consumer can parse and cache a v2-signed list,
//! it just won't enforce the signature yet.

use crate::muragent::MuragentError;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// A single revoked item — either a specific package or an entire author.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum RevokedEntry {
    Package {
        /// SHA-256 hash of manifest.signed.json, prefixed `sha256:<hex>`.
        manifest_hash: String,
        reason: String,
        revoked_at: DateTime<Utc>,
    },
    Author {
        /// Ed25519 public key, prefixed `ed25519:<base64>`.
        pubkey: String,
        reason: String,
        revoked_at: DateTime<Utc>,
    },
}

/// The signed revocations list fetched from `https://mur.run/revocations.json`.
///
/// Modeled on TUF's `timestamp.json` role. Outer DSSE envelope is stripped
/// during fetch; this struct represents the payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevocationsList {
    pub version: u32,
    pub this_update: DateTime<Utc>,
    pub next_update: DateTime<Utc>,
    /// After this timestamp Hub refuses to operate until the list is refreshed.
    pub expires_at: DateTime<Utc>,
    /// Monotonically increasing counter. Hub rejects any list whose
    /// `crl_number` is ≤ the last accepted value (rollback / clock-rollback
    /// defence per spec §7.4.1).
    pub crl_number: u64,
    pub revoked: Vec<RevokedEntry>,
}

impl RevocationsList {
    /// Parse from raw JSON bytes and validate monotonicity + expiry.
    ///
    /// `known_crl` is the `crl_number` from the last accepted list (or `None`
    /// on first fetch — in which case any validly-signed list is accepted).
    pub fn parse_and_validate(bytes: &[u8], known_crl: Option<u64>) -> Result<Self, MuragentError> {
        let list: Self = serde_json::from_slice(bytes)
            .map_err(|e| MuragentError::Other(format!("revocations parse: {e}")))?;

        if list.version != 1 {
            return Err(MuragentError::Other(format!(
                "revocations: unsupported version {}",
                list.version
            )));
        }

        // Monotonicity: reject rollback even under clock-rollback attack.
        if let Some(n) = known_crl
            && list.crl_number <= n
        {
            return Err(MuragentError::Other(format!(
                "revocations: crl_number {} is not greater than known {}",
                list.crl_number, n
            )));
        }

        if list.expires_at < Utc::now() {
            return Err(MuragentError::Other(
                "revocations: list is already expired".into(),
            ));
        }

        // TODO(M-export-V2): verify DSSE envelope signature against mur root key.

        Ok(list)
    }

    /// Returns `true` if the list's `expires_at` is in the past.
    pub fn is_expired(&self) -> bool {
        Utc::now() > self.expires_at
    }

    /// Returns `true` if the given manifest hash appears in the revoked list.
    ///
    /// `manifest_hash` should be the `sha256:<hex>` string from the manifest.
    pub fn is_package_revoked(&self, manifest_hash: &str) -> bool {
        self.revoked.iter().any(
            |e| matches!(e, RevokedEntry::Package { manifest_hash: h, .. } if h == manifest_hash),
        )
    }

    /// Returns `true` if the given author pubkey is revoked.
    ///
    /// `pubkey` should be the `ed25519:<base64>` string from the manifest.
    pub fn is_author_revoked(&self, pubkey: &str) -> bool {
        self.revoked
            .iter()
            .any(|e| matches!(e, RevokedEntry::Author { pubkey: k, .. } if k == pubkey))
    }

    /// Load the locally-cached revocations list from `<mur_home>/trust/revocations.json`.
    ///
    /// Returns `None` if no cache file exists or if the file cannot be parsed.
    /// Callers that only need the cached `crl_number` should unwrap and read
    /// `list.crl_number`.
    pub fn load_cached(mur_home: &Path) -> Option<Self> {
        let path = mur_home.join("trust").join("revocations.json");
        let bytes = std::fs::read(&path).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Atomically write the revocations list to the local cache.
    ///
    /// Uses temp-file + rename for crash-safety (same pattern as TrustStore).
    pub fn save_cached(&self, mur_home: &Path) -> Result<(), MuragentError> {
        let dir = mur_home.join("trust");
        std::fs::create_dir_all(&dir).map_err(MuragentError::Io)?;
        let path = dir.join("revocations.json");
        let tmp = path.with_extension("json.tmp");
        let json = serde_json::to_vec_pretty(self)
            .map_err(|e| MuragentError::Other(format!("revocations serialize: {e}")))?;
        std::fs::write(&tmp, &json).map_err(MuragentError::Io)?;
        std::fs::rename(&tmp, &path).map_err(MuragentError::Io)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trust::test_env_lock::MUR_HOME_LOCK;
    use chrono::Duration;

    fn make_list(crl: u64, expires_offset_secs: i64) -> RevocationsList {
        let now = Utc::now();
        RevocationsList {
            version: 1,
            this_update: now,
            next_update: now + Duration::hours(24),
            expires_at: now + Duration::seconds(expires_offset_secs),
            crl_number: crl,
            revoked: vec![
                RevokedEntry::Package {
                    manifest_hash: "sha256:deadbeef".into(),
                    reason: "test".into(),
                    revoked_at: now,
                },
                RevokedEntry::Author {
                    pubkey: "ed25519:abc123".into(),
                    reason: "test".into(),
                    revoked_at: now,
                },
            ],
        }
    }

    fn serialize(list: &RevocationsList) -> Vec<u8> {
        serde_json::to_vec(list).unwrap()
    }

    #[test]
    fn parse_valid_list() {
        let list = make_list(1, 3600);
        let bytes = serialize(&list);
        let parsed = RevocationsList::parse_and_validate(&bytes, None).unwrap();
        assert_eq!(parsed.crl_number, 1);
        assert_eq!(parsed.revoked.len(), 2);
    }

    #[test]
    fn rejects_crl_rollback() {
        let list = make_list(5, 3600);
        let bytes = serialize(&list);
        // known_crl >= new crl_number → rejected
        let err = RevocationsList::parse_and_validate(&bytes, Some(5)).unwrap_err();
        assert!(err.to_string().contains("crl_number"));
        let err2 = RevocationsList::parse_and_validate(&bytes, Some(6)).unwrap_err();
        assert!(err2.to_string().contains("crl_number"));
    }

    #[test]
    fn accepts_first_fetch() {
        let list = make_list(1, 3600);
        let bytes = serialize(&list);
        // None known_crl → always accepted
        RevocationsList::parse_and_validate(&bytes, None).unwrap();
    }

    #[test]
    fn is_expired_past() {
        let list = make_list(1, -60); // expired 1 minute ago
        assert!(list.is_expired());
    }

    #[test]
    fn is_expired_future() {
        let list = make_list(1, 3600);
        assert!(!list.is_expired());
    }

    #[test]
    fn rejects_already_expired_on_parse() {
        let list = make_list(1, -60);
        let bytes = serialize(&list);
        let err = RevocationsList::parse_and_validate(&bytes, None).unwrap_err();
        assert!(err.to_string().contains("expired"));
    }

    #[test]
    fn package_revocation_lookup() {
        let list = make_list(1, 3600);
        assert!(list.is_package_revoked("sha256:deadbeef"));
        assert!(!list.is_package_revoked("sha256:00000000"));
    }

    #[test]
    fn author_revocation_lookup() {
        let list = make_list(1, 3600);
        assert!(list.is_author_revoked("ed25519:abc123"));
        assert!(!list.is_author_revoked("ed25519:nothere"));
    }

    #[test]
    fn cache_roundtrip() {
        let _guard = MUR_HOME_LOCK.lock().unwrap();
        let tmp = tempfile::TempDir::new().unwrap();
        let prev_home = std::env::var_os("MUR_HOME");
        unsafe { std::env::set_var("MUR_HOME", tmp.path()) };

        let list = make_list(42, 3600);
        list.save_cached(tmp.path()).unwrap();
        let loaded = RevocationsList::load_cached(tmp.path()).unwrap();
        assert_eq!(loaded.crl_number, 42);
        assert_eq!(loaded.revoked.len(), 2);

        unsafe {
            if let Some(p) = prev_home {
                std::env::set_var("MUR_HOME", p);
            } else {
                std::env::remove_var("MUR_HOME");
            }
        }
    }
}

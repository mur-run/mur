//! Publisher trust keyring — SSH-style pinned trust roots for skill signature verification.
//!
//! `PublisherKeyring` lives at `~/.mur/trust/publishers.yaml` and is seeded on first
//! use with the MUR official publisher fingerprint.  Downstream units consult
//! `classify()` to decide whether a DSSE signer is Trusted / Revoked / Unknown.

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Pinned MUR official publisher key fingerprint (trust anchor).
/// Derived via: SHA-256(raw 32-byte pubkey), first 8 hex chars, prefixed "ed25519-".
/// No other values are hardcoded — all trust decisions flow through the keyring.
pub const MUR_OFFICIAL_PUBLISHER_KEY_FP: &str = "ed25519-861d2acb";

/// Pinned MUR official **license** key fingerprint (trust anchor for
/// `OfficialLicense` signatures — a SEPARATE key from the bundle publisher key
/// above, so the online license-signing key on the server can never sign
/// official bundles). Same derivation: SHA-256(raw 32-byte pubkey), first 8 hex.
pub const MUR_OFFICIAL_LICENSE_KEY_FP: &str = "ed25519-71594984";

/// Trust classification for a signer fingerprint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PublisherTrust {
    Trusted,
    Revoked,
    Unknown,
}

/// A single trusted publisher entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustedPublisher {
    pub name: String,
    pub key_fp: String,
    #[serde(default)]
    pub comment: String,
}

/// Serialisable publisher keyring stored at `~/.mur/trust/publishers.yaml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublisherKeyring {
    pub schema_version: u32,
    #[serde(default)]
    pub publishers: Vec<TrustedPublisher>,
    #[serde(default)]
    pub revoked: Vec<String>,
}

impl PublisherKeyring {
    /// Canonical on-disk path for the keyring.
    pub fn path(mur_home: &Path) -> PathBuf {
        mur_home.join("trust").join("publishers.yaml")
    }

    /// Classify a key fingerprint.
    ///
    /// **Revoked always takes precedence over Trusted (fail-closed):** a key that
    /// appears in both `publishers` and `revoked` is classified `Revoked`.
    pub fn classify(&self, key_fp: &str) -> PublisherTrust {
        if self.revoked.iter().any(|r| r == key_fp) {
            return PublisherTrust::Revoked;
        }
        if self.publishers.iter().any(|p| p.key_fp == key_fp) {
            return PublisherTrust::Trusted;
        }
        PublisherTrust::Unknown
    }

    /// Build the seeded keyring containing only the pinned official publisher key.
    fn seed() -> Self {
        PublisherKeyring {
            schema_version: 1,
            publishers: vec![TrustedPublisher {
                name: "mur".to_string(),
                key_fp: MUR_OFFICIAL_PUBLISHER_KEY_FP.to_string(),
                comment: "MUR official publisher (pinned trust root)".to_string(),
            }],
            revoked: Vec::new(),
        }
    }

    /// Load the keyring from disk; if absent, seed with the pinned official key and persist.
    pub fn load_or_seed(mur_home: &Path) -> anyhow::Result<Self> {
        let p = Self::path(mur_home);
        if p.exists() {
            let text =
                std::fs::read_to_string(&p).with_context(|| format!("read {}", p.display()))?;
            serde_yaml_ng::from_str(&text)
                .map_err(|e| anyhow::anyhow!("parse publisher keyring: {e}"))
        } else {
            let kr = Self::seed();
            kr.save(mur_home)?;
            Ok(kr)
        }
    }

    /// Persist the keyring to disk using temp-file + rename for atomicity.
    pub fn save(&self, mur_home: &Path) -> anyhow::Result<()> {
        let p = Self::path(mur_home);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create dir {}", parent.display()))?;
        }
        let yaml = serde_yaml_ng::to_string(self)
            .map_err(|e| anyhow::anyhow!("serialize publisher keyring: {e}"))?;
        // Atomic write: write to a sibling .tmp then rename.
        let tmp_path = p.with_extension("yaml.tmp");
        std::fs::write(&tmp_path, yaml.as_bytes())
            .with_context(|| format!("write {}", tmp_path.display()))?;
        std::fs::rename(&tmp_path, &p)
            .with_context(|| format!("rename {} -> {}", tmp_path.display(), p.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kr() -> PublisherKeyring {
        PublisherKeyring {
            schema_version: 1,
            publishers: vec![TrustedPublisher {
                name: "mur".into(),
                key_fp: "ed25519-aabbccdd".into(),
                comment: "official".into(),
            }],
            revoked: vec!["ed25519-deadbeef".into()],
        }
    }

    #[test]
    fn classify_trusted_revoked_unknown() {
        let k = kr();
        assert_eq!(k.classify("ed25519-aabbccdd"), PublisherTrust::Trusted);
        assert_eq!(k.classify("ed25519-deadbeef"), PublisherTrust::Revoked);
        assert_eq!(k.classify("ed25519-00000000"), PublisherTrust::Unknown);
    }

    #[test]
    fn revoked_beats_trusted() {
        // A key both listed AND revoked must classify Revoked (fail-closed).
        let k = PublisherKeyring {
            schema_version: 1,
            publishers: vec![TrustedPublisher {
                name: "mur".into(),
                key_fp: "ed25519-aabbccdd".into(),
                comment: String::new(),
            }],
            revoked: vec!["ed25519-aabbccdd".into()],
        };
        assert_eq!(k.classify("ed25519-aabbccdd"), PublisherTrust::Revoked);
    }

    #[test]
    fn load_or_seed_creates_file_with_official_key() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let kr = PublisherKeyring::load_or_seed(tmp.path()).expect("load_or_seed");
        assert_eq!(
            kr.classify(MUR_OFFICIAL_PUBLISHER_KEY_FP),
            PublisherTrust::Trusted
        );
        // File must now exist on disk.
        assert!(PublisherKeyring::path(tmp.path()).exists());
        // Round-trip: reload → same result.
        let kr2 = PublisherKeyring::load_or_seed(tmp.path()).expect("reload");
        assert_eq!(
            kr2.classify(MUR_OFFICIAL_PUBLISHER_KEY_FP),
            PublisherTrust::Trusted
        );
    }
}

//! Signer trust classification — layers the publisher keyring onto a DSSE
//! `SignatureStatus` to produce a three-state trust decision.
//!
//! Used by `skill_registry_add::resolve_consent_in` to fold publisher trust
//! into the fail-closed install gate.
//!
//! Also provides `check_drift` / `DriftDecision` for rug-pull and rollback
//! detection at reinstall time.

use mur_common::skill::publisher_trust::{PublisherKeyring, PublisherTrust};

use crate::cmd::agent::skill_verify::SignatureStatus;

/// Trust classification of a skill's signer, relative to the local publisher keyring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignerTrust {
    /// Signer is in the keyring's `publishers` list and not revoked.
    Trusted,
    /// Signature is valid but signer is not in the keyring (Unknown).
    Untrusted,
    /// Signer key appears in the keyring's `revoked` list. Hard block.
    Revoked,
    /// No publisher signature present.
    Unsigned,
    /// Publisher signature present but failed to verify (tampering / malformed).
    Invalid,
}

impl SignerTrust {
    pub fn as_str(&self) -> &'static str {
        match self {
            SignerTrust::Trusted => "trusted",
            SignerTrust::Untrusted => "untrusted",
            SignerTrust::Revoked => "revoked",
            SignerTrust::Unsigned => "unsigned",
            SignerTrust::Invalid => "invalid",
        }
    }
}

/// Classify a DSSE `SignatureStatus` against the local publisher keyring.
///
/// - `Verified { key_fp }` → look up the fingerprint in the keyring:
///   - `Trusted` if the key is in `publishers` (and not revoked).
///   - `Revoked` if the key appears in `revoked` (fail-closed; revoked beats trusted).
///   - `Untrusted` if the key is not in the keyring.
/// - `Unsigned` → `Unsigned` (no publisher signature present).
/// - `Invalid` → `Invalid` (signature present but cryptographic check failed).
pub fn classify_signer(sig: &SignatureStatus, keyring: &PublisherKeyring) -> SignerTrust {
    match sig {
        SignatureStatus::Unsigned => SignerTrust::Unsigned,
        SignatureStatus::Invalid => SignerTrust::Invalid,
        SignatureStatus::Verified { key_fp, .. } => match keyring.classify(key_fp) {
            PublisherTrust::Trusted => SignerTrust::Trusted,
            PublisherTrust::Revoked => SignerTrust::Revoked,
            PublisherTrust::Unknown => SignerTrust::Untrusted,
        },
    }
}

// ─── Drift detection ──────────────────────────────────────────────────────

/// Result of comparing a prior install record against a new install attempt.
#[derive(Debug, PartialEq)]
pub enum DriftDecision {
    /// No prior record, or everything matches — proceed normally.
    None,
    /// Content hash or publisher key changed between installs.
    Changed { what: String },
    /// Offered version is an older semver than the installed one (downgrade).
    Rollback { installed: String, offered: String },
}

/// Compare a prior install record against the incoming version, hash, and signer.
///
/// `prior` is `(content_sha256, signer_key_fp, installed_version)`.
///
/// Priority (fail-closed, first match wins):
/// 1. **Rollback**: offered semver < installed semver → `Rollback`.
/// 2. **Content drift**: hashes differ (both non-empty) → `Changed { what: "content" }`.
/// 3. **Publisher drift**: signer key changed (prior had a signer) → `Changed { what: "publisher" }`.
/// 4. Otherwise → `None`.
pub fn check_drift(
    prior: Option<(&str, Option<&str>, &str)>,
    new_hash: &str,
    new_signer: Option<&str>,
    new_ver: &str,
) -> DriftDecision {
    let (old_hash, old_signer, old_ver) = match prior {
        None => return DriftDecision::None,
        Some(t) => t,
    };
    // 1. Rollback check — only if both parse as valid semver.
    if let (Ok(n), Ok(o)) = (
        semver::Version::parse(new_ver),
        semver::Version::parse(old_ver),
    ) && n < o
    {
        return DriftDecision::Rollback {
            installed: old_ver.to_string(),
            offered: new_ver.to_string(),
        };
    }
    // 2. Content drift — both hashes must be non-empty to compare.
    if !new_hash.is_empty() && !old_hash.is_empty() && new_hash != old_hash {
        return DriftDecision::Changed {
            what: "content".to_string(),
        };
    }
    // 3. Publisher drift — only fires if the prior install had a known signer.
    if old_signer.is_some() && new_signer != old_signer {
        return DriftDecision::Changed {
            what: "publisher".to_string(),
        };
    }
    DriftDecision::None
}

#[cfg(test)]
mod drift_tests {
    use super::*;

    #[test]
    fn no_prior_is_no_drift() {
        assert!(matches!(
            check_drift(None, "h1", Some("k1"), "1.0.0"),
            DriftDecision::None
        ));
    }

    #[test]
    fn same_everything_is_no_drift() {
        // version advances — same hash + signer is fine
        assert!(matches!(
            check_drift(Some(("h1", Some("k1"), "1.0.0")), "h1", Some("k1"), "1.1.0"),
            DriftDecision::None
        ));
    }

    #[test]
    fn changed_hash_is_drift() {
        assert!(matches!(
            check_drift(Some(("h1", Some("k1"), "1.0.0")), "h2", Some("k1"), "1.1.0"),
            DriftDecision::Changed { what } if what == "content"
        ));
    }

    #[test]
    fn changed_signer_is_drift() {
        assert!(matches!(
            check_drift(Some(("h1", Some("k1"), "1.0.0")), "h1", Some("k2"), "1.1.0"),
            DriftDecision::Changed { what } if what == "publisher"
        ));
    }

    #[test]
    fn version_rollback_is_detected() {
        assert!(matches!(
            check_drift(Some(("h1", Some("k1"), "1.1.0")), "h1", Some("k1"), "1.0.0"),
            DriftDecision::Rollback { .. }
        ));
    }

    #[test]
    fn rollback_beats_content_change() {
        // Even if hash differs, rollback is reported first.
        assert!(matches!(
            check_drift(Some(("h1", Some("k1"), "1.1.0")), "h2", Some("k2"), "1.0.0"),
            DriftDecision::Rollback { .. }
        ));
    }

    #[test]
    fn no_signer_change_when_prior_had_none() {
        // Prior had no signer; new install is also unsigned — no publisher drift.
        assert!(matches!(
            check_drift(Some(("h1", None, "1.0.0")), "h1", None, "1.1.0"),
            DriftDecision::None
        ));
    }

    #[test]
    fn empty_hashes_skip_content_check() {
        // Both hashes empty → can't compare content — no drift.
        assert!(matches!(
            check_drift(Some(("", Some("k1"), "1.0.0")), "", Some("k1"), "1.1.0"),
            DriftDecision::None
        ));
    }

    #[test]
    fn non_semver_versions_skip_rollback_check() {
        // Non-semver versions cannot be compared, so no rollback detected.
        assert!(matches!(
            check_drift(Some(("h1", Some("k1"), "main")), "h1", Some("k1"), "dev"),
            DriftDecision::None
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::skill::publisher_trust::{PublisherKeyring, TrustedPublisher};

    use crate::cmd::agent::skill_verify::SignatureStatus;

    fn keyring(trusted: &str, revoked: &str) -> PublisherKeyring {
        PublisherKeyring {
            schema_version: 1,
            publishers: vec![TrustedPublisher {
                name: "mur".into(),
                key_fp: trusted.into(),
                comment: String::new(),
            }],
            revoked: vec![revoked.into()],
        }
    }

    fn verified(fp: &str) -> SignatureStatus {
        SignatureStatus::Verified {
            publisher: "mur".into(),
            key_fp: fp.into(),
        }
    }

    #[test]
    fn verified_known_key_is_trusted() {
        let k = keyring("ed25519-trusted0", "ed25519-revoked0");
        assert_eq!(
            classify_signer(&verified("ed25519-trusted0"), &k),
            SignerTrust::Trusted
        );
    }

    #[test]
    fn verified_revoked_key_is_revoked() {
        let k = keyring("ed25519-trusted0", "ed25519-revoked0");
        assert_eq!(
            classify_signer(&verified("ed25519-revoked0"), &k),
            SignerTrust::Revoked
        );
    }

    #[test]
    fn verified_unknown_key_is_untrusted() {
        let k = keyring("ed25519-trusted0", "ed25519-revoked0");
        assert_eq!(
            classify_signer(&verified("ed25519-unknown0"), &k),
            SignerTrust::Untrusted
        );
    }

    #[test]
    fn unsigned_is_unsigned() {
        let k = keyring("ed25519-trusted0", "ed25519-revoked0");
        assert_eq!(
            classify_signer(&SignatureStatus::Unsigned, &k),
            SignerTrust::Unsigned
        );
    }

    #[test]
    fn invalid_is_invalid() {
        let k = keyring("ed25519-trusted0", "ed25519-revoked0");
        assert_eq!(
            classify_signer(&SignatureStatus::Invalid, &k),
            SignerTrust::Invalid
        );
    }
}

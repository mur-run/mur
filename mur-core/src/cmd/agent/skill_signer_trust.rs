//! Signer trust classification — layers the publisher keyring onto a DSSE
//! `SignatureStatus` to produce a three-state trust decision.
//!
//! Used by `skill_registry_add::resolve_consent_in` to fold publisher trust
//! into the fail-closed install gate.

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

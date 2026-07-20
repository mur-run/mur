//! `.fleet` bundle manifest types + signing primitives (pure; no I/O).
//!
//! The MANIFEST is the only signed object: it pins every bundled file by
//! SHA-256, so the archive container need not be byte-deterministic — verifying
//! each file's hash against the manifest plus the manifest signature suffices.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Bundle wire-format version. Bump on any breaking manifest change.
pub const FLEET_BUNDLE_FORMAT: u32 = 1;

/// One file in a bundle, pinned by content hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleEntry {
    /// Bundle-relative path, e.g. `fleet.yaml`, `skills/triage/skill.yaml`.
    pub path: String,
    /// Lowercase hex SHA-256 of the file's bytes.
    pub sha256: String,
}

/// The signed manifest at `bundle.yaml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleManifest {
    pub format_version: u32,
    pub fleet_name: String,
    pub created_at: String,
    /// Exporter's concierge identity public key (multibase).
    pub signer_pubkey: String,
    /// Short, human-checkable fingerprint of `signer_pubkey`.
    pub signer_fingerprint: String,
    pub includes_members: bool,
    /// Declared member names (always listed, even when not bundled).
    pub members: Vec<String>,
    /// Every bundled file pinned by hash.
    pub entries: Vec<BundleEntry>,
    /// Multibase Ed25519 signature over `manifest_sign_input` (this manifest
    /// with `sig=None`). `None` only while building, before signing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sig: Option<String>,
    /// `Some("official")` for bundles published from the official catalog.
    /// Inside the signed payload: stripping it invalidates the signature.
    /// Absent on user-created bundles (and skipped in serialization, so
    /// pre-existing signed bundles keep verifying byte-for-byte).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distribution: Option<String>,
}

/// Lowercase hex SHA-256 of `bytes`.
pub fn content_hash(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

/// Canonical signing input: the manifest serialized with `sig` cleared. Struct
/// field order is fixed, so this is deterministic without a custom canonicalizer.
pub fn manifest_sign_input(m: &BundleManifest) -> Vec<u8> {
    let mut unsigned = m.clone();
    unsigned.sig = None;
    serde_json::to_vec(&unsigned).expect("manifest serializes")
}

/// Short fingerprint of a multibase pubkey: first 8 hex chars of its SHA-256,
/// hyphen-grouped (e.g. `ab12-cd34`) for human out-of-band comparison.
pub fn signer_fingerprint(signer_pubkey_multibase: &str) -> String {
    let h = content_hash(signer_pubkey_multibase.as_bytes());
    format!("{}-{}", &h[0..4], &h[4..8])
}

/// Verify the manifest signature against `pubkey`. Fail-closed: no `sig` → false.
pub fn verify_manifest_sig(m: &BundleManifest, pubkey: &[u8; 32]) -> bool {
    let Some(sig) = m.sig.as_deref() else {
        return false;
    };
    crate::identity::verify_bytes(pubkey, &manifest_sign_input(m), sig)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::AgentIdentity;

    fn sample_manifest() -> BundleManifest {
        BundleManifest {
            format_version: FLEET_BUNDLE_FORMAT,
            fleet_name: "devteam".into(),
            created_at: "2026-06-20T00:00:00Z".into(),
            signer_pubkey: String::new(),
            signer_fingerprint: String::new(),
            includes_members: false,
            members: vec!["pm".into(), "qa".into()],
            entries: vec![BundleEntry {
                path: "fleet.yaml".into(),
                sha256: content_hash(b"name: devteam\n"),
            }],
            sig: None,
            distribution: None,
        }
    }

    #[test]
    fn content_hash_is_deterministic_hex() {
        assert_eq!(content_hash(b"abc"), content_hash(b"abc"));
        assert_ne!(content_hash(b"abc"), content_hash(b"abd"));
        assert_eq!(content_hash(b"").len(), 64); // sha256 hex
    }

    #[test]
    fn manifest_roundtrips_yaml() {
        let m = sample_manifest();
        let y = serde_yaml::to_string(&m).unwrap();
        let back: BundleManifest = serde_yaml::from_str(&y).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn sign_then_verify_roundtrip_and_tamper_fails() {
        let id = AgentIdentity::generate();
        let mut m = sample_manifest();
        m.signer_pubkey = id.public_key_multibase();
        m.signer_fingerprint = signer_fingerprint(&m.signer_pubkey);
        // sign canonical input (sig must be None during signing)
        let input = manifest_sign_input(&m);
        m.sig = Some(multibase::encode(
            multibase::Base::Base58Btc,
            id.sign_bytes(&input),
        ));
        let pubkey = id.verifying_key_bytes();
        assert!(verify_manifest_sig(&m, &pubkey));

        // tamper an entry hash → verify fails
        let mut tampered = m.clone();
        tampered.entries[0].sha256 = content_hash(b"evil");
        assert!(!verify_manifest_sig(&tampered, &pubkey));

        // flip the signature → verify fails (fail-closed)
        let mut badsig = m.clone();
        badsig.sig = Some(multibase::encode(multibase::Base::Base58Btc, [0u8; 64]));
        assert!(!verify_manifest_sig(&badsig, &pubkey));

        // missing signature → false
        let mut unsigned = m.clone();
        unsigned.sig = None;
        assert!(!verify_manifest_sig(&unsigned, &pubkey));
    }

    #[test]
    fn fingerprint_is_short_and_stable() {
        let fp = signer_fingerprint("zSomePubKey");
        assert_eq!(fp, signer_fingerprint("zSomePubKey"));
        assert!(fp.len() <= 12 && !fp.is_empty());
    }

    #[test]
    fn manifest_without_distribution_keeps_legacy_sign_input() {
        // A manifest with distribution=None must produce byte-identical sign
        // input to one that predates the field (skip_serializing_if).
        let m = BundleManifest {
            format_version: FLEET_BUNDLE_FORMAT,
            fleet_name: "f".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            signer_pubkey: "z".into(),
            signer_fingerprint: "aa11-bb22".into(),
            includes_members: false,
            members: vec![],
            entries: vec![],
            sig: None,
            distribution: None,
        };
        let json = String::from_utf8(manifest_sign_input(&m)).unwrap();
        assert!(!json.contains("distribution"));
    }

    #[test]
    fn stripping_distribution_breaks_signature() {
        use ed25519_dalek::{Signer, SigningKey};
        let key = SigningKey::from_bytes(&[9u8; 32]);
        let mut m = BundleManifest {
            format_version: FLEET_BUNDLE_FORMAT,
            fleet_name: "f".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            signer_pubkey: multibase::encode(
                multibase::Base::Base58Btc,
                key.verifying_key().as_bytes(),
            ),
            signer_fingerprint: "aa11-bb22".into(),
            includes_members: false,
            members: vec![],
            entries: vec![],
            sig: None,
            distribution: Some(crate::official::DISTRIBUTION_OFFICIAL.into()),
        };
        let sig = key.sign(&manifest_sign_input(&m));
        m.sig = Some(multibase::encode(
            multibase::Base::Base58Btc,
            sig.to_bytes(),
        ));
        let pk = key.verifying_key().to_bytes();
        assert!(verify_manifest_sig(&m, &pk));
        // Strip the marker → signature must fail.
        m.distribution = None;
        assert!(!verify_manifest_sig(&m, &pk));
    }
}

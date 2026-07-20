//! Official catalog license — pure types + signing/verification (no I/O).
//!
//! Model mirrors `fleet_bundle.rs`: canonical sign input = the struct
//! serialized as JSON with `sig` cleared; Ed25519 over those bytes. The
//! license binds an official catalog item to one app.mur.run account.
//! Expiry gates downloads/updates only — never installed content.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

use crate::muragent::dsse::keyid_from_pubkey;
use crate::skill::publisher_trust::MUR_OFFICIAL_PUBLISHER_KEY_FP;

/// License wire-format version. Bump on any breaking change.
pub const OFFICIAL_LICENSE_FORMAT: u32 = 1;

/// Value of the `distribution` marker stamped inside official bundle manifests.
pub const DISTRIBUTION_OFFICIAL: &str = "official";

/// A signed record binding an official catalog item to one app.mur.run account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OfficialLicense {
    pub format_version: u32,
    /// app.mur.run account id the license is bound to.
    pub user_id: String,
    /// Catalog item id, e.g. `agents/researcher` or `fleets/deep-research`.
    pub item: String,
    /// Item version this license was issued for.
    pub version: String,
    /// RFC3339 expiry (subscription end + grace). Gates downloads only.
    pub expires_at: String,
    /// Signer's Ed25519 public key, base64 (32 bytes).
    pub signer_pubkey: String,
    /// Base64 Ed25519 signature over `license_sign_input`. `None` = unsigned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sig: Option<String>,
}

/// Canonical signing input: the license serialized with `sig` cleared.
pub fn license_sign_input(l: &OfficialLicense) -> Vec<u8> {
    let mut unsigned = l.clone();
    unsigned.sig = None;
    serde_json::to_vec(&unsigned).expect("license serializes")
}

/// Sign in place: fills `signer_pubkey` from `key` and sets `sig`.
pub fn sign_license(l: &mut OfficialLicense, key: &SigningKey) {
    use base64::{Engine, engine::general_purpose::STANDARD as B64};
    l.signer_pubkey = B64.encode(key.verifying_key().as_bytes());
    l.sig = None;
    let sig: Signature = key.sign(&license_sign_input(l));
    l.sig = Some(B64.encode(sig.to_bytes()));
}

/// Verify `sig` against the embedded `signer_pubkey`. False on any failure.
pub fn verify_license_sig(l: &OfficialLicense) -> bool {
    use base64::{Engine, engine::general_purpose::STANDARD as B64};
    let Some(sig_b64) = &l.sig else { return false };
    let Ok(pk_bytes) = B64.decode(&l.signer_pubkey) else {
        return false;
    };
    let Ok(pk_arr) = <[u8; 32]>::try_from(pk_bytes.as_slice()) else {
        return false;
    };
    let Ok(vk) = VerifyingKey::from_bytes(&pk_arr) else {
        return false;
    };
    let Ok(sig_bytes) = B64.decode(sig_b64) else {
        return false;
    };
    let Ok(sig_arr) = <[u8; 64]>::try_from(sig_bytes.as_slice()) else {
        return false;
    };
    vk.verify(&license_sign_input(l), &Signature::from_bytes(&sig_arr))
        .is_ok()
}

/// Publisher-trust-style fingerprint (`ed25519-<8hex>`) of the embedded key.
pub fn license_key_fp(l: &OfficialLicense) -> Option<String> {
    use base64::{Engine, engine::general_purpose::STANDARD as B64};
    let pk = B64.decode(&l.signer_pubkey).ok()?;
    let arr = <[u8; 32]>::try_from(pk.as_slice()).ok()?;
    Some(keyid_from_pubkey(&arr))
}

/// Whether `fp` is the pinned MUR-official publisher key fingerprint.
pub fn is_official_key_fp(fp: &str) -> bool {
    fp == MUR_OFFICIAL_PUBLISHER_KEY_FP
}

/// Outcome of a full license check. Order of checks: signature → signer
/// identity → item binding → user binding (fail-closed at the first miss).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LicenseCheck {
    Ok,
    BadSignature,
    NotOfficialKey,
    WrongUser,
    WrongItem,
}

/// Full check against an expected item + logged-in user + official key fp.
/// `official_fp` is a parameter for testability; production callers pass
/// `MUR_OFFICIAL_PUBLISHER_KEY_FP`. Expiry is deliberately NOT checked here.
pub fn check_license(
    l: &OfficialLicense,
    expected_item: &str,
    user_id: &str,
    official_fp: &str,
) -> LicenseCheck {
    if !verify_license_sig(l) {
        return LicenseCheck::BadSignature;
    }
    match license_key_fp(l) {
        Some(fp) if fp == official_fp => {}
        _ => return LicenseCheck::NotOfficialKey,
    }
    if l.item != expected_item {
        return LicenseCheck::WrongItem;
    }
    if l.user_id != user_id {
        return LicenseCheck::WrongUser;
    }
    LicenseCheck::Ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    fn test_license(key: &SigningKey) -> OfficialLicense {
        let mut l = OfficialLicense {
            format_version: OFFICIAL_LICENSE_FORMAT,
            user_id: "user-123".into(),
            item: "fleets/deep-research".into(),
            version: "1.0.0".into(),
            expires_at: "2027-01-01T00:00:00Z".into(),
            signer_pubkey: String::new(),
            sig: None,
        };
        sign_license(&mut l, key);
        l
    }

    #[test]
    fn sign_verify_roundtrip() {
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let l = test_license(&key);
        assert!(verify_license_sig(&l));
    }

    #[test]
    fn tampered_field_fails_verify() {
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let mut l = test_license(&key);
        l.user_id = "someone-else".into();
        assert!(!verify_license_sig(&l));
    }

    #[test]
    fn unsigned_fails_verify() {
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let mut l = test_license(&key);
        l.sig = None;
        assert!(!verify_license_sig(&l));
    }

    #[test]
    fn check_license_matrix() {
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let l = test_license(&key);
        let fp = license_key_fp(&l).unwrap();
        assert_eq!(
            check_license(&l, "fleets/deep-research", "user-123", &fp),
            LicenseCheck::Ok
        );
        assert_eq!(
            check_license(&l, "fleets/deep-research", "user-999", &fp),
            LicenseCheck::WrongUser
        );
        assert_eq!(
            check_license(&l, "fleets/other", "user-123", &fp),
            LicenseCheck::WrongItem
        );
        assert_eq!(
            check_license(&l, "fleets/deep-research", "user-123", "ed25519-ffffffff"),
            LicenseCheck::NotOfficialKey
        );
        let mut bad = l.clone();
        bad.sig = Some("mtampered".into());
        assert_eq!(
            check_license(&bad, "fleets/deep-research", "user-123", &fp),
            LicenseCheck::BadSignature
        );
    }
}

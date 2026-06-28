//! Verify-on-install registry skills (fail-closed): content-hash pin +
//! DSSE/Ed25519 publisher signature. First real consumer of
//! `mur_common::skill::sign::verify_manifest` + `RegistrySkillEntry.content_sha256`.

use mur_common::muragent::dsse::DsseEnvelope;
use mur_common::skill::SkillManifest;
use mur_common::skill::sign::verify_manifest;

/// Result comparing resolved skill file against registry's pinned hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HashStatus {
    Match,
    Mismatch,
    /// Registry entry carried no `content_sha256` pin.
    Absent,
}

/// Result verifying manifest's DSSE publisher signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignatureStatus {
    Verified { publisher: String, key_fp: String },
    Unsigned,
    Invalid,
}

#[derive(Debug, Clone)]
pub struct VerifyOutcome {
    pub hash: HashStatus,
    pub signature: SignatureStatus,
}

impl VerifyOutcome {
    /// Hard failure — positive sign of tampering. Abort unless explicitly overridden.
    #[allow(dead_code)] // first consumer; called by Task 2 (per-agent install gate)
    pub fn is_blocking(&self) -> bool {
        matches!(self.hash, HashStatus::Mismatch)
            || matches!(self.signature, SignatureStatus::Invalid)
    }

    /// Not proven-bad, but not proven-good (unsigned / unhashed). Require `--yes`.
    #[allow(dead_code)] // first consumer; called by Task 2 (per-agent install gate)
    pub fn needs_ack(&self) -> bool {
        !self.is_blocking()
            && (matches!(self.hash, HashStatus::Absent)
                || matches!(self.signature, SignatureStatus::Unsigned))
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}

/// Verify a resolved skill file against its registry pin.
///
/// - `manifest`         — parsed publisher-authored fields (`SkillManifest`).
/// - `file_text`        — raw YAML content of the downloaded skill file.
/// - `expected_sha256`  — hex SHA-256 from the registry entry; `""` → `Absent`.
///
/// Returns a `VerifyOutcome`. Call `.is_blocking()` to fail-close before install.
///
/// # Security contract
/// - `Mismatch` or `Invalid` ⇒ `is_blocking()` — abort unless operator overrides.
/// - `Absent` or `Unsigned` ⇒ `needs_ack()` — warn and require `--yes`.
/// - `Match` + `Verified`   ⇒ neither — install silently.
#[allow(dead_code)] // first consumer; wired in Task 2 (per-agent install gate)
pub fn verify_skill_install(
    manifest: &SkillManifest,
    file_text: &str,
    expected_sha256: &str,
) -> VerifyOutcome {
    // ── 1. Content-hash check ─────────────────────────────────────────────
    let hash = if expected_sha256.is_empty() {
        HashStatus::Absent
    } else {
        let actual = sha256_hex(file_text.as_bytes());
        if actual == expected_sha256 {
            HashStatus::Match
        } else {
            HashStatus::Mismatch
        }
    };

    // ── 2. Publisher-signature check ──────────────────────────────────────
    // The full skill file (serialized `Skill`) may carry `publisher_signature`.
    // Parse `file_text` as `Skill` to extract the optional DSSE envelope JSON.
    // Failure to parse → treat as unsigned (no forged-but-unparseable category).
    let sig_json: Option<String> = serde_yaml_ng::from_str::<mur_common::skill::Skill>(file_text)
        .ok()
        .and_then(|s| s.publisher_signature);

    let signature = match sig_json {
        None => SignatureStatus::Unsigned,
        Some(envelope_json) => match verify_manifest(manifest, &envelope_json) {
            Ok(()) => {
                // Extract key fingerprint from the first DSSE signature's keyid.
                let key_fp = serde_json::from_str::<DsseEnvelope>(&envelope_json)
                    .ok()
                    .and_then(|e| e.signatures.into_iter().next())
                    .map(|s| s.keyid)
                    .unwrap_or_default();
                SignatureStatus::Verified {
                    publisher: manifest.publisher.clone(),
                    key_fp,
                }
            }
            Err(_) => SignatureStatus::Invalid,
        },
    };

    VerifyOutcome { hash, signature }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::identity::AgentIdentity;
    use mur_common::skill::{parse_canonical, sign::sign_manifest};

    const CLEAN: &str = "name: reg-skill\nversion: 1.0.0\npublisher: human:test\ndescription: d\ncategory: context\ncontent:\n abstract: x\n context: y\n";

    fn sha(s: &str) -> String {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(s.as_bytes()))
    }

    #[test]
    fn hash_match_unsigned_is_nonblocking_but_needs_ack() {
        let m = parse_canonical(CLEAN).unwrap();
        let o = verify_skill_install(&m, CLEAN, &sha(CLEAN));
        assert!(matches!(o.hash, HashStatus::Match));
        assert!(matches!(o.signature, SignatureStatus::Unsigned));
        assert!(!o.is_blocking()); // unsigned is not a hard block
        assert!(o.needs_ack()); // but unsigned requires --yes
    }

    #[test]
    fn hash_mismatch_is_blocking() {
        let m = parse_canonical(CLEAN).unwrap();
        let o = verify_skill_install(&m, CLEAN, "deadbeef");
        assert!(matches!(o.hash, HashStatus::Mismatch));
        assert!(o.is_blocking());
        assert!(!o.needs_ack());
    }

    #[test]
    fn absent_hash_needs_ack() {
        let m = parse_canonical(CLEAN).unwrap();
        let o = verify_skill_install(&m, CLEAN, "");
        assert!(matches!(o.hash, HashStatus::Absent));
        assert!(!o.is_blocking());
        assert!(o.needs_ack());
    }

    #[test]
    fn signed_and_hash_verified_is_clean() {
        let id = AgentIdentity::generate();
        let m = parse_canonical(CLEAN).unwrap();
        let env = sign_manifest(&m, &id).unwrap();

        // Build a full Skill YAML with publisher_signature embedded.
        let skill = mur_common::skill::Skill {
            manifest: m.clone(),
            content_sha256: None,
            trust_level: mur_common::skill::TrustLevel::Sandboxed,
            capabilities_declared: vec![],
            publisher_signature: Some(env),
        };
        let skill_yaml = serde_yaml_ng::to_string(&skill).unwrap();

        let o = verify_skill_install(&m, &skill_yaml, &sha(&skill_yaml));
        assert!(matches!(o.hash, HashStatus::Match));
        assert!(matches!(o.signature, SignatureStatus::Verified { .. }));
        assert!(!o.is_blocking());
        assert!(!o.needs_ack()); // signed + hash match = fully verified
    }

    #[test]
    fn invalid_sig_is_blocking() {
        // publisher_signature present but not valid DSSE JSON → Invalid.
        let m = parse_canonical(CLEAN).unwrap();
        let skill_yaml = format!("{CLEAN}publisher_signature: 'not-valid-json'\n");
        let o = verify_skill_install(&m, &skill_yaml, &sha(&skill_yaml));
        assert!(matches!(o.signature, SignatureStatus::Invalid));
        assert!(o.is_blocking());
        assert!(!o.needs_ack());
    }
}

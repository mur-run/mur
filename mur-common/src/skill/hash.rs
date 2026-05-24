use crate::skill::manifest::SkillManifest;
use crate::skill::serialize_canonical;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// Hex-encoded SHA-256 of the canonical YAML form. Lowercase.
pub fn content_sha256(m: &SkillManifest) -> Result<String, crate::skill::ParseError> {
    let yaml = serialize_canonical(m)?;
    Ok(sha256_hex(yaml.as_bytes()))
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let hash = Sha256::digest(bytes);
    let mut s = String::with_capacity(64);
    for b in hash {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

/// Constant-time hex-string comparison. Used by drift detection.
pub fn ct_eq_hex(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

#[derive(Debug, PartialEq, Eq)]
pub enum DriftStatus {
    Pinned,
    Drift { expected: String, actual: String },
    Unpinned,
}

pub fn drift_status(m: &SkillManifest, expected: Option<&str>) -> Result<DriftStatus, crate::skill::ParseError> {
    let actual = content_sha256(m)?;
    Ok(match expected {
        None => DriftStatus::Unpinned,
        Some(exp) if ct_eq_hex(exp, &actual) => DriftStatus::Pinned,
        Some(exp) => DriftStatus::Drift { expected: exp.to_string(), actual },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skill::parse_canonical;

    const SAMPLE: &str = r#"
name: hashable
version: 1.0.0
publisher: human:t
description: d
category: context
content:
  abstract: a
  context: body
"#;

    #[test]
    fn deterministic_hash() {
        let m = parse_canonical(SAMPLE).unwrap();
        let h1 = content_sha256(&m).unwrap();
        let h2 = content_sha256(&m).unwrap();
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64);
    }

    #[test]
    fn drift_detected_when_field_changed() {
        let m1 = parse_canonical(SAMPLE).unwrap();
        let h1 = content_sha256(&m1).unwrap();
        let mut m2 = m1.clone();
        m2.description = "tampered".into();
        let s = drift_status(&m2, Some(&h1)).unwrap();
        assert!(matches!(s, DriftStatus::Drift { .. }));
    }

    #[test]
    fn pinned_matches() {
        let m = parse_canonical(SAMPLE).unwrap();
        let h = content_sha256(&m).unwrap();
        assert_eq!(drift_status(&m, Some(&h)).unwrap(), DriftStatus::Pinned);
    }

    #[test]
    fn ct_eq_hex_rejects_unequal_length() {
        assert!(!ct_eq_hex("aa", "aaa"));
    }
}

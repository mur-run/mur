use crate::skill::manifest::SkillManifest;
use crate::skill::serialize_canonical;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// Compute content hash for trust verification — excludes `transfer_chain`
/// and `evolution_log` so registry lookup and trust-store keys remain stable
/// across transfers and across generation increments.
pub fn content_hash_for_trust(m: &SkillManifest) -> Result<String, crate::skill::ParseError> {
    let mut clone = m.clone();
    clone.transfer_chain = vec![];
    clone.evolution_log = vec![];
    content_sha256(&clone)
}

/// Origin-stable content hash — excludes `origin`, `origin_version`,
/// `origin_hash` (the stamp itself) plus `transfer_chain`/`evolution_log`
/// (trust-excluded already) so restamping a skill never changes its own
/// hash, but a real content edit always does.
pub fn content_hash_for_origin(m: &SkillManifest) -> Result<String, crate::skill::ParseError> {
    let mut clone = m.clone();
    clone.transfer_chain = vec![];
    clone.evolution_log = vec![];
    clone.origin = None;
    clone.origin_version = None;
    clone.origin_hash = None;
    content_sha256(&clone)
}

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

pub fn drift_status(
    m: &SkillManifest,
    expected: Option<&str>,
) -> Result<DriftStatus, crate::skill::ParseError> {
    let actual = content_sha256(m)?;
    Ok(match expected {
        None => DriftStatus::Unpinned,
        Some(exp) if ct_eq_hex(exp, &actual) => DriftStatus::Pinned,
        Some(exp) => DriftStatus::Drift {
            expected: exp.to_string(),
            actual,
        },
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

    #[test]
    fn trust_hash_is_stable_across_transfers() {
        let m = parse_canonical(SAMPLE).unwrap();
        let h1 = content_hash_for_trust(&m).unwrap();
        let mut m2 = m.clone();
        m2.transfer_chain = vec!["agent://alice".into(), "agent://bob".into()];
        let h2 = content_hash_for_trust(&m2).unwrap();
        assert_eq!(h1, h2);
    }

    #[test]
    fn trust_hash_is_stable_across_evolution() {
        use crate::skill::EvolutionEvent;
        let m = parse_canonical(SAMPLE).unwrap();
        let h1 = content_hash_for_trust(&m).unwrap();
        let mut m2 = m.clone();
        m2.evolution_log = vec![EvolutionEvent {
            version: "0.2.0".into(),
            generation: 0,
            source: "agent:self".into(),
            changes: "tweak".into(),
            quality_score: None,
            timestamp: "2026-05-25T00:00:00Z".into(),
        }];
        let h2 = content_hash_for_trust(&m2).unwrap();
        assert_eq!(h1, h2);
    }

    #[test]
    fn origin_hash_stable_across_restamp_but_not_content_edit() {
        let m = parse_canonical(SAMPLE).unwrap();
        let before = content_hash_for_origin(&m).unwrap();
        let mut m2 = m.clone();
        m2.origin = Some("registry:mur-official/hashable".into());
        m2.origin_version = Some("1.1.0".into());
        m2.origin_hash = Some(before.clone());
        assert_eq!(content_hash_for_origin(&m2).unwrap(), before);

        // but real content changes DO change it
        m2.description = "changed".into();
        assert_ne!(content_hash_for_origin(&m2).unwrap(), before);
    }

    #[test]
    fn trust_hash_differs_when_content_changes() {
        let m = parse_canonical(SAMPLE).unwrap();
        let mut m2 = m.clone();
        m2.description = "changed".into();
        assert_ne!(
            content_hash_for_trust(&m).unwrap(),
            content_hash_for_trust(&m2).unwrap()
        );
    }
}

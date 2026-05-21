//! in-toto v1 Statement with subject hashes.
//!
//! Spec §6.3: the Statement binds every file in the `.muragent` tarball
//! (except the manifest and signature files themselves) to a SHA-256 digest.
//! The predicate carries `manifest_sha256` — the SHA-256 of `manifest.signed.json`.

use serde::{Deserialize, Serialize};
use sha2::Digest;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InTotoStatement {
    #[serde(rename = "_type")]
    pub type_: String,
    pub subject: Vec<SubjectEntry>,
    #[serde(rename = "predicateType")]
    pub predicate_type: String,
    pub predicate: Predicate,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubjectEntry {
    pub name: String,
    pub digest: SubjectDigest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubjectDigest {
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Predicate {
    pub manifest_sha256: String,
}

/// Files excluded from the Statement subject list.
const EXCLUDED_FILES: &[&str] = &["manifest.yaml", "signatures.json", "manifest.signed.json"];

/// Build an in-toto Statement from a list of (path, content_bytes) for every
/// file in the tarball.
pub fn build_statement(
    manifest_signed_json_bytes: &[u8],
    tarball_files: &[(String, Vec<u8>)],
) -> InTotoStatement {
    let manifest_sha256 = hex::encode(sha2::Sha256::digest(manifest_signed_json_bytes));

    let mut subjects: Vec<SubjectEntry> = tarball_files
        .iter()
        .filter(|(path, _)| !EXCLUDED_FILES.contains(&path.as_str()))
        .map(|(path, content)| {
            let hash = hex::encode(sha2::Sha256::digest(content));
            SubjectEntry {
                name: path.clone(),
                digest: SubjectDigest { sha256: hash },
            }
        })
        .collect();

    subjects.sort_by(|a, b| a.name.cmp(&b.name));

    InTotoStatement {
        type_: "https://in-toto.io/Statement/v1".into(),
        subject: subjects,
        predicate_type: "https://mur.run/agent-manifest/v1".into(),
        predicate: Predicate { manifest_sha256 },
    }
}

/// Verify that every subject in the statement exists in the tarball with
/// matching hash, and every tarball file (excluding EXCLUDED_FILES) is listed.
pub fn verify_subjects(
    statement: &InTotoStatement,
    tarball_files: &[(String, Vec<u8>)],
) -> Result<(), crate::muragent::MuragentError> {
    let tarball_map: std::collections::BTreeMap<&str, &[u8]> = tarball_files
        .iter()
        .filter(|(p, _)| !EXCLUDED_FILES.contains(&p.as_str()))
        .map(|(p, c)| (p.as_str(), c.as_slice()))
        .collect();

    for subject in &statement.subject {
        match tarball_map.get(subject.name.as_str()) {
            None => {
                return Err(crate::muragent::MuragentError::MissingSubject(
                    subject.name.clone(),
                ));
            }
            Some(content) => {
                let actual_hash = hex::encode(sha2::Sha256::digest(content));
                if actual_hash != subject.digest.sha256 {
                    return Err(crate::muragent::MuragentError::SubjectHashMismatch {
                        path: subject.name.clone(),
                        expected: subject.digest.sha256.clone(),
                        actual: actual_hash,
                    });
                }
            }
        }
    }

    for path in tarball_map.keys() {
        if !statement.subject.iter().any(|s| &s.name == path) {
            return Err(crate::muragent::MuragentError::ExtraFile(format!(
                "tarball file '{}' not listed in statement subjects",
                path
            )));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_statement_matches_spec_shape() {
        let manifest_json = br#"{"schema":"mur-agent/2"}"#;
        let files = vec![
            ("icon/icon.png".to_string(), b"fake-png-data".to_vec()),
            ("profile.yaml".to_string(), b"profile: content".to_vec()),
            ("manifest.yaml".to_string(), b"should be excluded".to_vec()),
            (
                "signatures.json".to_string(),
                b"should be excluded".to_vec(),
            ),
            (
                "manifest.signed.json".to_string(),
                b"should be excluded".to_vec(),
            ),
        ];
        let stmt = build_statement(manifest_json, &files);

        assert_eq!(stmt.type_, "https://in-toto.io/Statement/v1");
        assert_eq!(stmt.predicate_type, "https://mur.run/agent-manifest/v1");
        assert_eq!(stmt.subject.len(), 2);
        assert!(stmt.subject.iter().any(|s| s.name == "icon/icon.png"));
        assert!(stmt.subject.iter().any(|s| s.name == "profile.yaml"));
    }

    #[test]
    fn subjects_sorted_lexicographically() {
        let files = vec![
            ("zzz.txt".to_string(), b"z".to_vec()),
            ("aaa.txt".to_string(), b"a".to_vec()),
            ("mmm.txt".to_string(), b"m".to_vec()),
        ];
        let stmt = build_statement(b"manifest", &files);
        let names: Vec<&str> = stmt.subject.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["aaa.txt", "mmm.txt", "zzz.txt"]);
    }

    #[test]
    fn verify_subjects_passes_for_matching() {
        let manifest_json = br#"{}"#;
        let files = vec![("profile.yaml".to_string(), b"hello".to_vec())];
        let stmt = build_statement(manifest_json, &files);
        verify_subjects(&stmt, &files).unwrap();
    }

    #[test]
    fn verify_subjects_fails_on_mismatch() {
        let manifest_json = br#"{}"#;
        let files = vec![("profile.yaml".to_string(), b"hello".to_vec())];
        let stmt = build_statement(manifest_json, &files);
        let tampered = vec![("profile.yaml".to_string(), b"goodbye".to_vec())];
        assert!(verify_subjects(&stmt, &tampered).is_err());
    }

    #[test]
    fn verify_subjects_fails_on_extra_file() {
        let manifest_json = br#"{}"#;
        let files = vec![("profile.yaml".to_string(), b"hello".to_vec())];
        let stmt = build_statement(manifest_json, &files);
        let with_extra = vec![
            ("profile.yaml".to_string(), b"hello".to_vec()),
            ("extra.txt".to_string(), b"surprise".to_vec()),
        ];
        assert!(verify_subjects(&stmt, &with_extra).is_err());
    }

    #[test]
    fn verify_subjects_fails_on_missing_subject() {
        let manifest_json = br#"{}"#;
        let files = vec![("profile.yaml".to_string(), b"hello".to_vec())];
        let stmt = build_statement(manifest_json, &files);
        let missing: Vec<(String, Vec<u8>)> = vec![];
        assert!(verify_subjects(&stmt, &missing).is_err());
    }
}

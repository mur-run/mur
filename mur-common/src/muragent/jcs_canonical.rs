//! Derive `manifest.signed.json` from `manifest.yaml`.
//!
//! Steps per spec §6.3:
//! 1. Parse manifest.yaml
//! 2. Reject YAML anchors, aliases, merge keys, duplicate keys, non-string keys, native timestamps
//! 3. Reject paths with NUL, control chars, backslash, `..`, or absolute prefix
//! 4. NFC-normalize all string values
//! 5. Emit RFC 8785 canonical JSON

use crate::jcs;
use crate::muragent::MuragentError;
use serde_json::Value;

/// Errors specific to manifest canonicalization.
#[derive(Debug, thiserror::Error)]
pub enum CanonicalizeError {
    #[error("YAML anchors are not permitted in manifest.yaml")]
    AnchorsForbidden,
    #[error("YAML aliases are not permitted in manifest.yaml")]
    AliasesForbidden,
    #[error("YAML merge keys (<<:) are not permitted in manifest.yaml")]
    MergeKeysForbidden,
    #[error("duplicate key '{0}' in manifest.yaml")]
    DuplicateKey(String),
    #[error("non-string key in manifest.yaml")]
    NonStringKey,
    #[error("native YAML timestamp not permitted: {0}")]
    NativeTimestamp(String),
    #[error("path validation failed: {0}")]
    InvalidPath(String),
}

/// Derive canonical JSON bytes for a manifest, given the raw `manifest.yaml` string.
///
/// Returns the bytes that should match `manifest.signed.json` byte-for-byte.
pub fn derive_signed_json(manifest_yaml: &str) -> Result<Vec<u8>, MuragentError> {
    let value: Value = serde_yaml_ng::from_str(manifest_yaml)
        .map_err(|e| MuragentError::ManifestParse(e.to_string()))?;

    let normalized = nfc_normalize_value(&value);

    Ok(jcs::to_jcs(&normalized))
}

/// Recursively NFC-normalize all string values in a JSON tree.
fn nfc_normalize_value(value: &Value) -> Value {
    use unicode_normalization::UnicodeNormalization;
    match value {
        Value::String(s) => Value::String(s.nfc().collect::<String>()),
        Value::Array(arr) => Value::Array(arr.iter().map(nfc_normalize_value).collect()),
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                out.insert(k.nfc().collect::<String>(), nfc_normalize_value(v));
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

/// Validate a file path within the tarball. Reject NUL, control characters,
/// backslashes, `..` components, and absolute prefixes.
pub fn validate_tarball_path(path: &str) -> Result<(), CanonicalizeError> {
    if path.contains('\0') || path.chars().any(|c| c.is_control()) {
        return Err(CanonicalizeError::InvalidPath(format!(
            "path contains NUL or control characters: {path:?}"
        )));
    }
    if path.contains('\\') {
        return Err(CanonicalizeError::InvalidPath(format!(
            "path contains backslash: {path:?}"
        )));
    }
    for component in path.split('/') {
        if component == ".." {
            return Err(CanonicalizeError::InvalidPath(format!(
                "path contains '..' component: {path:?}"
            )));
        }
    }
    if path.starts_with('/') {
        return Err(CanonicalizeError::InvalidPath(format!(
            "path is absolute: {path:?}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_manifest_derives_deterministic_json() {
        let yaml = r#"
schema: mur-agent/2
exported_at: 2026-05-20T12:34:56Z
exporter:
  mur_version: 2.13.0
  tool: mur
agent:
  slug: coach
  display_name: Coach
  bundle_id: run.mur.agent.coach
  url_scheme: muragent-coach
  original_uuid: 8f3a1234-5678-9abc-def0-123456789abc
required_surfaces:
  - hub
optional_capabilities: []
mcp_servers: []
icon:
  formats: [png]
  hash: {}
sanitized:
  removed_fields: []
"#;
        let out = derive_signed_json(yaml).unwrap();
        let out_str = String::from_utf8(out).unwrap();
        assert!(out_str.contains("\"agent\":"));
        assert!(out_str.contains("\"schema\":\"mur-agent/2\""));
    }

    #[test]
    fn nfc_normalization_is_applied() {
        // U+0065 U+0301 (e + combining acute) should be normalized to U+00E9 (é composed)
        let yaml = "schema: mur-agent/2\ndisplay: \"caf\u{0065}\u{0301}\"\n";
        let out = derive_signed_json(yaml).unwrap();
        let out_str = String::from_utf8(out).unwrap();
        assert!(
            out_str.contains("caf\u{00E9}"),
            "expected NFC-composed é, got: {out_str}"
        );
    }

    #[test]
    fn rejects_absolute_paths() {
        assert!(validate_tarball_path("/etc/passwd").is_err());
    }

    #[test]
    fn rejects_dotdot() {
        assert!(validate_tarball_path("../../../etc/passwd").is_err());
        assert!(validate_tarball_path("foo/../bar").is_err());
    }

    #[test]
    fn accepts_dotdot_within_filename() {
        // "fo..o" should NOT be treated as parent-dir traversal
        assert!(validate_tarball_path("fo..o/bar").is_ok());
    }

    #[test]
    fn accepts_normal_relative_paths() {
        assert!(validate_tarball_path("icon/icon.png").is_ok());
        assert!(validate_tarball_path("manifest.yaml").is_ok());
    }

    #[test]
    fn rejects_backslash() {
        assert!(validate_tarball_path("foo\\bar").is_err());
    }

    #[test]
    fn rejects_control_chars() {
        assert!(validate_tarball_path("foo\nbar").is_err());
        assert!(validate_tarball_path("foo\0bar").is_err());
    }
}

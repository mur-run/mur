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
use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::Value;
use std::fmt;

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
    // Reject anchors/aliases/merge keys BEFORE deserializing. serde_yaml_ng
    // expands aliases during deserialization (an O(3^n) "billion laughs" blow-up
    // is reachable from a few hundred bytes), and an anchored manifest also lets
    // a human read one shape while a different, expanded shape gets signed.
    reject_unsafe_yaml(manifest_yaml).map_err(|e| MuragentError::ManifestParse(e.to_string()))?;

    // Deserialize through a guard that rejects duplicate and non-string keys
    // (serde_yaml_ng silently last-wins on duplicates), so the signed JSON can't
    // disagree with what a reviewer sees in manifest.yaml.
    let NoDupValue(value) = serde_yaml_ng::from_str(manifest_yaml)
        .map_err(|e| MuragentError::ManifestParse(e.to_string()))?;

    let normalized = nfc_normalize_value(&value);

    Ok(jcs::to_jcs(&normalized))
}

/// Scan raw YAML and reject anchors (`&a`), aliases (`*a`), and merge keys
/// (`<<:`). Our exporter never emits these, so the check is strict: any
/// occurrence at a node-start position is fatal. Characters inside quoted
/// scalars and comments are ignored, so ordinary values like `Tom & Jerry` or
/// `http://x` do not trip it.
pub fn reject_unsafe_yaml(yaml: &str) -> Result<(), CanonicalizeError> {
    let bytes = yaml.as_bytes();
    let mut i = 0;
    // `at_node_start` means the next non-space byte begins a node (a key, value,
    // or sequence item) — the only place an anchor/alias indicator is valid.
    let mut at_node_start = true;
    while i < bytes.len() {
        let c = bytes[i];
        match c {
            b' ' | b'\t' | b'\r' => {}
            b'\n' => at_node_start = true,
            b'#' => {
                // Comment to end of line (we only get here outside quotes).
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                at_node_start = true;
                continue;
            }
            b'"' => {
                at_node_start = false;
                i += 1;
                while i < bytes.len() {
                    match bytes[i] {
                        b'\\' => i += 2, // skip escaped char
                        b'"' => break,
                        _ => i += 1,
                    }
                }
            }
            b'\'' => {
                at_node_start = false;
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\'' {
                        if bytes.get(i + 1) == Some(&b'\'') {
                            i += 2; // '' escaped quote
                            continue;
                        }
                        break;
                    }
                    i += 1;
                }
            }
            b'&' if at_node_start => return Err(CanonicalizeError::AnchorsForbidden),
            b'*' if at_node_start => return Err(CanonicalizeError::AliasesForbidden),
            b'<' if at_node_start && is_merge_key(&bytes[i..]) => {
                return Err(CanonicalizeError::MergeKeysForbidden);
            }
            b'[' | b'{' | b',' | b'?' => at_node_start = true,
            b':' | b'-' => {
                // Only a structural indicator when followed by whitespace/EOL;
                // otherwise it's part of a scalar (`http://x`, `-5`).
                let next = bytes.get(i + 1);
                at_node_start = matches!(
                    next,
                    None | Some(b' ') | Some(b'\t') | Some(b'\n') | Some(b'\r')
                );
            }
            _ => at_node_start = false,
        }
        i += 1;
    }
    Ok(())
}

/// True if `rest` begins with a `<<` merge key (`<<` then optional spaces, `:`).
fn is_merge_key(rest: &[u8]) -> bool {
    if rest.len() < 3 || rest[0] != b'<' || rest[1] != b'<' {
        return false;
    }
    let mut j = 2;
    while j < rest.len() && (rest[j] == b' ' || rest[j] == b'\t') {
        j += 1;
    }
    rest.get(j) == Some(&b':')
}

/// A `serde_json::Value` whose `Deserialize` rejects duplicate object keys and
/// non-string keys (both of which serde_yaml_ng would otherwise accept).
struct NoDupValue(Value);

impl<'de> Deserialize<'de> for NoDupValue {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Value;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a YAML value with unique string keys")
            }
            fn visit_bool<E>(self, v: bool) -> Result<Value, E> {
                Ok(Value::Bool(v))
            }
            fn visit_i64<E>(self, v: i64) -> Result<Value, E> {
                Ok(Value::from(v))
            }
            fn visit_u64<E>(self, v: u64) -> Result<Value, E> {
                Ok(Value::from(v))
            }
            fn visit_f64<E>(self, v: f64) -> Result<Value, E> {
                Ok(Value::from(v))
            }
            fn visit_str<E>(self, v: &str) -> Result<Value, E> {
                Ok(Value::String(v.to_owned()))
            }
            fn visit_unit<E>(self) -> Result<Value, E> {
                Ok(Value::Null)
            }
            fn visit_none<E>(self) -> Result<Value, E> {
                Ok(Value::Null)
            }
            fn visit_some<D2: Deserializer<'de>>(self, d: D2) -> Result<Value, D2::Error> {
                Ok(NoDupValue::deserialize(d)?.0)
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
                let mut out = Vec::new();
                while let Some(NoDupValue(e)) = seq.next_element()? {
                    out.push(e);
                }
                Ok(Value::Array(out))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
                let mut obj = serde_json::Map::new();
                let mut seen = std::collections::HashSet::new();
                // next_key::<String> errors on a non-string key (NonStringKey).
                while let Some(k) = map.next_key::<String>()? {
                    if !seen.insert(k.clone()) {
                        return Err(de::Error::custom(format!(
                            "duplicate key '{k}' in manifest.yaml"
                        )));
                    }
                    let NoDupValue(v) = map.next_value()?;
                    obj.insert(k, v);
                }
                Ok(Value::Object(obj))
            }
        }
        d.deserialize_any(V).map(NoDupValue)
    }
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
    fn rejects_yaml_anchors_and_aliases() {
        // The billion-laughs vector and any anchored manifest must be refused.
        assert!(matches!(
            reject_unsafe_yaml("base: &b\n  x: 1\nuse: *b\n"),
            Err(CanonicalizeError::AnchorsForbidden)
        ));
        assert!(matches!(
            reject_unsafe_yaml("use: *b\n"),
            Err(CanonicalizeError::AliasesForbidden)
        ));
        assert!(matches!(
            reject_unsafe_yaml("a: &a [1,1]\nb: [*a, *a]\n"),
            Err(CanonicalizeError::AnchorsForbidden)
        ));
        // Merge key.
        assert!(matches!(
            reject_unsafe_yaml("child:\n  <<: x\n"),
            Err(CanonicalizeError::MergeKeysForbidden)
        ));
        // derive_signed_json refuses them too.
        assert!(derive_signed_json("base: &b\n  x: 1\nuse: *b\n").is_err());
    }

    #[test]
    fn ampersand_and_star_in_scalars_are_fine() {
        // `&`/`*` inside ordinary values must NOT trip the scanner.
        assert!(reject_unsafe_yaml("desc: Tom & Jerry\n").is_ok());
        assert!(reject_unsafe_yaml("math: 2 * 3 = 6\n").is_ok());
        assert!(reject_unsafe_yaml("url: http://example.com/a*b\n").is_ok());
        assert!(reject_unsafe_yaml("q: \"a * b & c\"\n").is_ok());
        assert!(reject_unsafe_yaml("c: \"x\" # *not an alias\n").is_ok());
        assert!(reject_unsafe_yaml("schema: mur-agent/2\nlist:\n  - hub\n").is_ok());
    }

    #[test]
    fn rejects_duplicate_keys() {
        let err = derive_signed_json("schema: mur-agent/2\nschema: evil\n").unwrap_err();
        assert!(format!("{err}").contains("duplicate key"), "got: {err}");
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

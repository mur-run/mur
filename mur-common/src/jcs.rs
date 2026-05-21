//! RFC 8785 JSON Canonicalization Scheme (JCS).
//!
//! Separate from `canonical.rs` — this implements the full JCS spec
//! including number canonicalization rules that the existing module
//! explicitly excludes. The two canonical forms serve different contracts.

use serde::Serialize;

/// Serialize a value to RFC 8785 canonical JSON bytes.
pub fn to_jcs(value: &serde_json::Value) -> Vec<u8> {
    serde_jcs::to_vec(value).expect("JCS serialization is infallible for valid JSON")
}

/// Serialize any `Serialize` type to RFC 8785 canonical JSON bytes.
pub fn to_jcs_for<T: Serialize>(value: &T) -> Vec<u8> {
    serde_jcs::to_vec(value).expect("JCS serialization should not fail for valid types")
}

/// Compute SHA-256 of the canonical JSON bytes of a value.
pub fn jcs_sha256(value: &serde_json::Value) -> String {
    use sha2::Digest;
    let bytes = to_jcs(value);
    format!("{:x}", sha2::Sha256::digest(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn scientific_notation_is_expanded() {
        let v = json!(1e10);
        let out = String::from_utf8(to_jcs(&v)).unwrap();
        assert_eq!(out, "10000000000");
    }

    #[test]
    fn trailing_zeros_removed() {
        let v = json!(1.0);
        let out = String::from_utf8(to_jcs(&v)).unwrap();
        assert_eq!(out, "1");
    }

    #[test]
    fn very_small_number_no_scientific() {
        let v = json!(0.00001);
        let out = String::from_utf8(to_jcs(&v)).unwrap();
        assert!(
            !out.contains('e') && !out.contains('E'),
            "expected no scientific notation, got: {out}"
        );
    }

    #[test]
    fn keys_sorted_lexicographically() {
        let v = json!({"z": 1, "a": 2, "m": 3});
        let out = String::from_utf8(to_jcs(&v)).unwrap();
        assert_eq!(out, r#"{"a":2,"m":3,"z":1}"#);
    }

    #[test]
    fn nested_keys_sorted_recursively() {
        let v = json!({"b": {"z": 1, "a": 2}, "a": 1});
        let out = String::from_utf8(to_jcs(&v)).unwrap();
        assert_eq!(out, r#"{"a":1,"b":{"a":2,"z":1}}"#);
    }

    #[test]
    fn boolean_and_null_preserved() {
        assert_eq!(String::from_utf8(to_jcs(&json!(true))).unwrap(), "true");
        assert_eq!(String::from_utf8(to_jcs(&json!(null))).unwrap(), "null");
    }

    #[test]
    fn deterministic_across_insertion_order() {
        let a: serde_json::Value = serde_json::from_str(r#"{"b":1,"a":2}"#).unwrap();
        let b: serde_json::Value = serde_json::from_str(r#"{"a":2,"b":1}"#).unwrap();
        assert_eq!(to_jcs(&a), to_jcs(&b));
    }
}

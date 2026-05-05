//! Canonical JSON serialization (sorted keys, no whitespace).
//!
//! Single source of truth used wherever two parties need to compute
//! identical byte sequences over a JSON value:
//!
//! - identity rotation attestations (`mur_common::identity`)
//! - card signing (`mur_common::card`)
//! - MCP description hashing (B0 rule 6 / M9.2)
//!
//! The algorithm walks the `serde_json::Value` tree depth-first,
//! sorts object keys lexicographically, and emits each leaf without
//! whitespace. It does NOT attempt RFC 8785 (JCS) compliance — that
//! would require number-canonicalisation rules we don't need for any
//! current caller. Two callers using this helper round-trip
//! losslessly; that's the only invariant we promise.

/// Returns the canonical-JSON byte sequence for `value`. The output
/// is deterministic across Rust runs, machine architectures, and
/// `serde_json` versions (assuming the input `Value` is the same).
pub fn canonical_json(value: &serde_json::Value) -> Vec<u8> {
    let mut out = Vec::new();
    write_canonical(&mut out, value);
    out
}

/// Serialize `value` directly to canonical-JSON bytes without an
/// intermediate `serde_json::Value`. For types that already implement
/// `Serialize`, this avoids the round-trip allocation.
pub fn canonical_json_for<T: serde::Serialize>(value: &T) -> Vec<u8> {
    let v: serde_json::Value =
        serde_json::to_value(value).expect("Serialize → Value should not fail for our types");
    canonical_json(&v)
}

fn write_canonical(out: &mut Vec<u8>, v: &serde_json::Value) {
    use serde_json::Value;
    match v {
        Value::Null => out.extend_from_slice(b"null"),
        Value::Bool(b) => out.extend_from_slice(if *b { b"true" } else { b"false" }),
        Value::Number(n) => out.extend_from_slice(n.to_string().as_bytes()),
        Value::String(s) => {
            let escaped = serde_json::to_string(s).expect("string serialization is infallible");
            out.extend_from_slice(escaped.as_bytes());
        }
        Value::Array(arr) => {
            out.push(b'[');
            for (i, item) in arr.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                write_canonical(out, item);
            }
            out.push(b']');
        }
        Value::Object(map) => {
            // Sort keys lexicographically for deterministic output.
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push(b'{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                let kesc = serde_json::to_string(k).expect("string serialization is infallible");
                out.extend_from_slice(kesc.as_bytes());
                out.push(b':');
                write_canonical(out, &map[*k]);
            }
            out.push(b'}');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn primitives_roundtrip() {
        assert_eq!(canonical_json(&json!(null)), b"null");
        assert_eq!(canonical_json(&json!(true)), b"true");
        assert_eq!(canonical_json(&json!(false)), b"false");
        assert_eq!(canonical_json(&json!(42)), b"42");
        assert_eq!(canonical_json(&json!(0.5)), b"0.5");
        assert_eq!(canonical_json(&json!("hi")), br#""hi""#);
    }

    #[test]
    fn object_keys_are_sorted() {
        let v = json!({"b": 1, "a": 2, "c": 3});
        assert_eq!(canonical_json(&v), br#"{"a":2,"b":1,"c":3}"#);
    }

    #[test]
    fn nested_objects_recursively_sorted() {
        let v = json!({"x": {"b": 1, "a": 2}, "a": {"y": 1, "x": 2}});
        let want = br#"{"a":{"x":2,"y":1},"x":{"a":2,"b":1}}"#;
        assert_eq!(canonical_json(&v), want);
    }

    #[test]
    fn arrays_preserve_order() {
        let v = json!([3, 1, 2]);
        assert_eq!(canonical_json(&v), b"[3,1,2]");
    }

    #[test]
    fn whitespace_stripped() {
        // serde_json::Value normalises away source whitespace, but
        // make sure our writer doesn't introduce any either.
        let v: serde_json::Value =
            serde_json::from_str(r#" { "a" : 1 ,  "b" : [ 2 , 3 ] } "#).unwrap();
        assert_eq!(canonical_json(&v), br#"{"a":1,"b":[2,3]}"#);
    }

    #[test]
    fn key_order_is_independent_of_insertion_order() {
        let a: serde_json::Value = serde_json::from_str(r#"{"a":1,"b":2}"#).unwrap();
        let b: serde_json::Value = serde_json::from_str(r#"{"b":2,"a":1}"#).unwrap();
        assert_eq!(canonical_json(&a), canonical_json(&b));
    }

    #[test]
    fn canonical_json_for_typed_value() {
        #[derive(serde::Serialize)]
        struct Probe {
            tools: Vec<&'static str>,
            version: u32,
        }
        let p = Probe {
            tools: vec!["a", "b"],
            version: 1,
        };
        // Field order of the input struct must not affect the output —
        // the keys are sorted.
        assert_eq!(
            canonical_json_for(&p),
            br#"{"tools":["a","b"],"version":1}"#
        );
    }
}

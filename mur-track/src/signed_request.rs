//! Canonical sign-input shared by the signed daemon requests (#2116, #2379).
//!
//! Every request a sealed agent drops for `mur-daemon` is signed the same way:
//! a per-type domain tag, then the RFC 8785 form of the request body with
//! `requested_at` and `key_version` folded in. `sig` itself is never part of
//! the input (v3d ChannelEvent precedent), and because the body is serialized
//! whole, adding a field can never leave it unsigned by accident.

use chrono::{DateTime, Utc};
use serde::Serialize;

/// `domain` + JCS(`body` ∪ {requested_at, key_version}). `domain` must be
/// unique per request type, so a signature over one can never verify as
/// another.
pub fn sign_input<T: Serialize>(
    domain: &str,
    body: &T,
    requested_at: &DateTime<Utc>,
    key_version: u32,
) -> Vec<u8> {
    let mut value = serde_json::to_value(body).unwrap_or(serde_json::Value::Null);
    if let Some(obj) = value.as_object_mut() {
        obj.insert(
            "requested_at".into(),
            serde_json::Value::String(requested_at.to_rfc3339()),
        );
        obj.insert("key_version".into(), serde_json::Value::from(key_version));
    }
    let mut out = domain.as_bytes().to_vec();
    out.extend(mur_common::jcs::to_jcs(&value));
    out
}

/// Inside the acceptance window `[requested_at, requested_at + max_age]`?
/// A request from the future is refused as well.
pub fn is_fresh(requested_at: DateTime<Utc>, now: DateTime<Utc>, max_age_secs: u64) -> bool {
    let age = now.signed_duration_since(requested_at);
    age >= chrono::Duration::zero() && age <= chrono::Duration::seconds(max_age_secs as i64)
}

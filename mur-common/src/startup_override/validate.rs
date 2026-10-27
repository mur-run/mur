//! The override file's schema and the boot-time check (boot step 4).
//! Fail-closed: any failure rejects the whole file, never part of it.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{ConfirmedBy, IssuedBy, OVERRIDE_AGE_MARGIN_SECS, is_valid_pair, override_max_age};
use crate::agent::{OverrideFlags, OverrideRecord, OverrideRejection};

/// The only `version` this build understands.
pub const OVERRIDE_SCHEMA_VERSION: u32 = 1;
/// Every nonce is `NONCE_PREFIX` + `NONCE_HEX_LEN` lowercase hex digits.
pub const NONCE_PREFIX: &str = "ovr-";
pub const NONCE_HEX_LEN: usize = 32;

/// Flags this build can apply. A file asking for any other flag is rejected
/// whole. PR 3 (`no_hitl`) and PR 4 (`net_unrestricted`, `full_perms`) each
/// flip their field here once the relaxation behind it exists.
pub const SUPPORTED: OverrideFlags = OverrideFlags {
    no_hitl: false,
    net_unrestricted: false,
    full_perms: false,
};

pub const REASON_UNPARSEABLE: &str = "override rejected: unparseable";
pub const REASON_UNKNOWN_CONFIRMED_BY: &str = "override rejected: unknown confirmed_by";
pub const REASON_UNKNOWN_ISSUED_BY: &str = "override rejected: unknown issued_by";
pub const REASON_UNKNOWN_FIELD: &str = "override rejected: unknown field";
pub const REASON_UNSUPPORTED_VERSION: &str = "override rejected: unsupported version";
pub const REASON_MALFORMED_NONCE: &str = "override rejected: malformed nonce";
pub const REASON_STALE: &str = "override rejected: stale";
pub const REASON_FUTURE: &str = "override rejected: created in the future";
pub const REASON_INVALID_PAIR: &str = "override rejected: invalid issued_by/confirmed_by pair";
/// Followed by the field name, e.g. `override rejected: unsupported flag: no_hitl`.
pub const REASON_UNSUPPORTED_FLAG_PREFIX: &str = "override rejected: unsupported flag: ";

/// `<mur_home>/secrets/startup-overrides/<agent>.json`, as written by
/// `mur agent start|restart` and the Hub.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct OverrideFile {
    pub version: u32,
    pub nonce: String,
    pub created_at: DateTime<Utc>,
    pub issued_by: IssuedBy,
    pub confirmed_by: ConfirmedBy,
    pub no_hitl: bool,
    pub net_unrestricted: bool,
    pub full_perms: bool,
}

impl OverrideFile {
    pub fn flags(&self) -> OverrideFlags {
        OverrideFlags {
            no_hitl: self.no_hitl,
            net_unrestricted: self.net_unrestricted,
            full_perms: self.full_perms,
        }
    }
}

/// `ovr-` followed by exactly 32 lowercase hex digits.
pub fn is_well_formed_nonce(nonce: &str) -> bool {
    nonce.strip_prefix(NONCE_PREFIX).is_some_and(|hex| {
        hex.len() == NONCE_HEX_LEN && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    })
}

/// Parse and check an override file. `Ok` is the record to put on the lock.
///
/// Order, first failure wins: parse → version → nonce shape → pair → age →
/// unsupported flags. `stop_timeout_secs` is the booting profile's own
/// `lifecycle.stop_timeout_secs`.
pub fn validate(
    bytes: &[u8],
    now: DateTime<Utc>,
    stop_timeout_secs: u64,
) -> Result<OverrideRecord, OverrideRejection> {
    let file: OverrideFile = serde_json::from_slice(bytes).map_err(|e| OverrideRejection {
        nonce: lenient_nonce(bytes),
        reason: classify_parse_error(bytes, &e).to_string(),
    })?;

    let reject = |reason: String| {
        let nonce = is_well_formed_nonce(&file.nonce).then(|| file.nonce.clone());
        OverrideRejection { nonce, reason }
    };

    if file.version != OVERRIDE_SCHEMA_VERSION {
        return Err(reject(REASON_UNSUPPORTED_VERSION.into()));
    }
    if !is_well_formed_nonce(&file.nonce) {
        return Err(reject(REASON_MALFORMED_NONCE.into()));
    }
    if !is_valid_pair(file.issued_by, file.confirmed_by) {
        return Err(reject(REASON_INVALID_PAIR.into()));
    }

    let max_age = chrono::Duration::from_std(override_max_age(stop_timeout_secs))
        .unwrap_or(chrono::Duration::MAX);
    if now - file.created_at > max_age {
        return Err(reject(REASON_STALE.into()));
    }
    let margin = chrono::Duration::seconds(OVERRIDE_AGE_MARGIN_SECS as i64);
    if file.created_at - now > margin {
        return Err(reject(REASON_FUTURE.into()));
    }

    let flags = file.flags();
    for (name, asked, supported) in [
        ("no_hitl", flags.no_hitl, SUPPORTED.no_hitl),
        (
            "net_unrestricted",
            flags.net_unrestricted,
            SUPPORTED.net_unrestricted,
        ),
        ("full_perms", flags.full_perms, SUPPORTED.full_perms),
    ] {
        if asked && !supported {
            return Err(reject(format!("{REASON_UNSUPPORTED_FLAG_PREFIX}{name}")));
        }
    }

    Ok(OverrideRecord {
        nonce: file.nonce,
        flags,
        confirmed_by: file.confirmed_by,
    })
}

/// The `nonce` of a file that failed strict parsing, only if it is JSON and
/// the nonce is well formed. Never trust anything else from such a file.
fn lenient_nonce(bytes: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let nonce = value.get("nonce")?.as_str()?;
    is_well_formed_nonce(nonce).then(|| nonce.to_string())
}

/// Name the strict-parse failure. An unrecognised `confirmed_by` /
/// `issued_by` string is checked on the raw value (serde's message does not
/// name the field); an unknown key comes from `deny_unknown_fields`.
fn classify_parse_error(bytes: &[u8], err: &serde_json::Error) -> &'static str {
    if let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) {
        let unknown = |key: &str, ok: fn(serde_json::Value) -> bool| {
            value
                .get(key)
                .is_some_and(|v| v.is_string() && !ok(v.clone()))
        };
        if unknown("confirmed_by", |v| {
            serde_json::from_value::<ConfirmedBy>(v).is_ok()
        }) {
            return REASON_UNKNOWN_CONFIRMED_BY;
        }
        if unknown("issued_by", |v| {
            serde_json::from_value::<IssuedBy>(v).is_ok()
        }) {
            return REASON_UNKNOWN_ISSUED_BY;
        }
    }
    if err.to_string().starts_with("unknown field") {
        return REASON_UNKNOWN_FIELD;
    }
    REASON_UNPARSEABLE
}

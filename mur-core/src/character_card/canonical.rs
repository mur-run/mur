//! Canonical-JSON encoding of `MurCard.data` (excluding the signature
//! block) for Ed25519 signing.
//!
//! Algorithm (RFC 8785-ish):
//! 1. Serialize the card as JSON via serde_json.
//! 2. Walk the tree and remove `extensions.mur.provenance.signature`
//!    (the signature can't sign itself).
//! 3. Canonicalize via `serde_jcs`: sort object keys lexically (UTF-8
//!    bytewise), strip whitespace, normalize numbers per RFC 8785 §3.2.2.
//! 4. Output as bytes.

use anyhow::{Context, Result};
use serde_json::Value;

use super::schema::MurCard;

pub fn canonical_data_bytes(card: &MurCard) -> Result<Vec<u8>> {
    let mut v = serde_json::to_value(card).context("card → Value")?;
    strip_signature(&mut v);
    let canon = serde_jcs::to_vec(&v).context("canonical JSON encode")?;
    Ok(canon)
}

fn strip_signature(v: &mut Value) {
    if let Some(obj) = v.as_object_mut()
        && let Some(ext) = obj.get_mut("extensions").and_then(|e| e.as_object_mut())
        && let Some(mur) = ext.get_mut("mur").and_then(|m| m.as_object_mut())
        && let Some(prov) = mur.get_mut("provenance").and_then(|p| p.as_object_mut())
    {
        prov.remove("signature");
    }
}

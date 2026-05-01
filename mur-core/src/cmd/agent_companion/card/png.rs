//! PNG chara / ccv3 chunk extractor.
//!
//! SillyTavern stores the card as a base64-encoded JSON string inside
//! a `tEXt` chunk. V2 uses keyword `chara`; V3 uses `ccv3`. We try V3
//! first (matches mur's preferred output format), fall back to V2.
//!
//! M4.3 lands the helpers; the `mur agent companion card import` CLI
//! dispatch is M4.7. Until then the `mur` bin has no callers, so we
//! suppress the bin-only dead-code warning here. Integration tests in
//! `mur-core/tests/card_png_import_*.rs` exercise the helpers via the
//! library.

#![allow(dead_code)]

use anyhow::{Context, Result, bail};
use base64::Engine;

const PNG_SIGNATURE: &[u8] = &[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];

/// Extract the chara/ccv3 JSON payload from a SillyTavern PNG.
/// Returns the decoded JSON string (caller deserializes into MurCard).
pub fn extract_card_json(bytes: &[u8]) -> Result<String> {
    if !bytes.starts_with(PNG_SIGNATURE) {
        bail!("not a PNG file (missing signature)");
    }
    let mut cursor = 8usize;
    let mut v3: Option<String> = None;
    let mut v2: Option<String> = None;
    while cursor + 12 <= bytes.len() {
        let len = u32::from_be_bytes(bytes[cursor..cursor + 4].try_into()?) as usize;
        let kind = &bytes[cursor + 4..cursor + 8];
        let data_start = cursor + 8;
        let data_end = data_start + len;
        if data_end + 4 > bytes.len() {
            break;
        }
        if kind == b"tEXt" {
            let chunk = &bytes[data_start..data_end];
            if let Some(zero) = chunk.iter().position(|&b| b == 0) {
                let keyword = std::str::from_utf8(&chunk[..zero]).unwrap_or("");
                let payload = &chunk[zero + 1..];
                let payload_str = std::str::from_utf8(payload).unwrap_or("").to_string();
                match keyword {
                    "ccv3" => v3 = Some(payload_str),
                    "chara" => v2 = Some(payload_str),
                    _ => {}
                }
            }
        }
        cursor = data_end + 4;
        if kind == b"IEND" {
            break;
        }
    }
    let chosen = v3.or(v2).context("PNG missing chara or ccv3 tEXt chunk")?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(chosen.as_bytes())
        .context("base64 decode card payload")?;
    let json = String::from_utf8(decoded).context("card payload not UTF-8 JSON")?;
    Ok(json)
}

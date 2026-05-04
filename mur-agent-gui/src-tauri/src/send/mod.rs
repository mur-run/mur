//! Track C3 (send-from-any-app) — shared payload schema.
//!
//! Every Track C3 channel (URL scheme, hotkey, macOS Services, dock-drop)
//! delivers user-facing share data as a [`SharePayload`]. The payload is
//! the single contract: channel front-ends construct it; the
//! [`SendIngestor`] (M-c3.0.2) routes it through the existing D3
//! multimodal pipeline; the M7 B0 hook tags it as
//! `<untrusted_share>` before it reaches the model.
//!
//! Per-channel sub-modules (`url_scheme`, `hotkey`, `services`, `dock`)
//! are stub-loaded here so downstream milestones can land them
//! incrementally without touching `lib.rs`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub mod hotkey;
pub mod url_scheme;

#[cfg(target_os = "macos")]
pub mod dock;
#[cfg(target_os = "macos")]
pub mod services;

/// Discriminated payload kind.
///
/// Serializes with an external tag (`kind`) and a single inner value
/// (`value`) so the wire form is stable across Tauri's IPC boundary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum ShareKind {
    /// Free-form text (snippet, paragraph, transcribed voice memo, …).
    Text(String),
    /// A URL string. Same wrapping pipeline as `Text` but tagged
    /// separately so callers can distinguish hyperlink-only shares.
    Url(String),
    /// Path to an image file on disk (PNG/JPEG/WebP/HEIC). Routed
    /// through D3's `process_artifact` for OCR + B0 wrapping.
    Image(PathBuf),
    /// Path to any other file on disk. Routed through D3's
    /// `process_artifact` (PDF text extraction, etc.).
    File(PathBuf),
}

/// Top-level share envelope produced by every Track C3 channel.
///
/// `source` is a stable channel tag (`"url_scheme" | "hotkey" |
/// "services" | "dock"`); the [`SendIngestor`] forwards it as
/// `format!("share:{source}")` into the provenance ledger so B0 hooks
/// and forensic readers can audit which channel introduced the bytes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SharePayload {
    /// Channel-tagged origin: `"url_scheme" | "hotkey" | "services" | "dock"`.
    pub source: String,
    pub kind: ShareKind,
    /// Free-form metadata (e.g. originating bundle id, hotkey combo).
    #[serde(default)]
    pub metadata: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn share_payload_round_trip_text() {
        let p = SharePayload {
            source: "url_scheme".into(),
            kind: ShareKind::Text("hello".into()),
            metadata: serde_json::json!({}),
        };
        let s = serde_json::to_string(&p).unwrap();
        let back: SharePayload = serde_json::from_str(&s).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn share_payload_round_trip_image() {
        let p = SharePayload {
            source: "dock".into(),
            kind: ShareKind::Image(PathBuf::from("/tmp/foo.png")),
            metadata: serde_json::json!({"size": 1024}),
        };
        let s = serde_json::to_string(&p).unwrap();
        let back: SharePayload = serde_json::from_str(&s).unwrap();
        assert_eq!(p, back);
    }
}

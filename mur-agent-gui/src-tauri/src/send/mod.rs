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
pub mod wiring;

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

/// Async surface every channel calls into.
///
/// Channel front-ends own the platform glue (URL scheme parser, hotkey
/// listener, NSServices handler, dock-drop AppleEvent) and translate
/// the platform event into a [`SharePayload`]. Anything past that point
/// — staging on disk, B0 wrapping, UI notification — is the ingestor's
/// job.
#[async_trait::async_trait]
pub trait SendIngestor: Send + Sync {
    async fn ingest(&self, payload: SharePayload) -> anyhow::Result<()>;
}

/// Side-channel for emitting a `share:received` UI event so the front
/// end can flash the dock badge / show a toast / focus the window.
///
/// Kept as a trait (rather than holding `tauri::AppHandle` directly)
/// so the ingestor unit-tests can swap in a fake counter without
/// pulling the Tauri runtime into `cargo test --test`.
pub trait ShareEmitter: Send + Sync {
    fn emit_received(&self, payload: &SharePayload) -> anyhow::Result<()>;
}

/// Default ingestor.
///
/// Routing rules:
/// - `Image`/`File` → read bytes, sniff mime via `mime_guess`, hand off
///   to [`mur_agent_runtime::multimodal::pipeline::process_artifact`].
///   The B0 hook subsequently tags the entry as `<untrusted_image_text>`
///   or `<untrusted_pdf_text>`.
/// - `Text`/`Url` → hand off to
///   [`mur_agent_runtime::multimodal::pipeline::process_share_text`],
///   which prefixes a `--- share\n` marker so B0 tags the entry as
///   `<untrusted_share>`.
///
/// `process_artifact` and `process_share_text` are both synchronous;
/// the ingestor surface stays async because channels invoke it from
/// Tauri command handlers (which are async).
pub struct DefaultIngestor {
    pub agent_home: PathBuf,
    /// Used to emit `share:received` to the front end.
    pub emitter: std::sync::Arc<dyn ShareEmitter>,
}

#[async_trait::async_trait]
impl SendIngestor for DefaultIngestor {
    async fn ingest(&self, payload: SharePayload) -> anyhow::Result<()> {
        match &payload.kind {
            ShareKind::Image(path) | ShareKind::File(path) => {
                let bytes = std::fs::read(path)?;
                let mime = mime_guess::from_path(path)
                    .first_or_octet_stream()
                    .essence_str()
                    .to_string();
                mur_agent_runtime::multimodal::pipeline::process_artifact(
                    &bytes,
                    &mime,
                    &self.agent_home,
                )?;
            }
            ShareKind::Text(body) | ShareKind::Url(body) => {
                mur_agent_runtime::multimodal::pipeline::process_share_text(
                    body,
                    &payload.source,
                    &self.agent_home,
                )?;
            }
        }
        self.emitter.emit_received(&payload)?;
        Ok(())
    }
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

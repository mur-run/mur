//! 9-step drag-drop pipeline orchestrator (D3, roadmap §4.3).
//!
//! Steps that run here: 3 (HEIC normalize), 4 (sandboxed decode +
//! re-encode), 8 (provenance ledger entry).
//!
//! Steps owned by callers / other layers:
//! * 1 dedupe — caller-side via `DropDeduper` (in `main.rs`)
//! * 2 iCloud fallback — caller-side via `icloud_fallback_bytes`
//! * 5 OCR — added to this module in M3.5.4
//! * 6 Unicode scrubber — applied to OCR output in M3.5.4
//! * 7 untrusted_image_text wrapper — `B0SafetyHook` (M3.8)
//! * 9 turn-flag — `B0SafetyHook` (M3.8)

use anyhow::{Context, Result, bail};
use chrono::Utc;
use sha2::{Digest, Sha256};
use std::path::PathBuf;

use mur_common::multimodal::{ArtifactKind, MultimodalArtifact, ProvenanceEntry, ProvenanceLedger};

use super::decode::DecoderClient;
use super::decoder_protocol::{DecodeResponse, PdfPageText};
use super::heic::heic_to_png;
use super::unicode_scrubber;

pub enum PipelineInput {
    Path {
        path: PathBuf,
        source: String,
    },
    Bytes {
        bytes: Vec<u8>,
        mime_hint: String,
        source: String,
    },
}

pub struct MultimodalPipeline {
    agent_home: PathBuf,
    turn_id: u64,
    decoder: DecoderClient,
}

impl MultimodalPipeline {
    pub fn new(agent_home: PathBuf, turn_id: u64) -> Self {
        Self {
            agent_home,
            turn_id,
            decoder: DecoderClient::new(),
        }
    }

    pub async fn process(&self, input: PipelineInput) -> Result<MultimodalArtifact> {
        let (raw_bytes, mime_hint, source) = match input {
            PipelineInput::Path { path, source } => {
                let bytes =
                    std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
                let mime_hint = mime_from_extension(&path);
                (bytes, mime_hint, source)
            }
            PipelineInput::Bytes {
                bytes,
                mime_hint,
                source,
            } => (bytes, mime_hint, source),
        };

        // Dispatch on mime BEFORE HEIC handling.
        if mime_hint == "application/pdf" {
            return self.process_pdf(raw_bytes, source).await;
        }

        // Step 3: HEIC normalization (skip for non-HEIC).
        let (bytes, mime_hint) = if mime_hint == "image/heic" || mime_hint == "image/heif" {
            (
                heic_to_png(&raw_bytes).context("HEIC → PNG")?,
                "image/png".to_string(),
            )
        } else {
            (raw_bytes, mime_hint)
        };

        // Step 4: sandboxed decode + re-encode.
        let resp = self
            .decoder
            .decode_image(bytes, &mime_hint)
            .await
            .context("DecoderClient::decode_image")?;
        let (png, decoder_version) = match resp {
            DecodeResponse::Ok {
                png_bytes,
                decoder_version,
            } => (png_bytes, decoder_version),
            DecodeResponse::Error(e) => bail!("decoder error: {e:?}"),
            DecodeResponse::PdfText { .. } => bail!("got PDF response on image path"),
        };

        // Step 8: provenance ledger entry.
        let mut hasher = Sha256::new();
        hasher.update(&png);
        let sha256 = format!("{:x}", hasher.finalize());

        let entry = ProvenanceEntry {
            sha256: sha256.clone(),
            source: source.clone(),
            decoder_version: decoder_version.clone(),
            ocr_engine_version: None, // M3.5.4 fills this
            turn_id: self.turn_id,
            recorded_at: Utc::now(),
        };
        let ledger = ProvenanceLedger::new(self.agent_home.join("telemetry/inputs.jsonl"));
        ledger.append(&entry).context("append provenance")?;

        Ok(MultimodalArtifact {
            sha256,
            kind: ArtifactKind::Image,
            mime: "image/png".into(),
            size_bytes: png.len() as u64,
            ocr_text: None, // M3.5.4 wires this
            page_count: None,
            created_at: Utc::now(),
            decoder_version,
            ocr_engine_version: None,
        })
    }

    async fn process_pdf(&self, bytes: Vec<u8>, source: String) -> Result<MultimodalArtifact> {
        let resp = self
            .decoder
            .decode_pdf(bytes.clone())
            .await
            .context("DecoderClient::decode_pdf")?;
        let (pages, decoder_version) = match resp {
            DecodeResponse::PdfText {
                pages,
                decoder_version,
            } => (pages, decoder_version),
            DecodeResponse::Error(e) => bail!("pdf decoder: {e:?}"),
            DecodeResponse::Ok { .. } => bail!("got Ok on PDF path"),
        };

        // Join page texts with quarantine markers.
        let joined = pages
            .iter()
            .map(|p: &PdfPageText| {
                if p.quarantined {
                    format!(
                        "--- page {} (quarantined: <1pt glyphs) ---\n{}",
                        p.page, p.text
                    )
                } else {
                    format!("--- page {} ---\n{}", p.page, p.text)
                }
            })
            .collect::<Vec<_>>()
            .join("\n");

        // Step 6: Unicode scrub the joined text (the model never sees raw
        // tag/bidi-override chars).
        let (scrubbed, _scrubbed_count) = unicode_scrubber::scrub(&joined);

        // Step 8: provenance ledger entry. PDF sha256 is over the input
        // bytes (we don't re-encode PDFs).
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        let sha256 = format!("{:x}", hasher.finalize());

        let entry = ProvenanceEntry {
            sha256: sha256.clone(),
            source,
            decoder_version: decoder_version.clone(),
            ocr_engine_version: None,
            turn_id: self.turn_id,
            recorded_at: Utc::now(),
        };
        ProvenanceLedger::new(self.agent_home.join("telemetry/inputs.jsonl"))
            .append(&entry)
            .context("append provenance")?;

        let page_count = pages.len() as u32;

        Ok(MultimodalArtifact {
            sha256,
            kind: ArtifactKind::Pdf,
            mime: "application/pdf".into(),
            size_bytes: bytes.len() as u64,
            ocr_text: Some(scrubbed),
            page_count: Some(page_count),
            created_at: Utc::now(),
            decoder_version,
            ocr_engine_version: None,
        })
    }
}

fn mime_from_extension(path: &std::path::Path) -> String {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("heic") | Some("heif") => "image/heic",
        Some("pdf") => "application/pdf",
        _ => "application/octet-stream",
    }
    .to_string()
}

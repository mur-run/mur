//! [`process_artifact`] — the "wrap raw bytes in a provenance entry"
//! glue used by M-c2.4 (Telegram document/photo handler) and any future
//! caller that pulls untrusted bytes onto disk.
//!
//! On success the function:
//!
//! 1. Computes the sha256 of `bytes`.
//! 2. Creates `<agent_home>/telemetry/inputs/` if missing.
//! 3. Writes `<agent_home>/telemetry/inputs/{sha}.txt` carrying a
//!    text-ish projection of the artifact (see module-level doc on
//!    `super` for what we write per mime).
//! 4. Appends a [`ProvenanceEntry`] (turn_id = 0 — the runtime stamps
//!    the real turn before the next prompt) to
//!    `<agent_home>/telemetry/inputs.jsonl`.
//! 5. Returns the sha and the ledger path so the caller can wire it
//!    into the outbound A2A envelope.
//!
//! The 0-turn-id is a deliberate "stage now, claim later" pattern: the
//! Telegram bridge ingests artifacts asynchronously w.r.t. the user
//! agent's turn counter, so the user-agent-side B0 hook (M3.8) is
//! responsible for promoting these entries into the current turn when
//! it processes the wrapped envelope. The dedicated fixup is part of
//! the M-c2.4.3 milestone (out-of-scope for the bridge itself).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use mur_common::multimodal::{ProvenanceEntry, ProvenanceLedger};
use sha2::{Digest, Sha256};

/// What [`process_artifact`] returns. `ledger_path` is the absolute
/// path to `<agent_home>/telemetry/inputs.jsonl`; callers asserting
/// "the ledger entry was appended" should check `ledger_path.exists()`.
pub struct ProcessResult {
    /// Hex-encoded sha256 of the original bytes (lowercase, 64 chars).
    pub sha256: String,
    /// Path to the JSONL ledger file. The new entry is the last line.
    pub ledger_path: PathBuf,
    /// Path to the `.txt` sidecar that B0SafetyHook reads.
    pub sidecar_path: PathBuf,
}

/// Stage `bytes` (with content-type `mime`) under `<agent_home>` and
/// append a provenance entry. See module docs for the per-mime
/// projection; in practice the projection is "good enough for B0 to
/// pick the right `<untrusted_*>` tag" and the production OCR / PDF
/// extraction wires in via subsequent D-track milestones.
pub fn process_artifact(bytes: &[u8], mime: &str, agent_home: &Path) -> Result<ProcessResult> {
    let sha = {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        format!("{:x}", hasher.finalize())
    };

    let inputs_dir = agent_home.join("telemetry/inputs");
    std::fs::create_dir_all(&inputs_dir)
        .with_context(|| format!("create {}", inputs_dir.display()))?;

    let sidecar_path = inputs_dir.join(format!("{sha}.txt"));
    let sidecar_body = sidecar_text_for(bytes, mime);
    std::fs::write(&sidecar_path, &sidecar_body)
        .with_context(|| format!("write sidecar {}", sidecar_path.display()))?;

    let ledger_path = agent_home.join("telemetry/inputs.jsonl");
    let entry = ProvenanceEntry {
        sha256: sha.clone(),
        source: source_for(mime),
        decoder_version: decoder_for(mime),
        ocr_engine_version: ocr_for(mime),
        // Bridge ingest happens out-of-turn; the user-agent-side hook
        // promotes this entry into its real turn before reading.
        turn_id: 0,
        recorded_at: Utc::now(),
    };
    ProvenanceLedger::new(&ledger_path)
        .append(&entry)
        .with_context(|| format!("append ledger {}", ledger_path.display()))?;

    Ok(ProcessResult {
        sha256: sha,
        ledger_path,
        sidecar_path,
    })
}

/// Per-mime sidecar projection. We deliberately keep this small:
///
/// - `text/*` → the bytes interpreted as UTF-8 (lossy fallback).
/// - `application/pdf` → a synthetic `--- page 1 ---\n` marker so
///   `B0SafetyHook` picks the `untrusted_pdf_text` tag (Spec §M3.8.1).
///   Real per-page text extraction lands in D3.4.
/// - `image/*` → empty (OCR wires in via a follow-up milestone).
/// - everything else → empty.
fn sidecar_text_for(bytes: &[u8], mime: &str) -> String {
    if mime.starts_with("text/") {
        String::from_utf8_lossy(bytes).into_owned()
    } else if mime == "application/pdf" {
        "--- page 1 ---\n".into()
    } else {
        String::new()
    }
}

fn source_for(_mime: &str) -> String {
    // Today every caller is the Telegram bridge; widen this when the
    // pipeline is adopted by the GUI composer / paste-handler.
    "user_drop".into()
}

fn decoder_for(mime: &str) -> String {
    match mime {
        "application/pdf" => "stub-pdf/v0".into(),
        m if m.starts_with("image/") => "stub-image/v0".into(),
        m if m.starts_with("text/") => "utf8/v1".into(),
        _ => "stub/v0".into(),
    }
}

fn ocr_for(mime: &str) -> Option<String> {
    if mime.starts_with("image/") {
        // Real OCR engine version slot; today we don't OCR.
        Some("none/v0".into())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn roundtrip_text() {
        let tmp = TempDir::new().unwrap();
        let r = process_artifact(b"hello world", "text/plain", tmp.path()).unwrap();
        assert_eq!(r.sha256.len(), 64);
        assert!(r.ledger_path.exists());
        assert!(r.sidecar_path.exists());
        assert_eq!(
            std::fs::read_to_string(&r.sidecar_path).unwrap(),
            "hello world"
        );
    }

    #[test]
    fn pdf_sidecar_starts_with_page_marker() {
        let tmp = TempDir::new().unwrap();
        let r = process_artifact(b"%PDF-1.3\n", "application/pdf", tmp.path()).unwrap();
        let body = std::fs::read_to_string(&r.sidecar_path).unwrap();
        assert!(body.starts_with("--- page"));
    }

    #[test]
    fn ledger_appended_once_per_call() {
        let tmp = TempDir::new().unwrap();
        process_artifact(b"a", "text/plain", tmp.path()).unwrap();
        process_artifact(b"b", "text/plain", tmp.path()).unwrap();
        let body = std::fs::read_to_string(tmp.path().join("telemetry/inputs.jsonl")).unwrap();
        assert_eq!(body.lines().count(), 2);
    }
}

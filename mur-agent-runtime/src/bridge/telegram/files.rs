//! Telegram document / photo handler (M-c2.4).
//!
//! Pipeline mirrors [`super::voice`]:
//!
//! 1. `getFile` download (mocked via [`MockBot::fetch_file`]).
//! 2. 20 MB cap (`FILE_MAX_BYTES`) — refuses the update before hitting
//!    the network when `update.file_size` is reported.
//! 3. Hand off to [`crate::multimodal::pipeline::process_artifact`]
//!    which writes `<agent_home>/telemetry/inputs/{sha}.txt` and
//!    appends a [`mur_common::multimodal::ProvenanceEntry`] to
//!    `<agent_home>/telemetry/inputs.jsonl`.
//! 4. Return the sha + ledger path so the inbound loop can attach an
//!    `artifact_sha256` field to the outbound `message/send` envelope.
//!
//! Photo updates are funnelled through the same code path. Real
//! Telegram photo updates carry an array of size variants; M-c2.4
//! treats `photo_file_id` on the synthetic [`MockUpdate`] as the
//! already-picked-largest variant and downloads it as a single blob.

use std::path::PathBuf;

use crate::bridge::telegram::mock::{MockBot, MockUpdate};
use crate::multimodal::pipeline;

/// 20 MB upper bound on inbound document / photo bytes. Telegram's Bot
/// API itself caps `getFile` downloads at 20 MB; rejecting before the
/// HTTP round-trip lets us short-circuit cleanly when a user attaches
/// a 25 MB PDF.
pub const FILE_MAX_BYTES: u64 = 20_000_000;

/// On-disk staging dependencies for [`handle_document_update`] and
/// [`handle_photo_update`]. `agent_home` is the agent's
/// `~/.mur/agents/<name>` (or a tempdir in tests). `mime` is the
/// content-type the caller wants stamped on the provenance entry; the
/// inbound loop derives it from `update.document.mime_type` /
/// `update.photo.mime_type` in production wiring.
pub struct FilesDeps {
    pub agent_home: PathBuf,
    pub mime: String,
}

/// Outcome of a successful document/photo ingest.
///
/// `sha256` is hex-encoded (lowercase, 64 chars). `ledger_entry` points
/// at `<agent_home>/telemetry/inputs.jsonl` — callers wanting to assert
/// "the entry exists" should check `ledger_entry.exists()` rather than
/// re-reading and parsing the JSONL line.
#[derive(Debug, Clone)]
pub struct FilesResult {
    pub ledger_entry: PathBuf,
    pub sha256: String,
}

/// Download a Telegram document, persist it under
/// `<agent_home>/telemetry/inputs/`, and append a provenance entry.
///
/// Errors:
///
/// - `document_file_id` missing on the update (caller bug).
/// - File size exceeds [`FILE_MAX_BYTES`] (we check `update.file_size`
///   BEFORE downloading; a missing field means we trust the bytes).
/// - `MockBot::fetch_file` returns no bytes (`getFile` 404 in prod).
/// - Filesystem write or ledger append failure under `agent_home`.
pub async fn handle_document_update(
    bot: &MockBot,
    update: &MockUpdate,
    deps: &FilesDeps,
) -> anyhow::Result<FilesResult> {
    if let Some(sz) = update.file_size
        && sz > FILE_MAX_BYTES
    {
        anyhow::bail!("document too large: {} bytes (cap {})", sz, FILE_MAX_BYTES);
    }
    let file_id = update
        .document_file_id
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("no document_file_id on update"))?;
    let bytes = bot.fetch_file(file_id)?;
    let res = pipeline::process_artifact(&bytes, &deps.mime, &deps.agent_home)?;
    Ok(FilesResult {
        ledger_entry: res.ledger_path,
        sha256: res.sha256,
    })
}

/// Download the (largest variant of a) Telegram photo and append a
/// provenance entry. Mirrors [`handle_document_update`]; the only
/// difference today is the `update.photo_file_id` accessor — the real
/// teloxide `Update.photo` is a `Vec<PhotoSize>` and the inbound loop
/// picks the largest before calling us, but the [`MockUpdate`] mirror
/// already collapses that to a single `Option<String>` for tests.
pub async fn handle_photo_update(
    bot: &MockBot,
    update: &MockUpdate,
    deps: &FilesDeps,
) -> anyhow::Result<FilesResult> {
    if let Some(sz) = update.file_size
        && sz > FILE_MAX_BYTES
    {
        anyhow::bail!("photo too large: {} bytes (cap {})", sz, FILE_MAX_BYTES);
    }
    let file_id = update
        .photo_file_id
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("no photo_file_id on update"))?;
    let bytes = bot.fetch_file(file_id)?;
    let res = pipeline::process_artifact(&bytes, &deps.mime, &deps.agent_home)?;
    Ok(FilesResult {
        ledger_entry: res.ledger_path,
        sha256: res.sha256,
    })
}

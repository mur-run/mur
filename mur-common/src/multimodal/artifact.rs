use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    Image,
    Pdf,
    Text,
}

/// In-memory record of one dropped/pasted artifact.
///
/// Consumed by:
/// * the GUI composer (thumbnails + remove control)
/// * `mur-agent-runtime`'s `B0SafetyHook` (untrusted wrapper injection,
///   side-effect-tool deny via the `after_untrusted_input` turn-flag).
///
/// `decoder_version` and `ocr_engine_version` are persisted in
/// `telemetry/inputs.jsonl` so a future audit can reproduce the exact
/// decode chain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultimodalArtifact {
    /// Content hash of the *re-encoded* (sanitized) bytes.
    pub sha256: String,
    pub kind: ArtifactKind,
    pub mime: String,
    pub size_bytes: u64,
    /// OCR'd text for images, extracted text for PDFs. None for `text/*`
    /// (the body itself is the text).
    pub ocr_text: Option<String>,
    /// Page count for PDFs. None for images.
    pub page_count: Option<u32>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub decoder_version: String,
    pub ocr_engine_version: Option<String>,
}

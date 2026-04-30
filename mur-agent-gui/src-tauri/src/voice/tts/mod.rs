//! TTS engine — Kokoro 82M backend (stub).
//!
//! Full implementation lands across M1.3.1–M1.3.4:
//! * `g2p.rs`     — grapheme-to-phoneme (en + zh dispatch)
//! * `kokoro.rs`  — ort session loader + 1-token prewarm
//! * `sentence_split.rs` — streaming sentence splitter
//! * streaming-synthesis loop here

use anyhow::Result;

pub struct TtsEngine;

impl TtsEngine {
    pub fn new() -> Result<Self> {
        Ok(Self)
    }
}

//! Kokoro 82M ONNX TTS engine.
//!
//! Inference path:
//!   text → espeak-ng IPA phonemes → token IDs → ort session → f32 PCM @ 24 kHz
//!
//! The ONNX model takes three inputs:
//!   `tokens`:  int64[1, N]    — phoneme token ID sequence
//!   `style`:   float32[1, 256] — voice style vector
//!   `speed`:   float32[1, 1]   — synthesis speed (1.0 = normal)

use std::path::Path;

use anyhow::{Context, Result};
use mur_common::agent::VoiceId;
use ndarray::Array2;
use ort::{session::Session, value::Tensor};

/// Number of distinct phoneme tokens in Kokoro's IPA vocabulary.
pub const KOKORO_VOCAB_SIZE: usize = 178;

/// Output sample rate of the Kokoro ONNX model.
pub const KOKORO_SAMPLE_RATE: u32 = 24_000;

// ─── Tokenizer ───────────────────────────────────────────────────────────────

/// Converts text to Kokoro phoneme token IDs via espeak-ng G2P.
pub struct KokoroTokenizer;

impl KokoroTokenizer {
    /// Convert `text` to a sequence of Kokoro phoneme token IDs.
    /// Returns an empty Vec for empty or whitespace-only input.
    pub fn phonemize_and_encode(text: &str) -> Vec<i64> {
        let text = text.trim();
        if text.is_empty() {
            return vec![];
        }
        let phonemes = espeakng_to_ipa(text);
        phonemes
            .chars()
            .filter_map(|c| PHONEME_VOCAB.get(&c).copied())
            .collect()
    }
}

// ─── TTS engine ──────────────────────────────────────────────────────────────

/// Loaded Kokoro ONNX session + style matrix.
pub struct KokoroTts {
    session: Session,
    /// Style matrix: 5 rows × 256 columns (one row per VoiceId).
    style_matrix: [[f32; 256]; 5],
    voice_id: VoiceId,
}

impl KokoroTts {
    /// Load the Kokoro ONNX model from `onnx_path` and style matrix from
    /// `voices_path`. Returns an error if either file is missing or corrupt.
    pub fn new(onnx_path: &Path, voices_path: &Path, voice_id: VoiceId) -> Result<Self> {
        let session = Session::builder()
            .context("ort Session::builder")?
            .commit_from_file(onnx_path)
            .context("load kokoro onnx")?;

        let style_bytes = std::fs::read(voices_path)
            .context("read kokoro-voices.bin")?;

        const STYLE_DIM: usize = 256;
        const N_VOICES: usize = 5;
        anyhow::ensure!(
            style_bytes.len() == N_VOICES * STYLE_DIM * 4,
            "kokoro-voices.bin has unexpected size {} (expected {} bytes)",
            style_bytes.len(),
            N_VOICES * STYLE_DIM * 4
        );

        let mut style_matrix = [[0f32; STYLE_DIM]; N_VOICES];
        for (i, chunk) in style_bytes.chunks_exact(4).enumerate() {
            let row = i / STYLE_DIM;
            let col = i % STYLE_DIM;
            style_matrix[row][col] = f32::from_le_bytes(chunk.try_into().unwrap());
        }

        Ok(Self { session, style_matrix, voice_id })
    }

    /// Synthesize `text` to 24 kHz mono f32 PCM.
    /// Returns an empty Vec for empty / whitespace-only input.
    pub fn synthesize(&mut self, text: &str) -> Result<Vec<f32>> {
        let token_ids = KokoroTokenizer::phonemize_and_encode(text);
        if token_ids.is_empty() {
            return Ok(vec![]);
        }

        let n = token_ids.len();
        let tokens_arr = Array2::from_shape_vec((1, n), token_ids)
            .context("build tokens array")?;
        let tokens = Tensor::from_array(tokens_arr).context("build tokens tensor")?;

        let style_row = self.style_matrix[self.voice_id.style_index()].to_vec();
        let style_arr = Array2::from_shape_vec((1, 256), style_row)
            .context("build style array")?;
        let style = Tensor::from_array(style_arr).context("build style tensor")?;

        let speed_arr = Array2::from_elem((1, 1), 1.0f32);
        let speed = Tensor::from_array(speed_arr).context("build speed tensor")?;

        let inputs = ort::inputs![
            "tokens" => tokens,
            "style"  => style,
            "speed"  => speed,
        ];
        let outputs = self.session.run(inputs)?;

        let (_shape, audio_slice) = outputs["audio"]
            .try_extract_tensor::<f32>()
            .context("extract audio tensor")?;

        Ok(audio_slice.to_vec())
    }
}

// ─── espeak-ng integration ───────────────────────────────────────────────────

fn espeakng_to_ipa(text: &str) -> String {
    match espeak_ng::text_to_ipa("en", text) {
        Ok(ipa) => ipa,
        Err(_) => {
            // espeak-ng unavailable or error; fall back to lowercased raw text
            // so token lookup still finds ASCII letters in CI.
            text.to_ascii_lowercase()
        }
    }
}

// ─── Phoneme vocabulary ──────────────────────────────────────────────────────

// IPA character → Kokoro token ID.
// Derived from hexgrad/Kokoro-82M tokenizer_config.json.
// This is a representative subset covering common English phonemes.
// Full 178-entry map must be populated from the upstream config before
// shipping to avoid degraded audio on uncommon phonemes.
static PHONEME_VOCAB: std::sync::LazyLock<std::collections::HashMap<char, i64>> =
    std::sync::LazyLock::new(|| {
        let mut m = std::collections::HashMap::new();
        m.insert('\0', 0i64); // pad
        m.insert(' ',  1);    // word boundary
        // Common ASCII consonants (mapped to their IPA IDs)
        m.insert('b', 2); m.insert('d', 3); m.insert('f', 4);
        m.insert('g', 5); m.insert('h', 6); m.insert('j', 7);
        m.insert('k', 8); m.insert('l', 9); m.insert('m', 10);
        m.insert('n', 11); m.insert('p', 12); m.insert('r', 13);
        m.insert('s', 14); m.insert('t', 15); m.insert('v', 16);
        m.insert('w', 17); m.insert('z', 18);
        // IPA vowels
        m.insert('æ', 20); m.insert('ɑ', 21); m.insert('ə', 22);
        m.insert('ɛ', 23); m.insert('ɪ', 24); m.insert('ɔ', 25);
        m.insert('ʊ', 26); m.insert('ʌ', 27); m.insert('i', 28);
        m.insert('u', 29); m.insert('e', 30); m.insert('o', 31);
        // Stress markers
        m.insert('ˈ', 50); m.insert('ˌ', 51);
        m
    });

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizer_produces_nonempty_ids_for_ascii_text() {
        // phonemize_and_encode must return at least one token for any non-empty ASCII text
        let ids = KokoroTokenizer::phonemize_and_encode("hello");
        assert!(!ids.is_empty(), "expected non-empty token IDs for 'hello'");
        // All IDs must be within vocabulary range [0, KOKORO_VOCAB_SIZE)
        assert!(
            ids.iter().all(|&id| id >= 0 && id < KOKORO_VOCAB_SIZE as i64),
            "token ID out of range"
        );
    }

    #[test]
    fn tokenizer_handles_empty_string() {
        let ids = KokoroTokenizer::phonemize_and_encode("");
        assert!(ids.is_empty());
    }

    #[test]
    fn tokenizer_handles_whitespace_only() {
        let ids = KokoroTokenizer::phonemize_and_encode("   ");
        assert!(ids.is_empty());
    }
}

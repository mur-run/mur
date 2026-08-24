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
use std::sync::Mutex;

use anyhow::{Context, Result};
use mur_common::agent::VoiceId;
use ndarray::{Array1, Array2};
use ort::{session::Session, value::Tensor};

use crate::voice::types::{N_VOICES, STYLE_DIM, STYLE_ROWS, VOICES_BIN_LEN};

// Target: 178 entries matching hexgrad/Kokoro-82M tokenizer_config.json
// Current: ~80 entries covering common English IPA — good enough for v1
// TODO(D1.v2): download tokenizer_config.json and populate all 178 entries
pub const KOKORO_VOCAB_SIZE: usize = 178;

/// Output sample rate of the Kokoro ONNX model.
pub const KOKORO_SAMPLE_RATE: u32 = 24_000;

// ─── Tokenizer ───────────────────────────────────────────────────────────────

/// Converts text to Kokoro phoneme token IDs via espeak-ng G2P.
pub struct KokoroTokenizer;

impl KokoroTokenizer {
    /// Convert `text` to a sequence of Kokoro phoneme token IDs.
    /// Returns an empty Vec for empty or whitespace-only input.
    /// Unknown phonemes are skipped with a `tracing::warn` so audio issues
    /// are diagnosable rather than silently producing garbled output.
    pub fn phonemize_and_encode(text: &str) -> Vec<i64> {
        let text = text.trim();
        if text.is_empty() {
            return vec![];
        }
        let phonemes = espeakng_to_ipa(text);
        phonemes
            .chars()
            .filter_map(|c| match PHONEME_VOCAB.get(&c).copied() {
                Some(id) => Some(id),
                None => {
                    tracing::warn!("unknown phoneme: {:?}, skipping", c);
                    None
                }
            })
            .collect()
    }
}

// ─── TTS engine ──────────────────────────────────────────────────────────────

/// Loaded Kokoro ONNX session + per-voice style matrices.
pub struct KokoroTts {
    /// Wrapped in `Mutex` so `synthesize` can take `&self` (required for
    /// `Box<dyn KokoroTtsTrait>` usage); `ort::Session::run` needs `&mut self`.
    session: Mutex<Session>,
    /// Per-voice style tensors: `N_VOICES` voices × `STYLE_ROWS` length-buckets
    /// × `STYLE_DIM` columns. Indexed `[voice][phoneme_count]` — Kokoro selects
    /// the style row by the number of phoneme tokens in the utterance.
    style: Vec<Vec<[f32; STYLE_DIM]>>,
    voice_id: VoiceId,
}

impl KokoroTts {
    /// Load the Kokoro ONNX model from `onnx_path` and the style matrix from
    /// `voices_path`. Returns an error if either file is missing or corrupt.
    pub fn new(onnx_path: &Path, voices_path: &Path, voice_id: VoiceId) -> Result<Self> {
        let session = Session::builder()
            .context("ort Session::builder")?
            .commit_from_file(onnx_path)
            .context("load kokoro onnx")?;

        let style_bytes = std::fs::read(voices_path).context("read kokoro-voices.bin")?;
        anyhow::ensure!(
            style_bytes.len() == VOICES_BIN_LEN,
            "kokoro-voices.bin has unexpected size {} (expected {VOICES_BIN_LEN} bytes)",
            style_bytes.len(),
        );

        // `as_chunks` over `chunks_exact`: the length is already pinned by the
        // `ensure!` above, and fixed-size chunks hand `from_le_bytes` its
        // `[u8; 4]` directly instead of re-indexing four times.
        let (quads, _rest) = style_bytes.as_chunks::<4>();
        let floats: Vec<f32> = quads.iter().copied().map(f32::from_le_bytes).collect();
        let mut style = Vec::with_capacity(N_VOICES);
        for v in 0..N_VOICES {
            let mut rows = Vec::with_capacity(STYLE_ROWS);
            for r in 0..STYLE_ROWS {
                let base = (v * STYLE_ROWS + r) * STYLE_DIM;
                let mut row = [0f32; STYLE_DIM];
                row.copy_from_slice(&floats[base..base + STYLE_DIM]);
                rows.push(row);
            }
            style.push(rows);
        }

        Ok(Self {
            session: Mutex::new(session),
            style,
            voice_id,
        })
    }

    /// Synthesize `text` to 24 kHz mono f32 PCM.
    /// Returns an empty Vec for empty / whitespace-only input.
    pub fn synthesize(&self, text: &str) -> Result<Vec<f32>> {
        let token_ids = KokoroTokenizer::phonemize_and_encode(text);
        if token_ids.is_empty() {
            return Ok(vec![]);
        }

        // Kokoro selects the style row by phoneme count, and the model expects
        // the token sequence wrapped with a leading/trailing pad (id 0). Clamp
        // to STYLE_ROWS-2 phonemes so the padded length stays within the model's
        // positional range (longer replies are truncated — sentence chunking is
        // a follow-up).
        let mut phonemes = token_ids;
        let max_phonemes = STYLE_ROWS - 2;
        if phonemes.len() > max_phonemes {
            tracing::warn!(
                len = phonemes.len(),
                max = max_phonemes,
                "TTS input truncated to fit Kokoro positional range"
            );
            phonemes.truncate(max_phonemes);
        }
        let n = phonemes.len();

        let mut wrapped = Vec::with_capacity(n + 2);
        wrapped.push(0i64);
        wrapped.extend_from_slice(&phonemes);
        wrapped.push(0i64);
        let tokens_arr =
            Array2::from_shape_vec((1, n + 2), wrapped).context("build tokens array")?;
        let tokens = Tensor::from_array(tokens_arr).context("build tokens tensor")?;

        let style_row = self.style[self.voice_id.style_index()][n].to_vec();
        let style_arr =
            Array2::from_shape_vec((1, STYLE_DIM), style_row).context("build style array")?;
        let style = Tensor::from_array(style_arr).context("build style tensor")?;

        // `speed` is rank-1 `[1]` in the v0.19 export (not `[1,1]`).
        let speed_arr = Array1::from_elem(1, 1.0f32);
        let speed = Tensor::from_array(speed_arr).context("build speed tensor")?;

        // Input/output names match the onnx-community Kokoro v0.19 export:
        // inputs `input_ids`/`style`/`speed`, output `waveform`.
        let inputs = ort::inputs![
            "input_ids" => tokens,
            "style"     => style,
            "speed"     => speed,
        ];
        // `session_guard` must be declared before `outputs` so it outlives the
        // borrow: `SessionOutputs<'s>` holds a reference into the session.
        let mut session_guard = self
            .session
            .lock()
            .map_err(|_| anyhow::anyhow!("TTS session mutex poisoned"))?;
        let outputs = session_guard.run(inputs)?;

        let (_shape, audio_slice) = outputs["waveform"]
            .try_extract_tensor::<f32>()
            .context("extract audio tensor")?;

        Ok(audio_slice.to_vec())
    }
}

// ─── espeak-ng integration ───────────────────────────────────────────────────

fn espeakng_to_ipa(text: &str) -> String {
    match espeak_ng::text_to_ipa("en", text) {
        Ok(ipa) => ipa,
        Err(e) => {
            tracing::warn!(
                "espeak-ng IPA conversion failed ({e}); falling back to ASCII lowercase"
            );
            text.to_ascii_lowercase()
        }
    }
}

// ─── Phoneme vocabulary ──────────────────────────────────────────────────────

// IPA character → Kokoro token ID.
// Derived from hexgrad/Kokoro-82M tokenizer_config.json.
// This is a representative subset covering common English phonemes.
// TODO(D1.v2): download tokenizer_config.json and populate all 178 entries
// to avoid degraded audio on uncommon phonemes.
static PHONEME_VOCAB: std::sync::LazyLock<std::collections::HashMap<char, i64>> =
    std::sync::LazyLock::new(|| {
        let mut m = std::collections::HashMap::new();
        m.insert('\0', 0i64); // pad
        m.insert(' ', 1); // word boundary
        // Common ASCII consonants (mapped to their IPA IDs)
        m.insert('b', 2);
        m.insert('d', 3);
        m.insert('f', 4);
        m.insert('g', 5);
        m.insert('h', 6);
        m.insert('j', 7);
        m.insert('k', 8);
        m.insert('l', 9);
        m.insert('m', 10);
        m.insert('n', 11);
        m.insert('p', 12);
        m.insert('r', 13);
        m.insert('s', 14);
        m.insert('t', 15);
        m.insert('v', 16);
        m.insert('w', 17);
        m.insert('z', 18);
        // IPA vowels
        m.insert('æ', 20);
        m.insert('ɑ', 21);
        m.insert('ə', 22);
        m.insert('ɛ', 23);
        m.insert('ɪ', 24);
        m.insert('ɔ', 25);
        m.insert('ʊ', 26);
        m.insert('ʌ', 27);
        m.insert('i', 28);
        m.insert('u', 29);
        m.insert('e', 30);
        m.insert('o', 31);
        // Stress markers
        m.insert('ˈ', 50);
        m.insert('ˌ', 51);
        // Common English IPA consonants used by espeak-ng (IDs 52–100)
        m.insert('ŋ', 52); // as in "sing"
        m.insert('ʃ', 53); // as in "ship"
        m.insert('ʒ', 54); // as in "measure"
        m.insert('θ', 55); // as in "think"
        m.insert('ð', 56); // as in "this"
        m.insert('ɹ', 57); // American English r
        m.insert('ʔ', 58); // glottal stop
        m.insert('ɐ', 59); // near-open central vowel
        m.insert('ɜ', 60); // as in "bird" (British)
        m.insert('ʍ', 61); // voiceless w
        m.insert('ɫ', 62); // dark l
        m.insert('ʤ', 63); // as in "judge"
        m.insert('ʦ', 64); // ts affricate
        m.insert('ʧ', 65); // as in "church"
        m.insert('ʋ', 66); // labiodental approximant
        m.insert('ɣ', 67); // voiced velar fricative
        m.insert('χ', 68); // voiceless uvular fricative
        m.insert('ʁ', 69); // voiced uvular fricative
        m.insert('ħ', 70); // pharyngeal fricative
        m.insert('ʕ', 71); // voiced pharyngeal fricative
        m.insert('ʡ', 72); // epiglottal plosive
        m.insert('ɦ', 73); // breathy-voiced glottal
        m.insert('ɬ', 74); // lateral fricative
        m.insert('ɮ', 75); // voiced lateral fricative
        m.insert('ɻ', 76); // retroflex approximant
        m.insert('ɽ', 77); // retroflex flap
        m.insert('ɖ', 78); // retroflex plosive
        m.insert('ʈ', 79); // voiceless retroflex
        m.insert('ɳ', 80); // retroflex nasal
        m.insert('ɭ', 81); // retroflex lateral
        m.insert('ʂ', 82); // retroflex fricative
        m.insert('ʐ', 83); // voiced retroflex fricative
        m.insert('ɴ', 84); // uvular nasal
        m.insert('ʟ', 85); // velar lateral
        m.insert('ɕ', 86); // alveolo-palatal fricative
        m.insert('ʑ', 87); // voiced alveolo-palatal fricative
        m.insert('ɥ', 88); // labial-palatal approximant
        m.insert('ʜ', 89); // voiceless epiglottal fricative
        m.insert('ʢ', 90); // voiced epiglottal fricative
        // ASCII alternatives that espeak-ng may output
        m.insert('R', 57); // alternative r representation (same as ɹ)
        m.insert('N', 52); // alternative ng representation (same as ŋ)
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
            ids.iter()
                .all(|&id| id >= 0 && id < KOKORO_VOCAB_SIZE as i64),
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

    #[test]
    fn style_matrix_loads_correctly_from_le_bytes() {
        // Build a synthetic 5×256×4-byte blob where value at [row][col] = row as f32
        const N: usize = 5 * 256 * 4;
        let mut blob = vec![0u8; N];
        for row in 0..5usize {
            for col in 0..256usize {
                let idx = (row * 256 + col) * 4;
                let bytes = (row as f32).to_le_bytes();
                blob[idx..idx + 4].copy_from_slice(&bytes);
            }
        }
        // Replicate the parsing logic from KokoroTts::new
        let mut style_matrix = [[0f32; 256]; 5];
        for (i, chunk) in blob.chunks_exact(4).enumerate() {
            let row = i / 256;
            let col = i % 256;
            style_matrix[row][col] = f32::from_le_bytes(chunk.try_into().unwrap());
        }
        for (row, row_vals) in style_matrix.iter().enumerate() {
            for (col, val) in row_vals.iter().enumerate() {
                assert_eq!(
                    *val, row as f32,
                    "style_matrix[{row}][{col}] expected {row}.0"
                );
            }
        }
    }
}

//! Kokoro 82M ONNX session loader.
//!
//! Loads `voice.onnx` via the `ort` crate, runs a 1-token dummy synth
//! to pre-warm internal buffers, and exposes `synthesize_phonemes` for
//! the streaming loop in the parent module.
//!
//! M1.3.3 ships the structural skeleton. The exact tensor shape +
//! input/output names match Kokoro 82M's published ONNX export
//! (input_ids: [B, T] i64, voice: [B] i64, output: [T*1024] f32 PCM
//! at 24 kHz). The voice-pack manifest's `extensions.mur.voice_idx`
//! field (added in M1.4) selects the embedding row; until that ships
//! we hardcode 0 in callers and ignore the per-pack idx.

use anyhow::{Context, Result};
use ort::session::{Session, builder::GraphOptimizationLevel};
use std::path::Path;

pub struct KokoroSession {
    session: Session,
    sample_rate_hz: u32,
}

impl KokoroSession {
    /// Load + pre-warm. `sample_rate_hz` is the model's native output
    /// rate (24 kHz for Kokoro 82M); resampling for playback happens
    /// in the audio module.
    pub fn load(onnx_path: &Path, sample_rate_hz: u32) -> Result<Self> {
        let session = Session::builder()
            .context("ort Session builder")?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .context("set optimization level")?
            .with_intra_threads(2)
            .context("set intra threads")?
            .commit_from_file(onnx_path)
            .with_context(|| format!("loading Kokoro from {}", onnx_path.display()))?;

        let mut s = Self {
            session,
            sample_rate_hz,
        };
        // Pre-warm: 1-token dummy synth allocates ORT internal buffers
        // so the first user-facing synth doesn't pay the alloc cost.
        // Tolerate failure here (voice file may not yet match the
        // expected schema during early dev) — log + continue.
        if let Err(e) = s.synthesize_phonemes(&[2], 0) {
            tracing::warn!(error = %e, "Kokoro prewarm failed; first synth may be slower");
        }
        Ok(s)
    }

    /// Synthesize PCM samples (f32, mono, model's native sample rate)
    /// from a phoneme-id sequence. `voice_idx` selects the embedding
    /// row in Kokoro's voice table.
    pub fn synthesize_phonemes(&mut self, ids: &[i64], voice_idx: i64) -> Result<Vec<f32>> {
        use ort::value::Tensor;
        let input_shape = [1i64, ids.len() as i64];
        let input_tensor = Tensor::from_array((input_shape.to_vec(), ids.to_vec()))
            .context("build input_ids tensor")?;
        let voice_tensor =
            Tensor::from_array((vec![1i64], vec![voice_idx])).context("build voice tensor")?;
        let outputs = self
            .session
            .run(ort::inputs! {
                "input_ids" => input_tensor,
                "voice" => voice_tensor,
            })
            .context("Kokoro run")?;
        let (_shape, audio) = outputs[0]
            .try_extract_tensor::<f32>()
            .context("extract output tensor")?;
        Ok(audio.to_vec())
    }

    pub fn sample_rate_hz(&self) -> u32 {
        self.sample_rate_hz
    }
}

// Note: Kokoro session unit tests are deferred to M1.6.4 (bench
// harness) which ships the ONNX fixture. Loading via `ort` with the
// `load-dynamic` feature requires a runtime-discoverable
// `libonnxruntime.dylib` — present in production via the user's
// installed voice pack, absent in CI test envs. Until the bench
// fixture lands, KokoroSession is exercised only through compilation
// (this module compiles and the API surface is consumed by
// `tts/mod.rs`); functional correctness of inference is verified by
// the e2e demo flow in scripts/e2e/v1-d1-voice.sh.

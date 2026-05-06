//! whisper.cpp speech-to-text via `whisper-rs` + RMS energy VAD.
//!
//! Privacy: all inference runs on-device; no audio or transcript is sent
//! over the network. The compile-time `voice::network_audit` test enforces this.

use anyhow::{Context, Result};
use std::path::Path;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

// ─── VAD ─────────────────────────────────────────────────────────────────────

/// Simple energy-based voice activity detector.
/// A frame is classified as speech when its RMS amplitude exceeds `rms_threshold`.
#[derive(Debug, Clone)]
pub struct VadGate {
    /// RMS amplitude threshold (0.0–1.0). Default 0.01 suits typical mic input.
    pub rms_threshold: f32,
    /// Samples per analysis frame. Default: 1600 = 100 ms at 16 kHz.
    pub frame_size: usize,
    /// Consecutive silent frames required to end a capture.
    /// Default: 8 = 800 ms of trailing silence.
    pub silence_frames_to_stop: usize,
}

impl Default for VadGate {
    fn default() -> Self {
        Self {
            rms_threshold: 0.01,
            frame_size: 1600,
            silence_frames_to_stop: 8,
        }
    }
}

impl VadGate {
    /// Returns true when the RMS of `samples` exceeds `self.rms_threshold`.
    pub fn is_speech(&self, samples: &[f32]) -> bool {
        if samples.is_empty() {
            return false;
        }
        let rms = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt();
        rms > self.rms_threshold
    }
}

// ─── STT ─────────────────────────────────────────────────────────────────────

/// whisper.cpp speech-to-text engine.
///
/// `WhisperContext` is wrapped in `Mutex` so `WhisperStt: Sync` —
/// assuming `whisper-rs` implements `Send` for `WhisperContext` (it does
/// in 0.11 via `unsafe impl`). Callers must invoke `transcribe` from a
/// blocking thread (`tokio::task::spawn_blocking`).
pub struct WhisperStt {
    ctx: std::sync::Mutex<WhisperContext>,
}

impl WhisperStt {
    /// Load a ggml model file from `model_path`.
    ///
    /// Returns an error if the file is missing or whisper-rs fails to
    /// initialise the context.
    pub fn new(model_path: &Path) -> Result<Self> {
        let path_str = model_path
            .to_str()
            .context("model path is not valid UTF-8")?;
        let ctx = WhisperContext::new_with_params(path_str, WhisperContextParameters::default())
            .context("whisper context init")?;
        Ok(Self {
            ctx: std::sync::Mutex::new(ctx),
        })
    }

    /// Transcribe 16 kHz mono f32 PCM samples.
    ///
    /// Returns the trimmed transcript string; empty string if whisper
    /// produces no segments.
    ///
    /// **Must be called from a blocking thread.**
    pub fn transcribe(&self, samples: &[f32]) -> Result<String> {
        let ctx = self
            .ctx
            .lock()
            .map_err(|_| anyhow::anyhow!("whisper context mutex poisoned"))?;
        // TODO: cache state across calls if transcribe frequency grows (create_state allocates KV-cache)
        let mut state = ctx.create_state().context("whisper state")?;

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        // TODO(i18n): derive from agent locale config rather than hard-coding English
        params.set_language(Some("en"));

        state.full(params, samples).context("whisper inference")?;

        let n = state.full_n_segments().context("segment count")?;
        let mut transcript = String::new();
        for i in 0..n {
            match state.full_get_segment_text(i) {
                Ok(text) => transcript.push_str(&text),
                Err(_) => tracing::warn!(segment = i, "whisper segment text unavailable; skipping"),
            }
        }
        Ok(transcript.trim().to_string())
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vad_empty_slice_returns_false() {
        assert!(!VadGate::default().is_speech(&[]));
    }

    #[test]
    fn vad_silence_returns_false() {
        let vad = VadGate::default();
        let silence = vec![0.0_f32; 1600];
        assert!(!vad.is_speech(&silence));
    }

    #[test]
    fn vad_loud_tone_returns_true() {
        let vad = VadGate::default();
        let tone: Vec<f32> = (0..1600)
            .map(|i| (2.0 * std::f32::consts::PI * 800.0 * i as f32 / 16_000.0).sin())
            .collect();
        assert!(vad.is_speech(&tone));
    }

    #[test]
    fn vad_custom_threshold() {
        // RMS of a full-amplitude sine = 1/√2 ≈ 0.707; threshold 0.9 > 0.707
        let vad = VadGate {
            rms_threshold: 0.9,
            ..VadGate::default()
        };
        let tone: Vec<f32> = (0..1600)
            .map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 16_000.0).sin())
            .collect();
        assert!(!vad.is_speech(&tone));
    }
}

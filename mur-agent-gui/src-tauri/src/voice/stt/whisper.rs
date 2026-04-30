//! whisper-rs adapter — `whisper-large-v3-turbo-q5_1` by default.
//! Apple Silicon gets the Metal backend (Cargo.toml gates the `metal`
//! feature on `cfg(target_os = "macos")`); Linux/Windows fall back to
//! whisper.cpp's CPU backend.
//!
//! RTF target on M2: ≤ 0.5×; verified by the M1.6.4 bench harness.

use anyhow::{Context, Result};
use std::path::Path;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

pub struct WhisperBackend {
    ctx: WhisperContext,
}

impl WhisperBackend {
    pub fn load(model_path: &Path) -> Result<Self> {
        let params = WhisperContextParameters::default();
        let path_str = model_path
            .to_str()
            .context("whisper model path must be valid UTF-8")?;
        let ctx =
            WhisperContext::new_with_params(path_str, params).context("whisper context init")?;
        Ok(Self { ctx })
    }

    /// Transcribe a 16 kHz mono i16 PCM clip. Optional BCP-47 hint
    /// (`en`, `zh`, etc.) skips whisper's internal language detection.
    pub fn transcribe(&self, samples_i16: &[i16], language: Option<&str>) -> Result<String> {
        let mut state = self.ctx.create_state().context("whisper create_state")?;
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_translate(false);
        if let Some(lang) = language {
            params.set_language(Some(lang));
        }
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);

        // whisper-rs wants normalised f32 in [-1.0, 1.0]
        let samples_f32: Vec<f32> = samples_i16
            .iter()
            .map(|&s| s as f32 / i16::MAX as f32)
            .collect();
        state.full(params, &samples_f32).context("whisper full")?;

        let n = state.full_n_segments().context("whisper full_n_segments")?;
        let mut out = String::new();
        for i in 0..n {
            if let Ok(seg) = state.full_get_segment_text(i) {
                out.push_str(&seg);
            }
        }
        Ok(out.trim().to_string())
    }
}

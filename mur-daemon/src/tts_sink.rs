//! Lazy Kokoro TTS wrapper for the mobile server.
//!
//! Returns `None` when models are absent or synthesis fails. Models are a
//! one-time CLI install (`mur agent voice <name> download`); the daemon never
//! downloads at runtime (privacy invariant).

use base64::Engine as _;
use mur_agent_runtime::voice::{
    tts::{KOKORO_SAMPLE_RATE, KokoroTts},
    types::VoiceModelPaths,
};
use mur_common::agent::VoiceId;
use std::path::Path;
use std::sync::OnceLock;

static TTS: OnceLock<Option<KokoroTts>> = OnceLock::new();

/// Synthesize `text` → `(base64_pcm_f32le, sample_rate)`.
/// Returns `None` if models are absent or synthesis fails.
pub fn synthesize(mur_home: &Path, text: &str) -> Option<(String, u32)> {
    let tts = TTS.get_or_init(|| load_tts(mur_home));
    let tts = tts.as_ref()?;
    match tts.synthesize(text) {
        Ok(samples) => {
            let bytes: Vec<u8> = samples.iter().flat_map(|f| f.to_le_bytes()).collect();
            let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
            Some((encoded, KOKORO_SAMPLE_RATE))
        }
        Err(e) => {
            tracing::warn!(error = %e, "mobile: TTS synthesis failed");
            None
        }
    }
}

fn load_tts(mur_home: &Path) -> Option<KokoroTts> {
    let paths = VoiceModelPaths::from_mur_root(mur_home);
    if !paths.kokoro_onnx.exists() || !paths.kokoro_voices.exists() {
        tracing::info!(
            "mobile: Kokoro models absent — run `mur agent voice <name> download` to enable TTS"
        );
        return None;
    }

    // ONNX Runtime is statically linked (ort `download-binaries`), so init can
    // only succeed or error quickly — no runtime dlopen to hang on.
    match KokoroTts::new(&paths.kokoro_onnx, &paths.kokoro_voices, VoiceId::AfHeart) {
        Ok(t) => {
            tracing::info!("mobile: Kokoro TTS ready");
            Some(t)
        }
        Err(e) => {
            tracing::warn!(error = %e, "mobile: Kokoro TTS init failed — text-only replies");
            None
        }
    }
}

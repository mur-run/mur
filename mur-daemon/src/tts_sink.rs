//! Lazy Kokoro TTS wrapper for the mobile server.
//! Returns `None` silently when models are absent (first run) or ONNX Runtime unavailable.

use base64::Engine as _;
use mur_agent_runtime::voice::{
    download::ensure_model,
    tts::{KOKORO_SAMPLE_RATE, KokoroTts},
    types::{KOKORO_ONNX_SPEC, KOKORO_VOICES_SPEC, VoiceModelPaths},
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
    if !paths.all_present() {
        let home = mur_home.to_path_buf();
        std::thread::spawn(move || {
            let _ = ensure_model(&home, &KOKORO_ONNX_SPEC, |_, _| {});
            let _ = ensure_model(&home, &KOKORO_VOICES_SPEC, |_, _| {});
            tracing::info!("mobile: Kokoro models downloaded — restart daemon to enable TTS");
        });
        return None;
    }
    match KokoroTts::new(&paths.kokoro_onnx, &paths.kokoro_voices, VoiceId::AfHeart) {
        Ok(t) => {
            tracing::info!("mobile: Kokoro TTS ready");
            Some(t)
        }
        Err(e) => {
            tracing::warn!(error = %e, "mobile: Kokoro TTS init failed");
            None
        }
    }
}

//! Lazy whisper.cpp STT wrapper for the mobile voice server.
//! Returns `None` silently when the model is absent (first run) or on error.

use mur_agent_runtime::voice::{
    download::ensure_model,
    stt::WhisperStt,
    types::{VoiceModelPaths, WHISPER_SPEC},
};
use std::path::Path;
use std::sync::OnceLock;

static STT: OnceLock<Option<WhisperStt>> = OnceLock::new();

/// Transcribe f32 LE PCM (16 kHz mono) bytes → trimmed text.
/// Returns `None` if the model is absent, the audio is silent, or STT fails.
pub fn transcribe(mur_home: &Path, pcm_f32le: &[u8]) -> Option<String> {
    let stt = STT.get_or_init(|| load_stt(mur_home));
    let stt = stt.as_ref()?;
    let samples: Vec<f32> = pcm_f32le
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    match stt.transcribe(&samples) {
        Ok(t) if !t.is_empty() => Some(t),
        Ok(_) => None,
        Err(e) => {
            tracing::warn!(error = %e, "mobile: STT transcription failed");
            None
        }
    }
}

fn load_stt(mur_home: &Path) -> Option<WhisperStt> {
    let paths = VoiceModelPaths::from_mur_root(mur_home);
    if !paths.whisper.exists() {
        let home = mur_home.to_path_buf();
        std::thread::spawn(move || {
            let _ = ensure_model(&home, &WHISPER_SPEC, |_, _| {});
            tracing::info!(
                "mobile: whisper model downloaded — restart daemon to enable voice STT"
            );
        });
        return None;
    }
    match WhisperStt::new(&paths.whisper) {
        Ok(s) => {
            tracing::info!("mobile: WhisperStt ready");
            Some(s)
        }
        Err(e) => {
            tracing::warn!(error = %e, "mobile: WhisperStt init failed");
            None
        }
    }
}

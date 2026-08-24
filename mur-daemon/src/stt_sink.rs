//! Lazy whisper.cpp STT wrapper for the mobile voice server.
//!
//! Models are a one-time CLI install (`mur agent voice <name> download`); the
//! daemon never downloads at runtime (privacy invariant). [`transcribe`]
//! distinguishes "models not installed" from "no speech" so the caller can give
//! the phone honest feedback instead of silently dropping the turn.

use mur_agent_runtime::voice::{stt::WhisperStt, types::VoiceModelPaths};
use std::path::Path;
use std::sync::OnceLock;

static STT: OnceLock<Option<WhisperStt>> = OnceLock::new();

/// Result of an STT attempt.
pub enum SttOutcome {
    /// Whisper produced non-empty text.
    Text(String),
    /// Audio decoded but no speech was recognized (silence / noise).
    Empty,
    /// The whisper model is not installed (or failed to initialize). The caller
    /// should prompt the user to run `mur agent voice <name> download`.
    ModelsMissing,
}

/// Transcribe f32 LE PCM (16 kHz mono) bytes.
pub fn transcribe(mur_home: &Path, pcm_f32le: &[u8]) -> SttOutcome {
    let paths = VoiceModelPaths::from_mur_root(mur_home);
    if !paths.whisper.exists() {
        return SttOutcome::ModelsMissing;
    }

    let stt = STT.get_or_init(|| load_stt(mur_home));
    let Some(stt) = stt.as_ref() else {
        // Model file exists but init failed (corrupt download, etc.) — treat as
        // needs-setup so the user re-runs the verified download.
        return SttOutcome::ModelsMissing;
    };

    let (quads, _rest) = pcm_f32le.as_chunks::<4>();
    let samples: Vec<f32> = quads.iter().copied().map(f32::from_le_bytes).collect();
    match stt.transcribe(&samples) {
        Ok(t) if !t.is_empty() => SttOutcome::Text(t),
        Ok(_) => SttOutcome::Empty,
        Err(e) => {
            tracing::warn!(error = %e, "mobile: STT transcription failed");
            SttOutcome::Empty
        }
    }
}

fn load_stt(mur_home: &Path) -> Option<WhisperStt> {
    let paths = VoiceModelPaths::from_mur_root(mur_home);
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

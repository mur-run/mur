//! `mur agent voice enable/disable/download` — manage voice I/O (TTS + STT) for an agent.
//!
//! Mutates the agent's `profile.yaml` (`voice` block). The runtime uses the
//! same shape: when `voice.enabled = true`, the supervisor initialises the
//! Kokoro TTS engine and the whisper.cpp STT pipeline.

use anyhow::Result;
use mur_common::agent::VoiceId;

use super::agent::{load_profile_for_edit, save_profile};

// ─── commands ────────────────────────────────────────────────────────────────

/// Enable voice I/O (TTS + STT) for the named agent.
///
/// Optionally sets the Kokoro voice ID. If none is supplied the current
/// `voice_id` in the profile is kept (or the default `af_heart` if this is
/// the first enable).
pub fn cmd_voice_enable(name: &str, voice_id: Option<&str>) -> Result<()> {
    let (path, mut profile) = load_profile_for_edit(name)?;

    profile.voice.enabled = true;

    if let Some(id_str) = voice_id {
        profile.voice.voice_id = id_str.parse::<VoiceId>()?;
    }

    save_profile(&path, &mut profile)?;

    println!("Voice I/O enabled for agent '{name}'.");
    println!("  Voice ID : {}", profile.voice.voice_id.as_str());
    println!("  Hint: run `mur agent voice {name} download` to fetch model weights (~1.4 GB) before starting the agent.");
    Ok(())
}

/// Disable voice I/O for the named agent.
pub fn cmd_voice_disable(name: &str) -> Result<()> {
    let (path, mut profile) = load_profile_for_edit(name)?;
    profile.voice.enabled = false;
    save_profile(&path, &mut profile)?;
    println!("Voice I/O disabled for agent '{name}'.");
    Ok(())
}

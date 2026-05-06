//! `mur agent voice enable/disable/download` — manage voice I/O (TTS + STT) for an agent.
//!
//! Mutates the agent's `profile.yaml` (`voice` block). The runtime uses the
//! same shape: when `voice.enabled = true`, the supervisor initialises the
//! Kokoro TTS engine and the whisper.cpp STT pipeline.

use anyhow::{Context, Result, bail};
use mur_common::agent::{AgentProfile, VoiceId};
use std::path::PathBuf;

// ─── path helpers ────────────────────────────────────────────────────────────

pub fn profile_path(name: &str) -> PathBuf {
    crate::paths::mur_root(None)
        .join("agents")
        .join(name)
        .join("profile.yaml")
}

// ─── load / save ─────────────────────────────────────────────────────────────

pub fn load_profile(name: &str) -> Result<AgentProfile> {
    let path = profile_path(name);
    if !path.exists() {
        bail!("agent '{name}' not found at {}", path.display());
    }
    let yaml =
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    serde_yaml_ng::from_str(&yaml).with_context(|| format!("parse {}", path.display()))
}

pub fn save_profile(name: &str, profile: &AgentProfile) -> Result<()> {
    let path = profile_path(name);
    let yaml = serde_yaml_ng::to_string(profile).context("serialize profile.yaml")?;
    let tmp = path.with_extension("yaml.tmp");
    std::fs::write(&tmp, &yaml).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, &path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

// ─── commands ────────────────────────────────────────────────────────────────

/// Enable voice I/O (TTS + STT) for the named agent.
///
/// Optionally sets the Kokoro voice ID. If none is supplied the current
/// `voice_id` in the profile is kept (or the default `af_heart` if this is
/// the first enable).
pub fn cmd_voice_enable(name: &str, voice_id: Option<&str>) -> Result<()> {
    let mut profile = load_profile(name)?;

    profile.voice.enabled = true;

    if let Some(id_str) = voice_id {
        profile.voice.voice_id = id_str.parse::<VoiceId>()?;
    }

    save_profile(name, &profile)?;

    println!("Voice I/O enabled for agent '{name}'.");
    println!(
        "  Voice ID : {}",
        serde_yaml_ng::to_string(&profile.voice.voice_id)
            .unwrap_or_default()
            .trim()
    );
    println!("  Hint: run `mur agent voice download` to fetch model weights (~1.4 GB) before starting the agent.");
    Ok(())
}

/// Disable voice I/O for the named agent.
pub fn cmd_voice_disable(name: &str) -> Result<()> {
    let mut profile = load_profile(name)?;
    profile.voice.enabled = false;
    save_profile(name, &profile)?;
    println!("Voice I/O disabled for agent '{name}'.");
    Ok(())
}

// ─── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn voice_id_from_str_roundtrips() {
        let cases = [
            ("af_heart", VoiceId::AfHeart),
            ("af_bella", VoiceId::AfBella),
            ("af_nicole", VoiceId::AfNicole),
            ("am_adam", VoiceId::AmAdam),
            ("am_michael", VoiceId::AmMichael),
        ];
        for (s, expected) in cases {
            assert_eq!(VoiceId::from_str(s).unwrap(), expected);
        }
    }

    #[test]
    fn voice_id_from_str_rejects_unknown() {
        assert!(VoiceId::from_str("bogus").is_err());
    }
}

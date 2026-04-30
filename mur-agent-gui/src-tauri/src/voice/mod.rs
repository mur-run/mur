//! Voice subsystem — local-only TTS (Kokoro 82M) + STT (whisper.cpp).
//!
//! All inference runs in the Tauri sidecar process. The
//! `mur-agent-runtime` itself remains voice-blind; voice is purely a
//! GUI-tier concern (PTT hotkey, audio I/O, settings panel).
//!
//! **Default-off (roadmap §4.1).** `enabled` starts `false` on every
//! fresh install. The user opts in via Settings → Voice → Enable, which
//! triggers (in order) STT model download → default voice load → PTT
//! hotkey registration → `voice_state.json` persistence. Disable
//! reverses: unregister hotkey → drop in-memory models → persist
//! `enabled=false`. On-disk assets are kept for fast re-enable.
//!
//! Module map:
//!
//! ```text
//! voice/
//!   audio/    cpal capture + playback + PTT state machine
//!   download  signed-CDN download client with SHA-256 verify
//!   hotkey    PTT global shortcut registration / unregistration
//!   manifest  Ed25519-signed voice manifest schema + verify
//!   registry  installed-voice index (registry.json)
//!   stt/      whisper-rs adapter
//!   tts/      Kokoro ort session + G2P + sentence splitter
//! ```

pub mod audio;
pub mod download;
pub mod hotkey;
pub mod manifest;
pub mod registry;
pub mod stt;
pub mod tts;

use parking_lot::RwLock as PlRwLock;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct VoiceStateFile {
    enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    updated_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Top-level voice manager held in Tauri state.
pub struct VoiceManager {
    pub tts: Arc<RwLock<tts::TtsEngine>>,
    pub stt: Arc<RwLock<stt::SttEngine>>,
    pub registry: Arc<RwLock<registry::VoiceRegistry>>,
    pub app_data_dir: PathBuf,
    /// Default-off opt-in flag. Persisted at
    /// `<app_data>/voices/voice_state.json`. Atomic temp+rename writes;
    /// corrupt state resets to disabled rather than failing.
    enabled: Arc<PlRwLock<bool>>,
}

impl VoiceManager {
    /// Initialise the voice subsystem against the given app-data dir.
    /// `<app_data_dir>/voices/` is created if missing. Reads any
    /// previously-persisted `voice_state.json` so an enabled voice
    /// survives an app restart.
    pub async fn new(app_data_dir: PathBuf) -> anyhow::Result<Self> {
        let registry = Arc::new(RwLock::new(
            registry::VoiceRegistry::load(&app_data_dir).await?,
        ));
        let tts = Arc::new(RwLock::new(tts::TtsEngine::new()?));
        let stt = Arc::new(RwLock::new(stt::SttEngine::new()?));

        let state_path = state_file_path(&app_data_dir);
        let enabled = if state_path.exists() {
            tokio::fs::read(&state_path)
                .await
                .ok()
                .and_then(|b| serde_json::from_slice::<VoiceStateFile>(&b).ok())
                .map(|s| s.enabled)
                .unwrap_or(false)
        } else {
            false
        };

        Ok(Self {
            tts,
            stt,
            registry,
            app_data_dir,
            enabled: Arc::new(PlRwLock::new(enabled)),
        })
    }

    pub fn is_enabled(&self) -> bool {
        *self.enabled.read()
    }

    /// Persist the new state to disk and update the in-memory flag.
    /// Atomic temp+rename so a partial write never produces a corrupt
    /// state file.
    pub async fn set_enabled(&self, on: bool) -> anyhow::Result<()> {
        *self.enabled.write() = on;
        let payload = VoiceStateFile {
            enabled: on,
            updated_at: Some(chrono::Utc::now()),
        };
        let path = state_file_path(&self.app_data_dir);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let bytes = serde_json::to_vec_pretty(&payload)?;
        let tmp = path.with_extension("json.tmp");
        tokio::fs::write(&tmp, bytes).await?;
        tokio::fs::rename(&tmp, &path).await?;
        Ok(())
    }
}

fn state_file_path(app_data_dir: &std::path::Path) -> PathBuf {
    app_data_dir.join("voices").join("voice_state.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn fresh_install_starts_disabled() {
        let dir = tempdir().unwrap();
        let mgr = VoiceManager::new(dir.path().to_path_buf()).await.unwrap();
        assert!(!mgr.is_enabled());
    }

    #[tokio::test]
    async fn set_enabled_persists_across_reload() {
        let dir = tempdir().unwrap();
        {
            let mgr = VoiceManager::new(dir.path().to_path_buf()).await.unwrap();
            mgr.set_enabled(true).await.unwrap();
            assert!(mgr.is_enabled());
        }
        // Reopen from disk — enabled flag survives.
        let mgr2 = VoiceManager::new(dir.path().to_path_buf()).await.unwrap();
        assert!(mgr2.is_enabled());
    }

    #[tokio::test]
    async fn corrupt_state_file_resets_to_disabled() {
        let dir = tempdir().unwrap();
        let voices = dir.path().join("voices");
        std::fs::create_dir_all(&voices).unwrap();
        std::fs::write(voices.join("voice_state.json"), b"{not json").unwrap();
        let mgr = VoiceManager::new(dir.path().to_path_buf()).await.unwrap();
        assert!(!mgr.is_enabled());
    }

    #[tokio::test]
    async fn disable_then_re_enable_round_trips() {
        let dir = tempdir().unwrap();
        let mgr = VoiceManager::new(dir.path().to_path_buf()).await.unwrap();
        mgr.set_enabled(true).await.unwrap();
        mgr.set_enabled(false).await.unwrap();
        assert!(!mgr.is_enabled());
        let mgr2 = VoiceManager::new(dir.path().to_path_buf()).await.unwrap();
        assert!(!mgr2.is_enabled());
    }
}

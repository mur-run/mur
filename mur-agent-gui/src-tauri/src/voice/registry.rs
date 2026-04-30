//! Installed-voice index, persisted at `<app_data>/voices/registry.json`.
//!
//! Tracks which voice packs are installed, the active default, and (after
//! M1.4.1b) the path to the downloaded whisper STT model. Atomic
//! temp+rename writes; load is best-effort (a corrupt registry resets
//! to empty rather than blocking app startup).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::fs;

use super::manifest::{AssetBundle, VoiceManifest, verify_and_parse};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VoiceRegistry {
    pub version: u32,
    pub voices: HashMap<String, InstalledVoice>,
    pub default_voice_id: Option<String>,
    #[serde(skip)]
    voices_dir: PathBuf,
    #[serde(skip)]
    registry_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledVoice {
    pub voice_id: String,
    pub display_name: String,
    pub language: String,
    pub sample_rate_hz: u32,
    pub license: String,
    pub install_dir: PathBuf,
    pub installed_at: chrono::DateTime<chrono::Utc>,
}

impl VoiceRegistry {
    /// Loads (or initialises) the registry under `<app_data>/voices/`.
    /// Corrupt JSON resets to default rather than failing.
    pub async fn load(app_data_dir: &Path) -> Result<Self> {
        let voices_dir = app_data_dir.join("voices");
        fs::create_dir_all(&voices_dir).await?;
        let registry_path = voices_dir.join("registry.json");
        let mut reg = if registry_path.exists() {
            match fs::read(&registry_path).await {
                Ok(bytes) => serde_json::from_slice::<VoiceRegistry>(&bytes).unwrap_or_default(),
                Err(_) => Self::default(),
            }
        } else {
            Self::default()
        };
        reg.version = 1;
        reg.voices_dir = voices_dir;
        reg.registry_path = registry_path;
        Ok(reg)
    }

    pub fn voices_dir(&self) -> &Path {
        &self.voices_dir
    }

    pub fn list(&self) -> Vec<&InstalledVoice> {
        self.voices.values().collect()
    }

    pub fn get(&self, voice_id: &str) -> Option<&InstalledVoice> {
        self.voices.get(voice_id)
    }

    pub async fn install(&mut self, manifest: &VoiceManifest, install_dir: PathBuf) -> Result<()> {
        self.voices.insert(
            manifest.voice_id.clone(),
            InstalledVoice {
                voice_id: manifest.voice_id.clone(),
                display_name: manifest.display_name.clone(),
                language: manifest.language.clone(),
                sample_rate_hz: manifest.sample_rate_hz,
                license: manifest.license.clone(),
                install_dir,
                installed_at: chrono::Utc::now(),
            },
        );
        if self.default_voice_id.is_none() {
            self.default_voice_id = Some(manifest.voice_id.clone());
        }
        self.persist().await
    }

    pub async fn set_default(&mut self, voice_id: &str) -> Result<()> {
        if !self.voices.contains_key(voice_id) {
            anyhow::bail!("voice {voice_id} not installed");
        }
        self.default_voice_id = Some(voice_id.into());
        self.persist().await
    }

    /// Returns the on-disk path to the whisper STT model if installed,
    /// `None` otherwise (caller must trigger `download_stt_model`).
    pub fn stt_model_path(&self) -> Option<PathBuf> {
        let dir = self
            .voices_dir
            .join("_stt")
            .join("whisper-large-v3-turbo-q5_1");
        let weights = dir.join("ggml-large-v3-turbo-q5_1.bin");
        if weights.exists() {
            Some(weights)
        } else {
            None
        }
    }

    /// Bundled-voice seeding. The installer ships `af_heart` under
    /// `<bundle>/voices/af_heart/`; on first launch we copy it into
    /// the app-data voices dir if registry is empty. Best-effort —
    /// a missing or unsigned bundled voice is logged but doesn't block
    /// startup (user can still download voices later).
    pub async fn ensure_bundled_voice_seeded(&mut self, bundled_voices_dir: &Path) -> Result<()> {
        if !self.voices.is_empty() {
            return Ok(());
        }
        let af_heart = bundled_voices_dir.join("af_heart");
        if !af_heart.exists() {
            return Ok(());
        }
        let manifest_bytes = fs::read(af_heart.join("manifest.json"))
            .await
            .context("read bundled manifest.json")?;
        let sig_bytes = fs::read(af_heart.join("manifest.json.sig"))
            .await
            .context("read bundled manifest.json.sig")?;
        let bundle = verify_and_parse(&manifest_bytes, &sig_bytes)
            .context("verify bundled voice manifest")?;
        let manifest = match bundle {
            AssetBundle::Voice(v) => v,
            AssetBundle::SttModel(_) => {
                anyhow::bail!("bundled voice manifest has wrong kind (expected voice)")
            }
        };
        let target = self.voices_dir.join(&manifest.voice_id);
        if !target.exists() {
            fs::create_dir_all(&target).await?;
            // Synchronous std::fs::read_dir is fine here — small dir, runs once at first launch.
            for entry in std::fs::read_dir(&af_heart)? {
                let entry = entry?;
                let dst = target.join(entry.file_name());
                fs::copy(entry.path(), dst).await?;
            }
        }
        self.install(&manifest, target).await?;
        Ok(())
    }

    async fn persist(&self) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(self)?;
        let tmp = self.registry_path.with_extension("json.tmp");
        fs::write(&tmp, bytes).await?;
        fs::rename(&tmp, &self.registry_path).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn fake_voice(id: &str) -> VoiceManifest {
        VoiceManifest {
            schema_version: 1,
            voice_id: id.into(),
            display_name: format!("Voice {id}"),
            language: "en-US".into(),
            sample_rate_hz: 24000,
            license: "MIT".into(),
            creator: "test".into(),
            assets: vec![],
            size_bytes_total: 0,
        }
    }

    #[tokio::test]
    async fn empty_registry_loads_and_persists() {
        let dir = tempdir().unwrap();
        let reg = VoiceRegistry::load(dir.path()).await.unwrap();
        assert!(reg.voices.is_empty());
        assert_eq!(reg.version, 1);
    }

    #[tokio::test]
    async fn install_and_lookup_round_trips_through_disk() {
        let dir = tempdir().unwrap();
        let mut reg = VoiceRegistry::load(dir.path()).await.unwrap();
        let target = reg.voices_dir.join("af_heart");
        reg.install(&fake_voice("af_heart"), target.clone())
            .await
            .unwrap();
        assert_eq!(reg.default_voice_id.as_deref(), Some("af_heart"));

        // Reopen from disk; default + installed entry survive.
        let reg2 = VoiceRegistry::load(dir.path()).await.unwrap();
        assert_eq!(reg2.default_voice_id.as_deref(), Some("af_heart"));
        assert!(reg2.get("af_heart").is_some());
    }

    #[tokio::test]
    async fn second_install_does_not_change_default() {
        let dir = tempdir().unwrap();
        let mut reg = VoiceRegistry::load(dir.path()).await.unwrap();
        reg.install(&fake_voice("af_heart"), reg.voices_dir.join("af_heart"))
            .await
            .unwrap();
        reg.install(&fake_voice("am_michael"), reg.voices_dir.join("am_michael"))
            .await
            .unwrap();
        assert_eq!(reg.default_voice_id.as_deref(), Some("af_heart"));
    }

    #[tokio::test]
    async fn set_default_rejects_unknown_voice() {
        let dir = tempdir().unwrap();
        let mut reg = VoiceRegistry::load(dir.path()).await.unwrap();
        let r = reg.set_default("not_installed").await;
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn corrupt_registry_resets_to_empty() {
        let dir = tempdir().unwrap();
        let voices = dir.path().join("voices");
        std::fs::create_dir_all(&voices).unwrap();
        std::fs::write(voices.join("registry.json"), b"{not valid json").unwrap();
        let reg = VoiceRegistry::load(dir.path()).await.unwrap();
        assert!(reg.voices.is_empty());
    }

    #[tokio::test]
    async fn stt_model_path_returns_none_when_missing() {
        let dir = tempdir().unwrap();
        let reg = VoiceRegistry::load(dir.path()).await.unwrap();
        assert!(reg.stt_model_path().is_none());
    }

    #[tokio::test]
    async fn stt_model_path_detects_installed_weights() {
        let dir = tempdir().unwrap();
        let reg = VoiceRegistry::load(dir.path()).await.unwrap();
        let stt_dir = reg
            .voices_dir
            .join("_stt")
            .join("whisper-large-v3-turbo-q5_1");
        std::fs::create_dir_all(&stt_dir).unwrap();
        std::fs::write(stt_dir.join("ggml-large-v3-turbo-q5_1.bin"), b"x").unwrap();
        assert!(reg.stt_model_path().is_some());
    }
}

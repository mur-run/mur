//! STT engine — whisper.cpp backend (statically linked via whisper-rs).
//!
//! Default-off lifecycle: constructed empty by `VoiceManager::new`; the
//! GUI calls `load_model` after the user opts in via Settings → Voice
//! → Enable. `unload` is called from `voice_disable` to free RAM.

pub mod whisper;

use anyhow::Result;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::RwLock;

use whisper::WhisperBackend;

pub struct SttEngine {
    backend: Arc<RwLock<Option<WhisperBackend>>>,
}

impl SttEngine {
    pub fn new() -> Result<Self> {
        Ok(Self {
            backend: Arc::new(RwLock::new(None)),
        })
    }

    /// Load whisper context from disk. Async-friendly via spawn_blocking
    /// since whisper.cpp's mmap can stall on disk I/O.
    pub async fn load_model(&self, model_path: &Path) -> Result<()> {
        let path_owned = model_path.to_path_buf();
        let backend = tokio::task::spawn_blocking(move || WhisperBackend::load(&path_owned))
            .await
            .map_err(|e| anyhow::anyhow!("spawn_blocking join: {e}"))??;
        *self.backend.write().await = Some(backend);
        Ok(())
    }

    /// Drop the loaded whisper context, freeing RAM. Used by
    /// `voice_disable` to honor the opt-in promise.
    pub async fn unload(&self) {
        *self.backend.write().await = None;
    }

    /// True if a model is loaded; false means the GUI must call
    /// `voice_stt_download` (or `voice_enable` which subsumes it)
    /// before any transcribe attempt can succeed.
    pub async fn is_ready(&self) -> bool {
        self.backend.read().await.is_some()
    }

    pub async fn transcribe(&self, samples_i16: &[i16], language: Option<&str>) -> Result<String> {
        // Off-load the synchronous whisper run from the async runtime.
        let backend = self.backend.clone();
        let samples = samples_i16.to_vec();
        let lang = language.map(|s| s.to_string());
        tokio::task::spawn_blocking(move || {
            // Acquire read lock inside the blocking task.
            let g = futures::executor::block_on(backend.read());
            let b = g.as_ref().ok_or_else(|| {
                anyhow::anyhow!(
                    "STT model not loaded — run `voice_stt_download` (or `voice_enable`) first"
                )
            })?;
            b.transcribe(&samples, lang.as_deref())
        })
        .await
        .map_err(|e| anyhow::anyhow!("spawn_blocking join: {e}"))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unloaded_engine_is_not_ready() {
        let e = SttEngine::new().unwrap();
        assert!(!e.is_ready().await);
    }

    #[tokio::test]
    async fn unload_is_idempotent_when_not_loaded() {
        let e = SttEngine::new().unwrap();
        e.unload().await;
        e.unload().await;
        assert!(!e.is_ready().await);
    }

    #[tokio::test]
    async fn transcribe_without_model_errors() {
        let e = SttEngine::new().unwrap();
        let r = e.transcribe(&[0i16; 16000], Some("en")).await;
        assert!(r.is_err());
        assert!(format!("{r:?}").contains("STT model not loaded"));
    }
}

//! TTS engine — Kokoro 82M backend.
//!
//! Layout:
//! * `g2p.rs`            — grapheme-to-phoneme dispatch (en + zh)
//! * `kokoro.rs`         — ort session loader + 1-token prewarm
//! * `sentence_split.rs` — streaming sentence splitter
//! * (this file)         — `TtsEngine` facade + streaming synthesis loop
//!
//! Lifecycle: constructed empty by `VoiceManager::new`; the GUI calls
//! `load_voice` after the user enables voice (default-off, see roadmap
//! §4.1 + plan §M1.5.1). `unload` is called from `voice_disable` to
//! free RAM while voice is off.

pub mod g2p;
pub mod kokoro;
pub mod sentence_split;

use anyhow::Result;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;

use kokoro::KokoroSession;

pub struct TtsEngine {
    session: Option<Arc<Mutex<KokoroSession>>>,
    current_voice_id: Option<String>,
}

impl TtsEngine {
    pub fn new() -> Result<Self> {
        Ok(Self {
            session: None,
            current_voice_id: None,
        })
    }

    /// Load (or replace) the active voice. ONNX file load happens off
    /// the async runtime via `spawn_blocking` since it's CPU/IO heavy.
    pub async fn load_voice(
        &mut self,
        voice_id: &str,
        onnx_path: &Path,
        sample_rate_hz: u32,
    ) -> Result<()> {
        let onnx_owned = onnx_path.to_path_buf();
        let session =
            tokio::task::spawn_blocking(move || KokoroSession::load(&onnx_owned, sample_rate_hz))
                .await
                .map_err(|e| anyhow::anyhow!("spawn_blocking join: {e}"))??;
        self.session = Some(Arc::new(Mutex::new(session)));
        self.current_voice_id = Some(voice_id.into());
        Ok(())
    }

    /// Drop the loaded ort session + voice id, freeing RAM. Used by
    /// `voice_disable` to honor the opt-in promise.
    pub fn unload(&mut self) {
        self.session = None;
        self.current_voice_id = None;
    }

    pub fn current_voice_id(&self) -> Option<&str> {
        self.current_voice_id.as_deref()
    }

    pub fn is_loaded(&self) -> bool {
        self.session.is_some()
    }

    pub async fn sample_rate_hz(&self) -> Option<u32> {
        if let Some(s) = &self.session {
            Some(s.lock().await.sample_rate_hz())
        } else {
            None
        }
    }

    /// Synthesize a single sentence; returns f32 PCM samples at the
    /// session's native sample rate.
    pub async fn synthesize_sentence(
        &self,
        text: &str,
        language: &str,
        voice_idx: i64,
    ) -> Result<Vec<f32>> {
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("no voice loaded"))?;
        let ids = g2p::text_to_phoneme_ids(text, language)?;
        if ids.is_empty() {
            return Ok(vec![]);
        }
        let session = session.clone();
        // Inference is CPU-bound; off-load from the async runtime.
        tokio::task::spawn_blocking(move || {
            let mut g = session.blocking_lock();
            g.synthesize_phonemes(&ids, voice_idx)
        })
        .await
        .map_err(|e| anyhow::anyhow!("spawn_blocking join: {e}"))?
    }

    /// Stream text chunks through the splitter and yield PCM per
    /// complete sentence. `on_chunk` receives `(sentence_index, samples)`.
    /// Drives the first-byte trick: as soon as the LLM emits the first
    /// terminating punctuation, that sentence reaches TTS while later
    /// tokens are still streaming.
    pub async fn synthesize_streaming<F>(
        &self,
        mut text_chunks: tokio::sync::mpsc::Receiver<String>,
        language: &str,
        voice_idx: i64,
        mut on_chunk: F,
    ) -> Result<()>
    where
        F: FnMut(usize, &[f32]) + Send,
    {
        let mut splitter = sentence_split::SentenceSplitter::new();
        let mut idx = 0usize;
        while let Some(chunk) = text_chunks.recv().await {
            for sentence in splitter.push(&chunk) {
                let samples = self
                    .synthesize_sentence(&sentence, language, voice_idx)
                    .await?;
                if !samples.is_empty() {
                    on_chunk(idx, &samples);
                    idx += 1;
                }
            }
        }
        if let Some(tail) = splitter.flush() {
            let samples = self.synthesize_sentence(&tail, language, voice_idx).await?;
            if !samples.is_empty() {
                on_chunk(idx, &samples);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unloaded_engine_reports_state_correctly() {
        let engine = TtsEngine::new().unwrap();
        assert!(!engine.is_loaded());
        assert!(engine.current_voice_id().is_none());
        assert!(engine.sample_rate_hz().await.is_none());
    }

    #[tokio::test]
    async fn synthesize_without_voice_loaded_errors() {
        let engine = TtsEngine::new().unwrap();
        let r = engine.synthesize_sentence("hello", "en-US", 0).await;
        assert!(r.is_err());
        assert!(format!("{r:?}").contains("no voice loaded"));
    }

    #[tokio::test]
    async fn unload_clears_state() {
        let mut engine = TtsEngine::new().unwrap();
        // Already empty; unload is a no-op but must not panic.
        engine.unload();
        assert!(!engine.is_loaded());
        assert!(engine.current_voice_id().is_none());
    }
}

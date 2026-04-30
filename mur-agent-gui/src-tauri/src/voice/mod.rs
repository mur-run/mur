//! Voice subsystem — local-only TTS (Kokoro 82M) + STT (whisper.cpp).
//!
//! All inference runs in the Tauri sidecar process. The
//! `mur-agent-runtime` itself remains voice-blind; voice is purely a
//! GUI-tier concern (PTT hotkey, audio I/O, settings panel).
//!
//! The "voice never leaves this Mac" promise (roadmap §4.1) is upheld by:
//!
//! * No outbound network in this module except the signed-CDN voice
//!   download path (`download.rs`); model API endpoints are NOT touched.
//! * The runtime never sees raw audio buffers — only post-STT
//!   transcripts.
//! * Speech synthesis is local; there is no cloud TTS fallback in v1.
//!
//! Module map:
//!
//! ```text
//! voice/
//!   audio/    cpal capture + playback + PTT state machine
//!   download  signed-CDN download client with SHA-256 verify
//!   manifest  Ed25519-signed voice manifest schema + verify
//!   registry  installed-voice index (registry.json)
//!   stt/      whisper-rs adapter
//!   tts/      Kokoro ort session + G2P + sentence splitter
//! ```

pub mod audio;
pub mod download;
pub mod manifest;
pub mod registry;
pub mod stt;
pub mod tts;

use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Top-level voice manager held in Tauri state.
///
/// `tts` and `stt` start empty (no model loaded). The frontend voice
/// picker triggers `load_voice` on the TTS engine; STT lazy-loads
/// when the first PTT capture lands.
pub struct VoiceManager {
    pub tts: Arc<RwLock<tts::TtsEngine>>,
    pub stt: Arc<RwLock<stt::SttEngine>>,
    pub registry: Arc<RwLock<registry::VoiceRegistry>>,
    pub app_data_dir: PathBuf,
}

impl VoiceManager {
    /// Initialise the voice subsystem against the given app-data dir.
    /// `<app_data_dir>/voices/` is created if missing.
    pub async fn new(app_data_dir: PathBuf) -> anyhow::Result<Self> {
        let registry = Arc::new(RwLock::new(
            registry::VoiceRegistry::load(&app_data_dir).await?,
        ));
        let tts = Arc::new(RwLock::new(tts::TtsEngine::new()?));
        let stt = Arc::new(RwLock::new(stt::SttEngine::new()?));
        Ok(Self {
            tts,
            stt,
            registry,
            app_data_dir,
        })
    }
}

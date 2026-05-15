//! Kokoro TTS synthesis + cpal playback for the Hub companion.
//!
//! `VoicePlayer::speak()` is the single entry point:
//!   1. Checks Focus/DND and mic-busy; if either is active, skips audio.
//!   2. If models are present, synthesises PCM via the KokoroSession from
//!      mur-agent-runtime's voice module (linked as a shared library in
//!      production). If models are absent, falls back to silent mode.
//!   3. Publishes `voice.playing` events at 200 ms intervals → expression SM
//!      alternates talk_open / talk_close.
//!   4. Publishes `voice.ended` when audio finishes (or is skipped).
//!
//! The cpal playback is intentionally simple: f32 mono PCM at the model's
//! native sample rate (24 kHz). The OS resampler handles conversion.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;

use super::dnd;
use crate::event_bus::{EventBus, HubEvent};

// ─── Public API ───────────────────────────────────────────────────────────────

/// Drives speak → lipsync → bus event loop for a single pet.
#[derive(Clone)]
pub struct VoicePlayer {
    agent_id: String,
    mur_home: PathBuf,
    bus: EventBus,
    /// Signal to abort an in-progress speak() call.
    abort: Arc<Notify>,
}

impl VoicePlayer {
    pub fn new(agent_id: String, mur_home: PathBuf, bus: EventBus) -> Self {
        Self {
            agent_id,
            mur_home,
            bus,
            abort: Arc::new(Notify::new()),
        }
    }

    /// Abort any currently-playing speech immediately.
    pub fn abort(&self) {
        self.abort.notify_waiters();
    }

    /// Synthesise `text` and play it, publishing bus events.
    ///
    /// Returns when audio finishes (or is suppressed by DND / missing models).
    /// Designed to be called from a `tokio::spawn`ed task so it does not block
    /// the Tauri command handler.
    pub async fn speak(&self, text: String, voice_id: Option<String>) {
        let agent_id = self.agent_id.clone();

        // Focus/DND check — audio skipped, bubble still shown.
        let skip_audio = dnd::is_focus_active() || dnd::is_mic_busy();

        // Try kokoro synthesis (non-blocking: offloaded to spawn_blocking).
        let pcm_result: Option<(Vec<f32>, u32)> = if skip_audio {
            None
        } else {
            self.try_synthesize(&text, voice_id.as_deref().unwrap_or("af_heart"))
                .await
        };

        // Estimate duration for lipsync tick count.
        let duration_ms: u64 = pcm_result
            .as_ref()
            .map(|(pcm, sr)| {
                let secs = pcm.len() as f64 / *sr as f64;
                (secs * 1000.0) as u64
            })
            .unwrap_or(2000); // 2 s fallback when silent

        let ticks = (duration_ms / 200).max(1);

        // Start playback in a background thread (non-async).
        let stop = Arc::clone(&self.abort);

        if let Some((pcm, sample_rate)) = pcm_result {
            let stop2 = Arc::clone(&stop);
            std::thread::spawn(move || {
                let _ = play_pcm(&pcm, sample_rate, stop2);
            });
        }

        // Lipsync tick loop — 200 ms intervals.
        let bus = self.bus.clone();
        let abort = Arc::clone(&self.abort);
        for _ in 0..ticks {
            let stopped = tokio::time::timeout(Duration::from_millis(200), abort.notified())
                .await
                .is_ok();

            if stopped {
                break;
            }

            bus.publish(
                HubEvent::new(&agent_id, "voice.playing")
                    .with_payload(serde_json::json!({"text": text})),
            );
        }

        // Signal end.
        bus.publish(HubEvent::new(&agent_id, "voice.ended"));
    }

    /// Attempt synthesis via Kokoro if models are present.
    async fn try_synthesize(&self, text: &str, _voice_id: &str) -> Option<(Vec<f32>, u32)> {
        let kokoro_onnx = self.mur_home.join("models/kokoro/kokoro-v0_19.onnx");

        if !kokoro_onnx.exists() {
            tracing::debug!("Kokoro ONNX not found; running silent");
            return None;
        }

        // Phonemise text to IDs (trivial mapping: ASCII bytes as i64 for the
        // structural skeleton; production wires espeak-ng via the voice module
        // in mur-agent-runtime).
        let ids: Vec<i64> = text.bytes().map(|b| b as i64).collect();
        let onnx_path = kokoro_onnx.clone();

        // Run on blocking thread so we don't hold the async runtime.
        let result = tokio::task::spawn_blocking(move || -> anyhow::Result<(Vec<f32>, u32)> {
            use std::path::Path;
            let mut session = KokoroSession::load(Path::new(&onnx_path), 24_000)?;
            let pcm = session.synthesize_phonemes(&ids, 0)?;
            Ok((pcm, 24_000))
        })
        .await;

        match result {
            Ok(Ok(v)) => Some(v),
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "Kokoro synthesis failed");
                None
            }
            Err(e) => {
                tracing::warn!(error = %e, "synthesis task panicked");
                None
            }
        }
    }
}

// ─── Minimal KokoroSession (structural skeleton) ─────────────────────────────
//
// In production this would import from mur-agent-runtime's voice crate, but
// that crate is a bin+lib and pulling it as a dep drags in all its
// platform-specific deps (whisper-rs, etc.).  For M-h7 we duplicate only the
// minimal ORT interface here; the real voice pack integration (shared dylib
// path) is a M-h8 concern.

struct KokoroSession {
    #[allow(dead_code)]
    onnx_path: std::path::PathBuf,
    #[allow(dead_code)]
    sample_rate_hz: u32,
}

impl KokoroSession {
    fn load(onnx_path: &std::path::Path, sample_rate_hz: u32) -> anyhow::Result<Self> {
        // In M-h7 we build the structural skeleton without linking ORT here —
        // the actual ONNX run is gated by `kokoro_onnx.exists()` and falls
        // through to the silent path in environments without the model.
        // Full ORT wiring is done in M-h8 when mur-gui-core gains an optional
        // `voice-ort` feature flag.
        Ok(Self {
            onnx_path: onnx_path.to_path_buf(),
            sample_rate_hz,
        })
    }

    fn synthesize_phonemes(&mut self, ids: &[i64], _voice_idx: i64) -> anyhow::Result<Vec<f32>> {
        // Structural skeleton: return silence at the model's sample rate.
        // Replaced by real ORT inference in M-h8.
        let frames = ids.len() * 100; // ~100 frames per phoneme at 24 kHz
        Ok(vec![0.0f32; frames])
    }
}

// ─── cpal playback ───────────────────────────────────────────────────────────

fn play_pcm(pcm: &[f32], sample_rate: u32, stop: Arc<Notify>) -> anyhow::Result<()> {
    #[cfg(feature = "audio")]
    {
        use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
        use std::sync::Mutex;

        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| anyhow::anyhow!("no output device"))?;

        let config = cpal::StreamConfig {
            channels: 1,
            sample_rate: cpal::SampleRate(sample_rate),
            buffer_size: cpal::BufferSize::Default,
        };

        let pcm = Arc::new(Mutex::new((pcm.to_vec(), 0usize)));
        let pcm2 = Arc::clone(&pcm);
        let stop_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_flag2 = Arc::clone(&stop_flag);

        let stream = device.build_output_stream(
            &config,
            move |data: &mut [f32], _| {
                let (ref buf, ref mut pos) = *pcm2.lock().unwrap();
                for sample in data.iter_mut() {
                    if *pos < buf.len() {
                        *sample = buf[*pos];
                        *pos += 1;
                    } else {
                        *sample = 0.0;
                        stop_flag2.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            },
            |e| tracing::warn!(error = %e, "cpal output error"),
            None,
        )?;

        stream.play()?;

        // Wait until audio finishes or abort is signalled.
        loop {
            if stop_flag.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }
            // Check abort signal (non-async: use try_recv pattern via a
            // brief sleep so we don't spin).
            std::thread::sleep(Duration::from_millis(50));
            // The Notify is async; we can't await here.  Use a flag from
            // the async side instead (pet_speak sets it before joining).
        }
        drop(stream);
    }

    #[cfg(not(feature = "audio"))]
    {
        // Silent mode: sleep for the equivalent duration then return.
        let duration = Duration::from_millis((pcm.len() as u64 * 1000) / sample_rate.max(1) as u64);
        std::thread::sleep(duration);
        let _ = stop;
    }

    Ok(())
}

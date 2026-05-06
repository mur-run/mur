//! VoiceNotifier — speaks companion messages via Kokoro TTS.
//! Implements `companion::notifier::Notifier` so it slots into outbox step 11.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use crate::companion::notifier::{CompanionMessage, Notifier, NotifyOutcome};
use crate::voice::{audio, tts::KOKORO_SAMPLE_RATE, tts::KokoroTts};

// ─── KokoroTtsTrait ──────────────────────────────────────────────────────────

/// Trait abstraction over KokoroTts so tests can inject a mock without
/// loading a real ONNX model.
pub trait KokoroTtsTrait: Send + Sync {
    fn synthesize(&self, text: &str) -> anyhow::Result<Vec<f32>>;
}

impl KokoroTtsTrait for KokoroTts {
    fn synthesize(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        self.synthesize(text)
    }
}

// ─── AudioPlayerTrait ────────────────────────────────────────────────────────

/// Trait abstraction over `audio::play_pcm` so tests can inject a no-op
/// player without triggering real cpal hardware access.
pub trait AudioPlayerTrait: Send + Sync {
    fn play_pcm(
        &self,
        samples: &[f32],
        sample_rate: u32,
        output_device: Option<&str>,
    ) -> anyhow::Result<()>;
}

/// Production audio player — delegates to `audio::play_pcm`.
struct DefaultAudioPlayer;

impl AudioPlayerTrait for DefaultAudioPlayer {
    fn play_pcm(
        &self,
        samples: &[f32],
        sample_rate: u32,
        output_device: Option<&str>,
    ) -> anyhow::Result<()> {
        // TODO: thread output_device through audio::play_pcm when that function
        // gains device-selection support (audio::play_pcm currently uses the OS default)
        let _ = output_device;
        audio::play_pcm(samples, sample_rate)
    }
}

// ─── VoiceNotifier ───────────────────────────────────────────────────────────

pub struct VoiceNotifier {
    tts: Box<dyn KokoroTtsTrait>,
    audio: Arc<dyn AudioPlayerTrait>,
    /// cpal output device name; None = OS default.
    output_device: Option<String>,
}

impl VoiceNotifier {
    /// Production constructor: wraps a real `KokoroTts`.
    pub fn new(tts: KokoroTts, output_device: Option<String>) -> Self {
        Self {
            tts: Box::new(tts),
            audio: Arc::new(DefaultAudioPlayer),
            output_device,
        }
    }

    /// Test-only constructor: injects an arbitrary mock TTS + no-op audio.
    #[cfg(test)]
    pub fn with_mock_tts(tts: impl KokoroTtsTrait + 'static) -> Self {
        struct NoopAudio;
        impl AudioPlayerTrait for NoopAudio {
            fn play_pcm(&self, _: &[f32], _: u32, _: Option<&str>) -> anyhow::Result<()> {
                Ok(())
            }
        }
        Self {
            tts: Box::new(tts),
            audio: Arc::new(NoopAudio),
            output_device: None,
        }
    }
}

#[async_trait]
impl Notifier for VoiceNotifier {
    fn name(&self) -> &'static str {
        "VoiceNotifier"
    }

    async fn send(&self, msg: &CompanionMessage) -> Result<NotifyOutcome> {
        if msg.body.trim().is_empty() {
            return Ok(NotifyOutcome::Skipped {
                reason: "empty_body".into(),
            });
        }

        let samples = match self.tts.synthesize(&msg.body) {
            Ok(s) if s.is_empty() => {
                return Ok(NotifyOutcome::Skipped {
                    reason: "tts_empty_output".into(),
                });
            }
            Ok(s) => s,
            Err(e) => return Ok(NotifyOutcome::Failed(e)),
        };

        // Play on a blocking thread — cpal requires synchronous calls.
        let device = self.output_device.clone();
        let audio = Arc::clone(&self.audio);
        let result = tokio::task::spawn_blocking(move || {
            audio.play_pcm(&samples, KOKORO_SAMPLE_RATE, device.as_deref())
        })
        .await;

        match result {
            Ok(Ok(())) => Ok(NotifyOutcome::Delivered),
            Ok(Err(e)) => Ok(NotifyOutcome::Failed(e)),
            Err(join_err) => Ok(NotifyOutcome::Failed(anyhow::anyhow!(
                "spawn_blocking panicked: {join_err}"
            ))),
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use chrono::Utc;
    use mur_common::companion::Situation;

    use super::{KokoroTtsTrait, VoiceNotifier};
    use crate::companion::notifier::{CompanionMessage, Notifier, NotifyOutcome};

    // ── Mock TTS ──────────────────────────────────────────────────────────────

    struct MockTts {
        called: Arc<AtomicBool>,
        /// When Some, synthesize returns these samples; when None returns Err.
        samples: Option<Vec<f32>>,
    }

    impl MockTts {
        fn ok(called: Arc<AtomicBool>) -> Self {
            Self {
                called,
                samples: Some(vec![0.1_f32, 0.2, -0.1]),
            }
        }

        fn empty(called: Arc<AtomicBool>) -> Self {
            Self {
                called,
                samples: Some(vec![]),
            }
        }
    }

    impl KokoroTtsTrait for MockTts {
        fn synthesize(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
            self.called.store(true, Ordering::SeqCst);
            match &self.samples {
                Some(s) => Ok(s.clone()),
                None => Err(anyhow::anyhow!("mock tts error")),
            }
        }
    }

    // ── helpers ───────────────────────────────────────────────────────────────

    fn sample_msg(body: &str) -> CompanionMessage {
        CompanionMessage {
            id: "test_voice_id_01".to_string(),
            situation: Situation::GentleCheckIn,
            template_id: "check_in_001".to_string(),
            locale: "en-US".to_string(),
            body: body.to_string(),
            generated_at: Utc::now(),
        }
    }

    // ── Test 1: TTS called and outcome is Delivered ───────────────────────────

    #[tokio::test]
    async fn voice_notifier_calls_tts_and_returns_delivered() {
        let called = Arc::new(AtomicBool::new(false));
        let mock = MockTts::ok(Arc::clone(&called));

        let notifier = VoiceNotifier::with_mock_tts(mock);
        let msg = sample_msg("Hello, how are you today?");

        let outcome = notifier.send(&msg).await.expect("send must not error");

        assert!(
            called.load(Ordering::SeqCst),
            "TTS synthesize must be called"
        );
        assert!(
            matches!(outcome, NotifyOutcome::Delivered),
            "expected Delivered"
        );
    }

    // ── Test 2: Empty body skips TTS ─────────────────────────────────────────

    #[tokio::test]
    async fn voice_notifier_skips_empty_body() {
        let called = Arc::new(AtomicBool::new(false));
        let mock = MockTts::ok(Arc::clone(&called));

        let notifier = VoiceNotifier::with_mock_tts(mock);
        let msg = sample_msg("   "); // whitespace-only → empty after trim

        let outcome = notifier.send(&msg).await.expect("send must not error");

        assert!(
            !called.load(Ordering::SeqCst),
            "TTS must NOT be called for empty body"
        );
        match outcome {
            NotifyOutcome::Skipped { reason } => {
                assert_eq!(reason, "empty_body", "reason must be 'empty_body'");
            }
            other => panic!(
                "expected Skipped{{empty_body}}, got {:?}",
                match other {
                    NotifyOutcome::Delivered => "Delivered",
                    NotifyOutcome::Failed(_) => "Failed",
                    NotifyOutcome::Skipped { .. } => "Skipped(other)",
                }
            ),
        }
    }

    // ── Test 3: TTS returning empty samples → Skipped ────────────────────────

    #[tokio::test]
    async fn voice_notifier_skips_empty_tts_output() {
        let called = Arc::new(AtomicBool::new(false));
        let mock = MockTts::empty(Arc::clone(&called));

        let notifier = VoiceNotifier::with_mock_tts(mock);
        let msg = sample_msg("Non-empty body");

        let outcome = notifier.send(&msg).await.expect("send must not error");

        assert!(called.load(Ordering::SeqCst), "TTS must be called");
        assert!(
            matches!(outcome, NotifyOutcome::Skipped { ref reason } if reason == "tts_empty_output"),
            "expected Skipped{{tts_empty_output}}"
        );
    }
}

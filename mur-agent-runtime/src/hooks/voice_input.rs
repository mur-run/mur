//! VoiceInputHook — B0 rule 18.
//!
//! On every `on_prompt_submit`:
//! 1. Captures mic audio (real: cpal VAD-gated; test: pre-loaded samples)
//! 2. Transcribes with whisper.cpp
//! 3. Wraps the transcript in `<untrusted_voice_input>` spotlight tag
//! 4. Sets `after_untrusted_input` turn flag
//!
//! Design mirrors D3 drag-drop: untrusted input is wrapped so the model
//! knows it came from a mic and the B0 rule-4 cooldown applies.

use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::hooks::patch::UntrustedWrapper;
use crate::hooks::{Hook, HookCtx, HookError, PromptPatch, PromptView};
use crate::voice::stt::{VadGate, WhisperStt};

#[cfg(not(test))]
use crate::voice::audio;

/// Trait abstraction over WhisperStt so tests can inject a mock.
pub trait WhisperSttTrait: Send + Sync {
    fn transcribe(&self, samples: &[f32]) -> anyhow::Result<String>;
}

impl WhisperSttTrait for WhisperStt {
    fn transcribe(&self, samples: &[f32]) -> anyhow::Result<String> {
        self.transcribe(samples)
    }
}

pub struct VoiceInputHook {
    stt: Box<dyn WhisperSttTrait>,
    vad: VadGate,
    input_device: Option<String>,
    max_capture: Duration,
    /// Test-only: pre-loaded samples bypassing the real microphone.
    #[cfg(test)]
    test_samples: Vec<f32>,
}

impl VoiceInputHook {
    /// Production constructor.
    pub fn new(
        stt: WhisperStt,
        vad: VadGate,
        input_device: Option<String>,
        max_capture: Duration,
    ) -> Self {
        Self {
            stt: Box::new(stt),
            vad,
            input_device,
            max_capture,
            #[cfg(test)]
            test_samples: vec![],
        }
    }

    /// Test constructor: injects mock STT and pre-loaded audio samples.
    #[cfg(test)]
    pub fn with_mock_stt(stt: Box<dyn WhisperSttTrait>, test_samples: Vec<f32>) -> Self {
        Self {
            stt,
            vad: VadGate::default(),
            input_device: None,
            max_capture: Duration::from_secs(10),
            test_samples,
        }
    }
}

#[async_trait]
impl Hook for VoiceInputHook {
    fn name(&self) -> &str {
        "VoiceInputHook"
    }

    async fn on_prompt_submit(
        &self,
        _ctx: &HookCtx,
        _view: &PromptView,
        _tok: &CancellationToken,
    ) -> Result<PromptPatch, HookError> {
        // Capture audio (real mic or test samples).
        #[cfg(test)]
        let samples = self.test_samples.clone();

        #[cfg(not(test))]
        let samples = {
            let device = self.input_device.clone();
            let vad = self.vad.clone();
            let max = self.max_capture;
            tokio::task::spawn_blocking(move || audio::capture_vad_gated(&device, &vad, max))
                .await
                .map_err(|e| HookError::Runtime(format!("spawn_blocking: {e}")))?
                .map_err(|e| HookError::Runtime(e.to_string()))?
        };

        if samples.is_empty() {
            return Ok(PromptPatch::noop());
        }

        // Transcribe.
        let transcript = self
            .stt
            .transcribe(&samples)
            .map_err(|e| HookError::Runtime(e.to_string()))?;

        if transcript.is_empty() {
            return Ok(PromptPatch::noop());
        }

        // B0 rule 18: wrap transcript in spotlight tag.
        Ok(PromptPatch {
            wrap_untrusted: vec![UntrustedWrapper {
                tag: "untrusted_voice_input".into(),
                source: "mic".into(),
                content: transcript,
            }],
            turn_flags: vec!["after_untrusted_input".into()],
            ..PromptPatch::noop()
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::hooks::types::HookCtx;

    struct MockStt {
        transcript: String,
    }

    impl WhisperSttTrait for MockStt {
        fn transcribe(&self, _samples: &[f32]) -> anyhow::Result<String> {
            Ok(self.transcript.clone())
        }
    }

    #[tokio::test]
    async fn wraps_transcript_in_untrusted_voice_input_tag() {
        let hook = VoiceInputHook::with_mock_stt(
            Box::new(MockStt {
                transcript: "open the pod bay doors".into(),
            }),
            vec![0.1_f32; 1600], // non-silent → VAD passes
        );

        let ctx = HookCtx::for_test_with_home(PathBuf::new(), 0);
        let view = PromptView::empty();
        let tok = CancellationToken::new();

        let patch = hook.on_prompt_submit(&ctx, &view, &tok).await.unwrap();

        assert_eq!(patch.wrap_untrusted.len(), 1);
        let w = &patch.wrap_untrusted[0];
        assert_eq!(w.tag, "untrusted_voice_input");
        assert_eq!(w.source, "mic");
        assert_eq!(w.content, "open the pod bay doors");
        assert!(
            patch
                .turn_flags
                .contains(&"after_untrusted_input".to_string()),
            "expected after_untrusted_input turn flag"
        );
    }

    #[tokio::test]
    async fn empty_transcript_returns_noop() {
        let hook = VoiceInputHook::with_mock_stt(
            Box::new(MockStt {
                transcript: "".into(),
            }),
            vec![0.1_f32; 1600],
        );

        let ctx = HookCtx::for_test_with_home(PathBuf::new(), 0);
        let view = PromptView::empty();
        let tok = CancellationToken::new();

        let patch = hook.on_prompt_submit(&ctx, &view, &tok).await.unwrap();

        assert!(patch.wrap_untrusted.is_empty(), "empty transcript → noop");
        assert!(patch.turn_flags.is_empty());
    }

    #[tokio::test]
    async fn empty_samples_returns_noop_without_calling_stt() {
        // If mic captures nothing (all-zero), return noop before even calling STT.
        let hook = VoiceInputHook::with_mock_stt(
            Box::new(MockStt {
                transcript: "should not be called".into(),
            }),
            vec![], // empty samples
        );

        let ctx = HookCtx::for_test_with_home(PathBuf::new(), 0);
        let view = PromptView::empty();
        let tok = CancellationToken::new();

        let patch = hook.on_prompt_submit(&ctx, &view, &tok).await.unwrap();

        assert!(patch.wrap_untrusted.is_empty());
        assert!(patch.turn_flags.is_empty());
    }
}

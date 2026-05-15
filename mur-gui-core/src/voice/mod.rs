//! Voice pipeline for the Hub companion.
//!
//! Exposes two capabilities:
//!   - `dnd` — platform Focus / DND and microphone-busy detection.
//!   - `synth` — Kokoro TTS wrapper (optional; disabled when ONNX model is absent).
//!
//! The `VoicePlayer` drives the full speak-then-lipsync loop; callers only
//! call `VoicePlayer::speak()` and subscribe to bus events.

pub mod dnd;
pub mod synth;

pub use dnd::{is_focus_active, is_mic_busy};
pub use synth::VoicePlayer;

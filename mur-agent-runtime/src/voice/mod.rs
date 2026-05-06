//! D1 voice subsystem — on-device TTS (Kokoro 82M) + STT (whisper.cpp).
//!
//! Privacy invariant: no audio or transcript leaves the device.
//!
//! Entry point: model download via `download::ensure_model`.

pub mod audio;
pub mod download;
pub mod notifier;
pub mod stt;
pub mod tts;
pub mod types;

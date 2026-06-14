//! D1 voice subsystem — on-device TTS (Kokoro 82M) + STT (whisper.cpp).
//!
//! Privacy invariant: no audio or transcript leaves the device.
//!
//! Entry point: model download via `download::ensure_model`.

pub mod audio;
pub mod download;
pub mod network_audit;
pub mod stt;
pub mod types;

// Kokoro ONNX TTS + the companion VoiceNotifier that drives it. Only built
// with the `tts` feature (pulls onnxruntime); see the feature note in Cargo.toml.
#[cfg(feature = "tts")]
pub mod notifier;
#[cfg(feature = "tts")]
pub mod tts;

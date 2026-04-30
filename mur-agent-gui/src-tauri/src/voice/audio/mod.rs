//! Audio I/O — cpal-based capture + playback. Full impl lands in
//! M1.4.1 (capture) and M1.5.1 (playback).

pub mod ptt;

/// Sample rate whisper.cpp expects (mono i16).
pub const STT_SAMPLE_RATE_HZ: u32 = 16_000;

/// Default playback sample rate. Kokoro outputs 24 kHz; we resample
/// to 22.05 kHz for slightly faster synthesis at imperceptible quality
/// loss (per first-byte tuning notes in roadmap §4.1).
pub const TTS_PLAYBACK_SAMPLE_RATE_HZ: u32 = 22_050;

//! Synchronous PCM playback via cpal's default output device.
//! Used by `tts_speak` for fire-and-forget single-utterance synthesis.
//!
//! Streaming playback (TTS output streamed sentence-by-sentence into
//! a `PlaybackRing`, drained by an output stream callback) is the M1.5
//! follow-up; this synchronous variant is enough for the v1 voice-
//! picker preview button + opt-in panel "Hi, I'm here" greeting.

use anyhow::{Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Play a mono f32 PCM clip at `sample_rate_hz`. Blocks until
/// playback finishes or the device errors. Run from `spawn_blocking`
/// when called from async contexts.
pub fn play_pcm_blocking(samples_f32: &[f32], sample_rate_hz: u32) -> Result<()> {
    if samples_f32.is_empty() {
        return Ok(());
    }
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| anyhow!("no output device available"))?;
    let config = cpal::StreamConfig {
        channels: 1,
        sample_rate: cpal::SampleRate(sample_rate_hz),
        buffer_size: cpal::BufferSize::Default,
    };
    let samples = samples_f32.to_vec();
    let total = samples.len();
    let mut idx = 0usize;
    let done = Arc::new(AtomicBool::new(false));
    let done2 = done.clone();
    let stream = device.build_output_stream(
        &config,
        move |data: &mut [f32], _| {
            for slot in data.iter_mut() {
                if idx < samples.len() {
                    *slot = samples[idx];
                    idx += 1;
                } else {
                    *slot = 0.0;
                }
            }
            if idx >= total {
                done2.store(true, Ordering::SeqCst);
            }
        },
        |e| tracing::warn!(error = %e, "playback stream error"),
        None,
    )?;
    stream.play()?;
    while !done.load(Ordering::SeqCst) {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    // Small drain pause so the output device finishes the last buffer.
    std::thread::sleep(std::time::Duration::from_millis(40));
    Ok(())
}

//! cpal-based microphone capture and speaker playback.
//!
//! All hardware-touching functions are synchronous and intended to be
//! called inside `tokio::task::spawn_blocking`. Do NOT call them
//! directly from async context.
//!
//! Privacy: all audio stays on-device. No network I/O anywhere in this file.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::SampleFormat;

use crate::voice::stt::VadGate;

// ─── Device enumeration ───────────────────────────────────────────────────────

/// Returns the names of all available input devices on the default cpal host.
/// Returns an empty list (not an error) when no input devices exist (e.g. CI).
pub fn list_input_devices() -> Result<Vec<String>> {
    let host = cpal::default_host();
    let devices = host.input_devices().context("enumerate input devices")?;
    Ok(devices.filter_map(|d| d.name().ok()).collect())
}

// ─── Capture ──────────────────────────────────────────────────────────────────

/// Capture audio from the named input device (None = OS default).
///
/// Records until:
/// - `max_duration` elapses, OR
/// - the VAD detects `vad.silence_frames_to_stop` consecutive silent frames
///   after at least one speech frame
///
/// Returns 16 kHz mono f32 PCM samples resampled from the device's native rate.
///
/// **Must be called from a blocking thread (e.g. `spawn_blocking`).**
pub fn capture_vad_gated(
    device_name: &Option<String>,
    vad: &VadGate,
    max_duration: Duration,
) -> Result<Vec<f32>> {
    let host = cpal::default_host();
    let device = match device_name {
        Some(name) => host
            .input_devices()
            .context("enumerate input devices")?
            .find(|d| d.name().ok().as_deref() == Some(name.as_str()))
            .with_context(|| format!("input device '{name}' not found"))?,
        None => host
            .default_input_device()
            .context("no default input device")?,
    };

    let config = device
        .default_input_config()
        .context("default input config")?;

    let sample_rate = config.sample_rate().0;
    let channels = config.channels() as usize;

    // Shared ring buffer — stream callback appends; polling loop reads.
    let buf: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));

    let stream = build_input_stream(&device, &config, buf.clone())?;
    stream.play().context("start capture stream")?;

    let frame = vad.frame_size.max(1) * channels;
    let deadline = std::time::Instant::now() + max_duration;
    let mut speech_started = false;
    let mut silent_count = 0usize;

    loop {
        std::thread::sleep(Duration::from_millis(10));
        if std::time::Instant::now() >= deadline {
            break;
        }

        let samples = buf.lock().unwrap().clone();
        if samples.len() < frame {
            continue;
        }

        let last_frame = &samples[samples.len() - frame..];
        // Downmix multichannel to mono for VAD analysis.
        let mono: Vec<f32> = last_frame
            .chunks(channels)
            .map(|ch| ch.iter().sum::<f32>() / channels as f32)
            .collect();

        if vad.is_speech(&mono) {
            speech_started = true;
            silent_count = 0;
        } else if speech_started {
            silent_count += 1;
            if silent_count >= vad.silence_frames_to_stop {
                break;
            }
        }
    }

    drop(stream); // stop capture

    let raw = buf.lock().unwrap().clone();

    // Downmix to mono.
    let mono: Vec<f32> = raw
        .chunks(channels)
        .map(|ch| ch.iter().sum::<f32>() / channels as f32)
        .collect();

    // Resample to 16 kHz for whisper.
    Ok(resample_to_16k(&mono, sample_rate))
}

// ─── Playback ─────────────────────────────────────────────────────────────────

/// Play `samples` (f32 mono at `sample_rate` Hz) on the default output device.
///
/// Blocks until playback completes (up to 60 s safety timeout).
///
/// **Must be called from a blocking thread (e.g. `spawn_blocking`).**
pub fn play_pcm(samples: &[f32], sample_rate: u32) -> Result<()> {
    if samples.is_empty() {
        return Ok(());
    }

    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .context("no default output device")?;

    let config = cpal::StreamConfig {
        channels: 1,
        sample_rate: cpal::SampleRate(sample_rate),
        buffer_size: cpal::BufferSize::Default,
    };

    let pos = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let samples_arc = Arc::new(samples.to_vec());
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();

    let pos_c = pos.clone();
    let samples_c = samples_arc.clone();
    // done_tx is moved into the callback; we need to send only once.
    let mut sent = false;
    let stream = device.build_output_stream(
        &config,
        move |output: &mut [f32], _| {
            let p = pos_c.load(std::sync::atomic::Ordering::Relaxed);
            let remaining = samples_c.len().saturating_sub(p);
            let to_write = output.len().min(remaining);
            output[..to_write].copy_from_slice(&samples_c[p..p + to_write]);
            for s in output[to_write..].iter_mut() {
                *s = 0.0;
            }
            pos_c.fetch_add(to_write, std::sync::atomic::Ordering::Relaxed);
            if to_write == 0 && !sent {
                sent = true;
                let _ = done_tx.send(());
            }
        },
        |err| tracing::error!("audio playback error: {err}"),
        None,
    )?;

    stream.play().context("start playback stream")?;
    let _ = done_rx.recv_timeout(Duration::from_secs(60));
    Ok(())
}

// ─── Internal helpers ─────────────────────────────────────────────────────────

fn build_input_stream(
    device: &cpal::Device,
    config: &cpal::SupportedStreamConfig,
    buf: Arc<Mutex<Vec<f32>>>,
) -> Result<cpal::Stream> {
    let stream = match config.sample_format() {
        SampleFormat::F32 => {
            let b = buf.clone();
            device.build_input_stream(
                &config.clone().into(),
                move |data: &[f32], _| {
                    b.lock().unwrap().extend_from_slice(data);
                },
                |err| tracing::error!("audio capture error: {err}"),
                None,
            )?
        }
        SampleFormat::I16 => {
            let b = buf.clone();
            device.build_input_stream(
                &config.clone().into(),
                move |data: &[i16], _| {
                    let floats: Vec<f32> =
                        data.iter().map(|&s| s as f32 / i16::MAX as f32).collect();
                    b.lock().unwrap().extend_from_slice(&floats);
                },
                |err| tracing::error!("audio capture error: {err}"),
                None,
            )?
        }
        SampleFormat::U16 => {
            let b = buf.clone();
            device.build_input_stream(
                &config.clone().into(),
                move |data: &[u16], _| {
                    let floats: Vec<f32> = data
                        .iter()
                        .map(|&s| (s as f32 / u16::MAX as f32) * 2.0 - 1.0)
                        .collect();
                    b.lock().unwrap().extend_from_slice(&floats);
                },
                |err| tracing::error!("audio capture error: {err}"),
                None,
            )?
        }
        fmt => anyhow::bail!("unsupported input sample format: {fmt:?}"),
    };
    Ok(stream)
}

/// Naive linear-interpolation resample from `src_rate` Hz to 16 000 Hz.
/// Suitable for STT preprocessing (not for high-quality audio).
pub fn resample_to_16k(samples: &[f32], src_rate: u32) -> Vec<f32> {
    if src_rate == 16_000 || samples.is_empty() {
        return samples.to_vec();
    }
    let ratio = src_rate as f64 / 16_000.0;
    let out_len = (samples.len() as f64 / ratio) as usize;
    (0..out_len)
        .map(|i| {
            let src_pos = i as f64 * ratio;
            let lo = src_pos.floor() as usize;
            let hi = (lo + 1).min(samples.len() - 1);
            let frac = src_pos.fract() as f32;
            samples[lo] * (1.0 - frac) + samples[hi] * frac
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_devices_does_not_panic() {
        // In CI (no sound card) this may return an empty list; it must
        // not panic or return an error.
        let result = list_input_devices();
        assert!(result.is_ok(), "list_input_devices returned error: {result:?}");
    }

    #[test]
    fn resample_passthrough_when_already_16k() {
        let input = vec![0.1_f32, 0.2, 0.3];
        assert_eq!(resample_to_16k(&input, 16_000), input);
    }

    #[test]
    fn resample_empty_is_empty() {
        assert!(resample_to_16k(&[], 44_100).is_empty());
    }

    #[test]
    fn resample_halves_length_at_32k() {
        let input: Vec<f32> = (0..64).map(|i| i as f32).collect();
        let out = resample_to_16k(&input, 32_000);
        assert_eq!(out.len(), 32);
    }

    #[test]
    fn resample_values_interpolate_correctly() {
        // Two samples [0.0, 1.0] at 32kHz downsampled to 16kHz → 1 output.
        // At output index 0: src_pos = 0.0*2.0 = 0.0 → lo=0, hi=1, frac=0.0 → 0.0.
        let input = vec![0.0_f32, 1.0];
        let out = resample_to_16k(&input, 32_000);
        assert_eq!(out.len(), 1);
        assert!((out[0] - 0.0).abs() < 1e-6);
    }
}

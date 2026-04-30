//! Audio I/O — cpal-based mic capture (mono i16 @ 16 kHz for whisper)
//! and speaker playback (lands in M1.5.1).

pub mod capture_worker;
pub mod playback;
pub mod ptt;
pub mod ring_buffer;

use anyhow::{Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use parking_lot::Mutex;
use std::sync::Arc;

/// Sample rate whisper.cpp expects (mono i16).
pub const STT_SAMPLE_RATE_HZ: u32 = 16_000;

/// Default playback sample rate. Kokoro outputs 24 kHz; we resample
/// to 22.05 kHz for slightly faster synthesis at imperceptible quality
/// loss (per first-byte tuning notes in roadmap §4.1).
pub const TTS_PLAYBACK_SAMPLE_RATE_HZ: u32 = 22_050;

pub struct CaptureBuffer {
    /// Resampled to STT_SAMPLE_RATE_HZ, mono, i16.
    samples: Mutex<Vec<i16>>,
}

impl CaptureBuffer {
    pub fn new() -> Self {
        Self {
            samples: Mutex::new(vec![]),
        }
    }

    /// Take all buffered samples, leaving the buffer empty.
    pub fn drain(&self) -> Vec<i16> {
        std::mem::take(&mut self.samples.lock())
    }

    pub fn len(&self) -> usize {
        self.samples.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.lock().is_empty()
    }

    fn extend(&self, more: &[i16]) {
        self.samples.lock().extend_from_slice(more);
    }
}

impl Default for CaptureBuffer {
    fn default() -> Self {
        Self::new()
    }
}

/// Active capture stream. Dropping this stops capture.
pub struct CaptureHandle {
    _stream: cpal::Stream,
    pub buffer: Arc<CaptureBuffer>,
}

/// Open the default input device, resample to 16 kHz mono i16, and
/// stream into the returned `CaptureBuffer`. Drop the handle to stop.
///
/// Errors if no input device is available or the device's default
/// config uses a sample format we don't yet handle (v1: f32 only;
/// extending to i16/u16 is straightforward and lands when a real
/// device demands it).
pub fn start_capture() -> Result<CaptureHandle> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| anyhow!("no input device available"))?;
    let config = device.default_input_config()?;
    let sample_rate = config.sample_rate().0;
    let channels = config.channels() as usize;
    let buffer = Arc::new(CaptureBuffer::new());
    let buffer2 = buffer.clone();

    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &config.into(),
            move |data: &[f32], _| {
                let mono = downmix_to_mono_f32(data, channels);
                let resampled = resample_simple_to_16k(&mono, sample_rate);
                let i16_samples: Vec<i16> = resampled
                    .iter()
                    .map(|&s| (s * i16::MAX as f32).clamp(i16::MIN as f32, i16::MAX as f32) as i16)
                    .collect();
                buffer2.extend(&i16_samples);
            },
            |e| tracing::warn!(error = %e, "capture stream error"),
            None,
        )?,
        other => return Err(anyhow!("unsupported input sample format: {other:?}")),
    };
    stream.play()?;
    Ok(CaptureHandle {
        _stream: stream,
        buffer,
    })
}

fn downmix_to_mono_f32(data: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return data.to_vec();
    }
    data.chunks(channels)
        .map(|c| c.iter().sum::<f32>() / channels as f32)
        .collect()
}

/// Naive linear-interpolation resampler. Sufficient for whisper input
/// (it does its own internal resampling); production should use
/// `rubato` (M1.4.4 hardening — not v1 critical path).
fn resample_simple_to_16k(samples: &[f32], from_rate: u32) -> Vec<f32> {
    if from_rate == STT_SAMPLE_RATE_HZ {
        return samples.to_vec();
    }
    if samples.is_empty() {
        return vec![];
    }
    let ratio = STT_SAMPLE_RATE_HZ as f32 / from_rate as f32;
    let out_len = (samples.len() as f32 * ratio) as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let src = i as f32 / ratio;
        let i0 = (src as usize).min(samples.len() - 1);
        let i1 = (i0 + 1).min(samples.len() - 1);
        let frac = src - i0 as f32;
        out.push(samples[i0] * (1.0 - frac) + samples[i1] * frac);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_buffer_drain_round_trips() {
        let b = CaptureBuffer::new();
        assert!(b.is_empty());
        b.extend(&[1, 2, 3]);
        assert_eq!(b.len(), 3);
        let got = b.drain();
        assert_eq!(got, vec![1i16, 2, 3]);
        assert!(b.is_empty());
    }

    #[test]
    fn downmix_passes_through_mono() {
        let got = downmix_to_mono_f32(&[1.0, 2.0, 3.0], 1);
        assert_eq!(got, vec![1.0, 2.0, 3.0]);
    }

    #[test]
    fn downmix_averages_stereo_channels() {
        // Two stereo frames: [L=1, R=3], [L=2, R=4] → [(1+3)/2, (2+4)/2] = [2.0, 3.0]
        let got = downmix_to_mono_f32(&[1.0, 3.0, 2.0, 4.0], 2);
        assert_eq!(got, vec![2.0, 3.0]);
    }

    #[test]
    fn resample_passes_through_when_already_at_16k() {
        let got = resample_simple_to_16k(&[1.0, 2.0, 3.0], 16_000);
        assert_eq!(got, vec![1.0, 2.0, 3.0]);
    }

    #[test]
    fn resample_downsample_48k_to_16k_yields_third_length() {
        // 48k → 16k = 1/3 ratio; 90 samples → 30 samples.
        let input: Vec<f32> = (0..90).map(|i| i as f32).collect();
        let got = resample_simple_to_16k(&input, 48_000);
        assert_eq!(got.len(), 30);
        // First sample matches input[0]; last sample within [last-1, last]
        assert!((got[0] - 0.0).abs() < 0.5);
    }

    #[test]
    fn resample_handles_empty_input() {
        let got = resample_simple_to_16k(&[], 48_000);
        assert!(got.is_empty());
    }
}

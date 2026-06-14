//! Real-model TTS smoke test (ignored by default — needs `~/.mur/models`).
//!
//! Run after `mur agent voice <name> download` (needs the `tts` feature):
//!   cargo test -p mur-agent-runtime --features tts --test tts_real_model_smoke -- --ignored --nocapture
//!
//! Verifies the v0.19 ONNX I/O contract (`input_ids`/`style`/`speed` → `waveform`)
//! and the locally-built `kokoro-voices.bin`, and that synthesis yields non-silent PCM.
#![cfg(feature = "tts")]

use mur_agent_runtime::voice::tts::KokoroTts;
use mur_agent_runtime::voice::types::VoiceModelPaths;
use mur_common::agent::VoiceId;

#[test]
#[ignore = "requires downloaded models in ~/.mur/models"]
fn synthesizes_non_silent_audio() {
    let home = dirs::home_dir().unwrap().join(".mur");
    let paths = VoiceModelPaths::from_mur_root(&home);
    assert!(
        paths.all_present(),
        "models missing: run `mur agent voice mur download`"
    );

    let tts = KokoroTts::new(&paths.kokoro_onnx, &paths.kokoro_voices, VoiceId::AfHeart)
        .expect("load Kokoro TTS");
    let samples = tts
        .synthesize("Hello, this is a voice test.")
        .expect("synthesize");

    assert!(!samples.is_empty(), "no audio produced");
    let rms = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt();
    let peak = samples.iter().fold(0f32, |m, s| m.max(s.abs()));
    eprintln!("samples={} rms={rms:.5} peak={peak:.5}", samples.len());
    assert!(rms > 1e-3, "audio is effectively silent (rms={rms})");

    // Dump a listenable WAV when MUR_TTS_WAV is set (manual evidence).
    if let Ok(path) = std::env::var("MUR_TTS_WAV") {
        write_wav_16k(&path, &samples, 24_000);
        eprintln!("wrote {path}");
    }
}

/// Minimal 16-bit PCM WAV writer (mono).
fn write_wav_16k(path: &str, samples: &[f32], sample_rate: u32) {
    use std::io::Write as _;
    let n = samples.len() as u32;
    let data_len = n * 2;
    let mut f = std::fs::File::create(path).unwrap();
    let mut h = Vec::new();
    h.extend_from_slice(b"RIFF");
    h.extend_from_slice(&(36 + data_len).to_le_bytes());
    h.extend_from_slice(b"WAVE");
    h.extend_from_slice(b"fmt ");
    h.extend_from_slice(&16u32.to_le_bytes());
    h.extend_from_slice(&1u16.to_le_bytes()); // PCM
    h.extend_from_slice(&1u16.to_le_bytes()); // mono
    h.extend_from_slice(&sample_rate.to_le_bytes());
    h.extend_from_slice(&(sample_rate * 2).to_le_bytes()); // byte rate
    h.extend_from_slice(&2u16.to_le_bytes()); // block align
    h.extend_from_slice(&16u16.to_le_bytes()); // bits
    h.extend_from_slice(b"data");
    h.extend_from_slice(&data_len.to_le_bytes());
    f.write_all(&h).unwrap();
    for s in samples {
        let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
        f.write_all(&v.to_le_bytes()).unwrap();
    }
}

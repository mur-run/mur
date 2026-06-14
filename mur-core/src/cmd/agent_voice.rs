//! `mur agent voice enable/disable/download` — manage voice I/O (TTS + STT) for an agent.
//!
//! Mutates the agent's `profile.yaml` (`voice` block). The runtime uses the
//! same shape: when `voice.enabled = true`, the supervisor initialises the
//! Kokoro TTS engine and the whisper.cpp STT pipeline.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use futures::StreamExt as _;
use mur_agent_runtime::voice::download::ModelSpec;
use mur_agent_runtime::voice::types::{
    KOKORO_ONNX_SPEC, KOKORO_VOICE_SOURCES, N_VOICES, VOICES_BIN_LEN, VoiceModelPaths, WHISPER_SPEC,
};
use mur_common::agent::VoiceId;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt as _;

use super::agent::{load_profile_for_edit, save_profile};

// ─── commands ────────────────────────────────────────────────────────────────

/// Enable voice I/O (TTS + STT) for the named agent.
///
/// Optionally sets the Kokoro voice ID. If none is supplied the current
/// `voice_id` in the profile is kept (or the default `af_heart` if this is
/// the first enable).
pub fn cmd_voice_enable(name: &str, voice_id: Option<&str>) -> Result<()> {
    let (path, mut profile) = load_profile_for_edit(name)?;

    profile.voice.enabled = true;

    if let Some(id_str) = voice_id {
        profile.voice.voice_id = id_str.parse::<VoiceId>()?;
    }

    save_profile(&path, &mut profile)?;

    println!("Voice I/O enabled for agent '{name}'.");
    println!("  Voice ID : {}", profile.voice.voice_id.as_str());
    println!(
        "  Hint: run `mur agent voice {name} download` to fetch model weights (~1.4 GB) before starting the agent."
    );
    Ok(())
}

/// Disable voice I/O for the named agent.
pub fn cmd_voice_disable(name: &str) -> Result<()> {
    let (path, mut profile) = load_profile_for_edit(name)?;
    profile.voice.enabled = false;
    save_profile(&path, &mut profile)?;
    println!("Voice I/O disabled for agent '{name}'.");
    Ok(())
}

/// Download + verify the voice model weights (whisper STT + Kokoro TTS).
///
/// Models live under `~/.mur/models/` and are shared by all agents (`name` is
/// only used in messages). Each file is streamed from Hugging Face and verified
/// against a pinned SHA-256. The Kokoro voice style matrix (`kokoro-voices.bin`)
/// is built locally by concatenating the five per-voice tensors in
/// `VoiceId::style_index()` order — see `voice::types::KOKORO_VOICE_SOURCES`.
pub async fn cmd_voice_download(name: &str) -> Result<()> {
    let home = crate::paths::mur_root(None);
    println!("Downloading voice models for agent '{name}' (~1.4 GB; SHA-256 verified)…");

    fetch_verify(&home, &WHISPER_SPEC).await?;
    fetch_verify(&home, &KOKORO_ONNX_SPEC).await?;
    for spec in KOKORO_VOICE_SOURCES.iter() {
        fetch_verify(&home, spec).await?;
    }
    build_voices_bin(&home)?;

    let paths = VoiceModelPaths::from_mur_root(&home);
    println!("✓ Voice models ready:");
    println!("    {}", paths.whisper.display());
    println!("    {}", paths.kokoro_onnx.display());
    println!("    {}", paths.kokoro_voices.display());
    println!("Restart the daemon (`mur daemon stop && mur daemon start`) to load the models.");
    Ok(())
}

/// Stream `spec.url` to `~/.mur/models/<subdir>/<name>`, hashing as we go, and
/// atomically rename once the SHA-256 matches. Skips the download when a cached
/// file already matches.
async fn fetch_verify(home: &Path, spec: &ModelSpec) -> Result<PathBuf> {
    let dir = home.join("models").join(spec.subdir);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("create models dir {}", dir.display()))?;
    let final_path = dir.join(spec.name);

    if final_path.exists() && sha256_file(&final_path)? == spec.sha256 {
        println!("  ✓ {} (cached)", spec.name);
        return Ok(final_path);
    }

    print!("  ↓ {} ", spec.name);
    std::io::stdout().flush().ok();

    let resp = reqwest::get(spec.url)
        .await
        .with_context(|| format!("GET {}", spec.url))?
        .error_for_status()
        .with_context(|| format!("fetch {}", spec.name))?;
    let total = resp.content_length().unwrap_or(0);

    let tmp_path = final_path.with_extension("tmp");
    let mut file = tokio::fs::File::create(&tmp_path)
        .await
        .with_context(|| format!("create {}", tmp_path.display()))?;
    let mut hasher = Sha256::new();
    let mut downloaded: u64 = 0;
    let mut last_pct: u64 = 0;
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("download stream")?;
        file.write_all(&chunk).await.context("write chunk")?;
        hasher.update(&chunk);
        downloaded += chunk.len() as u64;
        if let Some(pct) = (downloaded * 100).checked_div(total)
            && pct >= last_pct + 10
        {
            print!("{pct}% ");
            std::io::stdout().flush().ok();
            last_pct = pct;
        }
    }
    file.flush().await.ok();
    drop(file);

    let actual = format!("{:x}", hasher.finalize());
    if actual != spec.sha256 {
        std::fs::remove_file(&tmp_path).ok();
        bail!(
            "sha256 mismatch for {}: expected {}, got {}",
            spec.name,
            spec.sha256,
            actual
        );
    }
    std::fs::rename(&tmp_path, &final_path)
        .with_context(|| format!("rename {} → {}", tmp_path.display(), final_path.display()))?;
    println!("done");
    Ok(final_path)
}

/// Concatenate the five downloaded per-voice tensors into `kokoro-voices.bin`
/// (`N_VOICES × 512 × 256` f32), written atomically.
fn build_voices_bin(home: &Path) -> Result<()> {
    let voices_dir = home.join("models").join("kokoro").join("voices");
    let per_voice_len = VOICES_BIN_LEN / N_VOICES;
    let mut out: Vec<u8> = Vec::with_capacity(VOICES_BIN_LEN);

    for spec in KOKORO_VOICE_SOURCES.iter() {
        let p = voices_dir.join(spec.name);
        let bytes = std::fs::read(&p).with_context(|| format!("read {}", p.display()))?;
        if bytes.len() != per_voice_len {
            bail!(
                "voice source {} has unexpected size {} (expected {per_voice_len})",
                spec.name,
                bytes.len()
            );
        }
        out.extend_from_slice(&bytes);
    }
    if out.len() != VOICES_BIN_LEN {
        bail!(
            "assembled voices.bin is {} bytes (expected {VOICES_BIN_LEN})",
            out.len()
        );
    }

    let dest = VoiceModelPaths::from_mur_root(home).kokoro_voices;
    let tmp = dest.with_extension("tmp");
    std::fs::write(&tmp, &out).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, &dest)
        .with_context(|| format!("rename {} → {}", tmp.display(), dest.display()))?;
    println!("  ✓ kokoro-voices.bin (built from {N_VOICES} voices)");
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut f = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut f, &mut hasher).with_context(|| format!("hash {}", path.display()))?;
    Ok(format!("{:x}", hasher.finalize()))
}

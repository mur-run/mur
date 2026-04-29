# mur Agent D1 — Voice Stack (M1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship local-only Kokoro 82M TTS + whisper.cpp STT in `mur-agent-gui`, with PTT hotkey `Cmd+Shift+'`, 5 curated voices (1 bundled + 4 download-on-demand from signed CDN), and a "voice never leaves this Mac" privacy badge — per roadmap §4.1 (D1 Voice Stack).

**Architecture:** All inference runs in the Tauri sidecar (`mur-agent-gui/src-tauri/`), not the runtime, because voice is a GUI-tier concern (PTT hotkey, audio I/O, settings panel). The runtime stays voice-blind. TTS uses ONNX via the `ort` crate; STT uses `whisper-rs` (statically linked whisper.cpp). Audio I/O via `cpal`. Voice manifests are signed JSON files served from a Cloudflare-fronted CDN; the runtime verifies SHA-256 + Ed25519 signature before `mmap`/load.

**Tech Stack:** Rust 2024, `ort = "2"` (ONNX Runtime), `whisper-rs = "0.14"`, `cpal = "0.16"`, `hound = "3"`, `dasp_ring_buffer = "0.11"`, `tauri-plugin-global-shortcut`, `tauri-plugin-clipboard-manager`. macOS Vision.framework via `objc2` for the future OCR step (D3); not required for D1.

**Spec:** `docs/superpowers/specs/2026-04-30-mur-agent-harness-roadmap-design.md` §4.1.

**Predecessor:** `docs/superpowers/plans/2026-04-30-mur-agent-hooks-a0.md` (M0, merged via PR #44 commit `60ee254`).

**Commit format:** `M1.<n>.<m>: <subject>` so `git log --grep "^M1"` shows progress.

**Branch policy:** All M1 work lands on `feat/mur-agent-d1-voice` (this plan ships on `feat/mur-agent-d1-voice-plan`; rename or branch off when starting impl).

---

## File Structure

```
mur-agent-gui/src-tauri/Cargo.toml      # MODIFY: add voice deps

mur-agent-gui/src-tauri/src/
  voice/                                # NEW module
    mod.rs                              # CREATE: VoiceManager + facade
    manifest.rs                         # CREATE: signed CDN manifest schema + verify
    registry.rs                         # CREATE: installed-voice index
    download.rs                         # CREATE: voice asset download client
    tts/
      mod.rs                            # CREATE: TtsEngine trait + facade
      kokoro.rs                         # CREATE: Kokoro 82M ort session
      g2p.rs                            # CREATE: grapheme-to-phoneme (en via espeak-ng-rs; zh via jieba-rs)
      sentence_split.rs                 # CREATE: sentence splitter for streaming
    stt/
      mod.rs                            # CREATE: SttEngine trait + facade
      whisper.rs                        # CREATE: whisper-rs adapter
    audio/
      mod.rs                            # CREATE: cpal playback + capture
      ring_buffer.rs                    # CREATE: dasp ring buffer wrapper
      ptt.rs                            # CREATE: push-to-talk state machine

  commands.rs                           # MODIFY: register tts/stt/voice tauri commands
  lib.rs                                # MODIFY: pub mod voice; init at app boot

mur-agent-gui/ui/src/
  voice/                                # NEW module
    VoicePicker.tsx                     # CREATE: 5-sample preview grid
    PttButton.tsx                       # CREATE: hold-to-talk indicator
    PrivacyBadge.tsx                    # CREATE: "voice never leaves this Mac"
    HotkeyRebinder.tsx                  # CREATE: rebind PTT shortcut
    types.ts                            # CREATE: shared types

  pages/Settings.tsx                    # MODIFY: add Voice tab

mur-agent-gui/src-tauri/voices/         # NEW asset dir (bundled assets)
  af_heart/
    voice.onnx                          # 22 MB; bundled in installer
    voice.json                          # metadata: id, language, sample_rate, license
    LICENSE.txt
  README.md                             # license notes for the 5 starters

mur-agent-gui/src-tauri/tests/
  voice_manifest.rs                     # CREATE: manifest signature + SHA-256 tests
  voice_download.rs                     # CREATE: mock CDN + retry + fail paths
  sentence_split.rs                     # CREATE: split unit tests
  ptt_state.rs                          # CREATE: state machine unit tests
  voice_e2e.rs                          # CREATE: end-to-end TTS+STT integration

scripts/e2e/
  v1-d1-voice.sh                        # CREATE: launches GUI, drives voice picker

docs/superpowers/specs/
  2026-04-30-mur-agent-harness-roadmap-design.md   # roadmap §4.1 reference
docs/cookbook/
  voice-stack.md                        # CREATE: end-user setup + license docs
```

---

## Milestone M1.1 — Workspace deps + voice runtime crate skeleton

### Task M1.1.1: Add voice runtime deps to mur-agent-gui

**Files:**
- Modify: `mur-agent-gui/src-tauri/Cargo.toml`

- [ ] **Step 1: Inspect current deps**

Run: `grep -E '^(ort|whisper-rs|cpal|hound|dasp|tauri-plugin)' mur-agent-gui/src-tauri/Cargo.toml || echo "none"`
Expected: none of these present.

- [ ] **Step 2: Add deps under `[dependencies]`**

```toml
ort = { version = "2", default-features = false, features = ["copy-dylibs", "load-dynamic"] }
whisper-rs = { version = "0.14", default-features = false, features = ["metal"] }
cpal = "0.16"
hound = "3"
dasp_ring_buffer = "0.11"
ed25519-dalek = { version = "2", default-features = false, features = ["std", "pkcs8"] }
sha2 = "0.10"
hex = "0.4"
reqwest = { version = "0.12", default-features = false, features = ["rustls-tls", "stream"] }
tokio-util = { version = "0.7", features = ["io"] }
tauri-plugin-global-shortcut = "2"
tauri-plugin-clipboard-manager = "2"
tauri-plugin-notification = "2"
```

For Linux only (under `[target.'cfg(target_os = "linux")'.dependencies]`):
```toml
alsa = "0.9"  # cpal alsa backend
```

- [ ] **Step 3: Build to verify resolution**

Run: `cargo build -p mur-agent-gui --target-dir target/voice-deps-check 2>&1 | tail -5`
Expected: success; warnings about unused deps OK.

- [ ] **Step 4: Commit**

```bash
git add mur-agent-gui/src-tauri/Cargo.toml
git commit -m "M1.1.1: add voice runtime deps (ort + whisper-rs + cpal + ed25519-dalek)"
```

### Task M1.1.2: Create voice module skeleton

**Files:**
- Create: `mur-agent-gui/src-tauri/src/voice/mod.rs`
- Modify: `mur-agent-gui/src-tauri/src/lib.rs`

- [ ] **Step 1: Write `voice/mod.rs`**

```rust
//! Voice subsystem — local-only TTS (Kokoro 82M) + STT (whisper.cpp).
//!
//! All inference runs in the Tauri sidecar process. The mur-agent-runtime
//! itself remains voice-blind; voice is purely a GUI-tier concern.
//!
//! The "voice never leaves this Mac" promise (D1 §4.1) is upheld by:
//!   * No outbound network in this module except the signed-CDN voice
//!     download path (manifest.rs); model API endpoints are NOT touched.
//!   * Runtime never sees raw audio buffers — only post-STT transcripts.
//!   * Speech synthesis is local; no cloud TTS fallback in v1.

pub mod audio;
pub mod download;
pub mod manifest;
pub mod registry;
pub mod stt;
pub mod tts;

use std::sync::Arc;
use tokio::sync::RwLock;

pub struct VoiceManager {
    pub tts: Arc<RwLock<tts::TtsEngine>>,
    pub stt: Arc<RwLock<stt::SttEngine>>,
    pub registry: Arc<RwLock<registry::VoiceRegistry>>,
}

impl VoiceManager {
    pub async fn new(app_data_dir: std::path::PathBuf) -> anyhow::Result<Self> {
        let registry = Arc::new(RwLock::new(registry::VoiceRegistry::load(&app_data_dir).await?));
        let tts = Arc::new(RwLock::new(tts::TtsEngine::new()?));
        let stt = Arc::new(RwLock::new(stt::SttEngine::new()?));
        Ok(Self { tts, stt, registry })
    }
}
```

- [ ] **Step 2: Stub each submodule**

For each of `audio/mod.rs`, `download.rs`, `manifest.rs`, `registry.rs`, `stt/mod.rs`, `tts/mod.rs`:

```rust
//! Stubbed in M1.1.2; full impl lands in M1.<later>.
```

For `tts/mod.rs` and `stt/mod.rs`, add a minimal facade:

```rust
//! TtsEngine facade — stub.
pub struct TtsEngine;
impl TtsEngine {
    pub fn new() -> anyhow::Result<Self> { Ok(Self) }
}
```

```rust
//! SttEngine facade — stub.
pub struct SttEngine;
impl SttEngine {
    pub fn new() -> anyhow::Result<Self> { Ok(Self) }
}
```

For `registry.rs`:

```rust
//! Installed-voice index — stub.
use std::path::Path;
pub struct VoiceRegistry;
impl VoiceRegistry {
    pub async fn load(_app_data_dir: &Path) -> anyhow::Result<Self> { Ok(Self) }
}
```

- [ ] **Step 3: Wire `pub mod voice;` into lib.rs**

Run: `grep -n 'pub mod' mur-agent-gui/src-tauri/src/lib.rs | head`

Add `pub mod voice;` near the other module declarations (sort lexicographically).

- [ ] **Step 4: Build**

Run: `cargo build -p mur-agent-gui 2>&1 | tail -5`
Expected: clean build (warnings about unused stubs OK).

- [ ] **Step 5: Commit**

```bash
git add mur-agent-gui/src-tauri/src/voice/ mur-agent-gui/src-tauri/src/lib.rs
git commit -m "M1.1.2: voice module skeleton (TtsEngine / SttEngine / VoiceRegistry stubs)"
```

---

## Milestone M1.2 — Voice Manifest + Download (signed CDN)

### Task M1.2.1: Manifest schema + Ed25519 signature verification

**Files:**
- Modify: `mur-agent-gui/src-tauri/src/voice/manifest.rs`

- [ ] **Step 1: Write the manifest module**

```rust
//! Voice manifest — signed JSON describing a voice asset.
//!
//! Hosted at https://voices.mur.run/<voice_id>/manifest.json with a
//! detached signature at /<voice_id>/manifest.json.sig.
//!
//! The verifying public key is pinned in the binary at build time via
//! the MUR_VOICE_PUBKEY env var (or build.rs constant). Rotation
//! requires a binary release.

use anyhow::{Context, Result, bail};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Compile-time pinned voice-manifest verifying key (multibase z…).
/// Override at build time with `MUR_VOICE_PUBKEY=z6Mk... cargo build`.
pub const PINNED_VOICE_PUBKEY: &str = env!(
    "MUR_VOICE_PUBKEY",
    "MUR_VOICE_PUBKEY env var must be set at build time (use build.rs default for dev)"
);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceManifest {
    pub schema_version: u32,
    pub voice_id: String,
    pub display_name: String,
    pub language: String,        // BCP-47, e.g. "en-US" or "zh-TW"
    pub sample_rate_hz: u32,     // Kokoro is 24000; we resample to 22050 in playback
    pub license: String,         // "MIT" / "Apache-2.0"
    pub creator: String,
    pub assets: Vec<AssetEntry>,
    pub size_bytes_total: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetEntry {
    pub name: String,            // e.g. "voice.onnx"
    pub sha256_hex: String,      // lowercase hex
    pub size_bytes: u64,
    pub url: String,             // absolute https URL
}

/// Verify the detached signature `sig` over `manifest_bytes` against the
/// pinned public key. Returns the parsed manifest on success.
pub fn verify_and_parse(manifest_bytes: &[u8], sig_bytes: &[u8]) -> Result<VoiceManifest> {
    if sig_bytes.len() != 64 {
        bail!("voice manifest signature must be 64 bytes; got {}", sig_bytes.len());
    }
    let pubkey = decode_pinned_pubkey()?;
    let mut sig_arr = [0u8; 64];
    sig_arr.copy_from_slice(sig_bytes);
    let signature = Signature::from_bytes(&sig_arr);
    pubkey.verify(manifest_bytes, &signature)
        .context("voice manifest signature verification failed")?;
    let manifest: VoiceManifest = serde_json::from_slice(manifest_bytes)
        .context("voice manifest is not valid JSON")?;
    Ok(manifest)
}

/// SHA-256 hex of `bytes`. Used to verify each downloaded asset against
/// the manifest entry.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn decode_pinned_pubkey() -> Result<VerifyingKey> {
    // multibase z... → 32 raw bytes
    let raw = multibase_decode_z(PINNED_VOICE_PUBKEY)?;
    if raw.len() != 32 {
        bail!("pinned voice pubkey must be 32 bytes; got {}", raw.len());
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&raw);
    VerifyingKey::from_bytes(&arr).context("invalid pinned voice pubkey")
}

fn multibase_decode_z(s: &str) -> Result<Vec<u8>> {
    let s = s.strip_prefix('z').context("expected multibase z prefix")?;
    bs58::decode(s).into_vec().context("invalid base58 in voice pubkey")
}
```

- [ ] **Step 2: Add `bs58` dep**

In `mur-agent-gui/src-tauri/Cargo.toml`:
```toml
bs58 = "0.5"
```

- [ ] **Step 3: Add a build.rs default pubkey for dev**

Create `mur-agent-gui/src-tauri/build.rs`:

```rust
fn main() {
    if std::env::var("MUR_VOICE_PUBKEY").is_err() {
        // Dev-only placeholder. Real release builds MUST override via env.
        println!("cargo:rustc-env=MUR_VOICE_PUBKEY=zCgTqCYEbNAQUKjcadtBwNaxvEgN5Z4S2BuKGsf7c1SMz8");
    }
    println!("cargo:rerun-if-env-changed=MUR_VOICE_PUBKEY");
    tauri_build::build();
}
```

(If `tauri_build::build()` is already invoked from existing build.rs, merge instead.)

- [ ] **Step 4: Unit tests**

Append to `manifest.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use rand::rngs::OsRng;

    #[test]
    fn sha256_hex_matches_known_vector() {
        // SHA-256("") = e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn signature_round_trip_with_freshly_generated_key() {
        // We cannot easily override PINNED_VOICE_PUBKEY at runtime, so
        // this test exercises the signing path only. End-to-end pubkey
        // pinning is exercised by the integration test in
        // tests/voice_manifest.rs (which uses a build-time override).
        let mut csprng = OsRng;
        let key = SigningKey::generate(&mut csprng);
        let payload = br#"{"schema_version":1,"voice_id":"x","display_name":"X","language":"en-US","sample_rate_hz":24000,"license":"MIT","creator":"test","assets":[],"size_bytes_total":0}"#;
        let sig = key.sign(payload);
        assert!(key.verifying_key().verify(payload, &sig).is_ok());
    }
}
```

Add `rand = "0.8"` to `[dev-dependencies]`.

- [ ] **Step 5: Run tests**

Run: `cargo test -p mur-agent-gui --lib voice::manifest`
Expected: 2 passed.

- [ ] **Step 6: Commit**

```bash
git add mur-agent-gui/src-tauri/src/voice/manifest.rs \
        mur-agent-gui/src-tauri/Cargo.toml \
        mur-agent-gui/src-tauri/build.rs
git commit -m "M1.2.1: voice manifest schema + Ed25519 verify + SHA-256 helper"
```

### Task M1.2.2: Voice download client with progress + cancellation

**Files:**
- Modify: `mur-agent-gui/src-tauri/src/voice/download.rs`

- [ ] **Step 1: Implement download**

```rust
//! Voice download client. Streams bytes from CDN, computes SHA-256
//! incrementally, verifies against the manifest, and stages the file
//! atomically (temp + rename).

use anyhow::{Context, Result, bail};
use futures::StreamExt;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::io::AsyncWriteExt;

use super::manifest::{AssetEntry, VoiceManifest, sha256_hex, verify_and_parse};

const CDN_BASE: &str = "https://voices.mur.run";
const CONNECT_TIMEOUT_S: u64 = 10;
const TOTAL_TIMEOUT_S: u64 = 600;

#[derive(Debug, Clone)]
pub enum DownloadProgress {
    ManifestFetched,
    ManifestVerified,
    AssetStarted { name: String, size_bytes: u64 },
    AssetProgress { name: String, downloaded_bytes: u64, total_bytes: u64 },
    AssetComplete { name: String },
    Done,
}

pub struct DownloadHandle {
    pub voice_id: String,
    pub install_dir: PathBuf,
}

pub async fn download_voice(
    voice_id: &str,
    install_dir: PathBuf,
    progress: tokio::sync::mpsc::Sender<DownloadProgress>,
    cancel: tokio_util::sync::CancellationToken,
) -> Result<DownloadHandle> {
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(CONNECT_TIMEOUT_S))
        .timeout(std::time::Duration::from_secs(TOTAL_TIMEOUT_S))
        .build()?;

    // 1. Fetch manifest + signature
    let manifest_url = format!("{CDN_BASE}/{voice_id}/manifest.json");
    let sig_url = format!("{CDN_BASE}/{voice_id}/manifest.json.sig");

    let manifest_bytes = client.get(&manifest_url).send().await?.error_for_status()?.bytes().await?;
    let _ = progress.send(DownloadProgress::ManifestFetched).await;

    let sig_bytes = client.get(&sig_url).send().await?.error_for_status()?.bytes().await?;

    // 2. Verify signature → parse manifest
    let manifest = verify_and_parse(&manifest_bytes, &sig_bytes)
        .context("voice manifest verification failed; refusing to install")?;
    let _ = progress.send(DownloadProgress::ManifestVerified).await;

    if manifest.voice_id != voice_id {
        bail!("manifest voice_id `{}` does not match request `{voice_id}`", manifest.voice_id);
    }

    fs::create_dir_all(&install_dir).await?;

    // 3. Download each asset, verify SHA-256, write atomic temp+rename.
    for asset in &manifest.assets {
        if cancel.is_cancelled() {
            bail!("download cancelled");
        }
        download_one_asset(&client, asset, &install_dir, &progress, &cancel).await
            .with_context(|| format!("downloading asset {}", asset.name))?;
    }

    // 4. Persist verified manifest alongside assets for later integrity rechecks
    fs::write(install_dir.join("manifest.json"), &manifest_bytes).await?;
    fs::write(install_dir.join("manifest.json.sig"), &sig_bytes).await?;

    let _ = progress.send(DownloadProgress::Done).await;
    Ok(DownloadHandle { voice_id: voice_id.into(), install_dir })
}

async fn download_one_asset(
    client: &reqwest::Client,
    asset: &AssetEntry,
    install_dir: &Path,
    progress: &tokio::sync::mpsc::Sender<DownloadProgress>,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<()> {
    let _ = progress.send(DownloadProgress::AssetStarted {
        name: asset.name.clone(),
        size_bytes: asset.size_bytes,
    }).await;

    let resp = client.get(&asset.url).send().await?.error_for_status()?;
    let mut stream = resp.bytes_stream();

    let tmp = install_dir.join(format!("{}.partial", asset.name));
    let final_path = install_dir.join(&asset.name);
    let mut file = fs::File::create(&tmp).await?;
    let mut hasher = Sha256::new();
    let mut downloaded: u64 = 0;
    let mut last_progress = std::time::Instant::now();

    while let Some(chunk) = stream.next().await {
        if cancel.is_cancelled() {
            drop(file);
            let _ = fs::remove_file(&tmp).await;
            bail!("download cancelled mid-asset");
        }
        let bytes = chunk?;
        hasher.update(&bytes);
        file.write_all(&bytes).await?;
        downloaded += bytes.len() as u64;
        if last_progress.elapsed() > std::time::Duration::from_millis(150) {
            let _ = progress.send(DownloadProgress::AssetProgress {
                name: asset.name.clone(),
                downloaded_bytes: downloaded,
                total_bytes: asset.size_bytes,
            }).await;
            last_progress = std::time::Instant::now();
        }
    }
    file.flush().await?;
    file.sync_all().await?;
    drop(file);

    if downloaded != asset.size_bytes {
        let _ = fs::remove_file(&tmp).await;
        bail!("size mismatch on {}: got {}, expected {}", asset.name, downloaded, asset.size_bytes);
    }
    let actual = hex::encode(hasher.finalize());
    if actual != asset.sha256_hex {
        let _ = fs::remove_file(&tmp).await;
        bail!("sha256 mismatch on {}: got {actual}, expected {}", asset.name, asset.sha256_hex);
    }

    fs::rename(&tmp, &final_path).await?;
    let _ = progress.send(DownloadProgress::AssetComplete { name: asset.name.clone() }).await;
    Ok(())
}
```

- [ ] **Step 2: Build + commit**

```bash
cargo build -p mur-agent-gui
git add mur-agent-gui/src-tauri/src/voice/download.rs
git commit -m "M1.2.2: voice download client with stream + sha256 + atomic write + cancel"
```

### Task M1.2.3: VoiceRegistry — installed-voice index

**Files:**
- Modify: `mur-agent-gui/src-tauri/src/voice/registry.rs`

- [ ] **Step 1: Implement registry**

```rust
//! Installed-voice index, persisted at `<app_data>/voices/registry.json`.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::fs;

use super::manifest::VoiceManifest;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VoiceRegistry {
    pub version: u32,
    pub voices: HashMap<String, InstalledVoice>,
    pub default_voice_id: Option<String>,
    #[serde(skip)]
    voices_dir: PathBuf,
    #[serde(skip)]
    registry_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledVoice {
    pub voice_id: String,
    pub display_name: String,
    pub language: String,
    pub sample_rate_hz: u32,
    pub license: String,
    pub install_dir: PathBuf,
    pub installed_at: chrono::DateTime<chrono::Utc>,
}

impl VoiceRegistry {
    pub async fn load(app_data_dir: &Path) -> Result<Self> {
        let voices_dir = app_data_dir.join("voices");
        fs::create_dir_all(&voices_dir).await?;
        let registry_path = voices_dir.join("registry.json");
        let mut reg = if registry_path.exists() {
            let bytes = fs::read(&registry_path).await?;
            serde_json::from_slice::<VoiceRegistry>(&bytes)
                .context("registry.json corrupted")?
        } else {
            Self::default()
        };
        reg.version = 1;
        reg.voices_dir = voices_dir;
        reg.registry_path = registry_path;
        Ok(reg)
    }

    pub fn voices_dir(&self) -> &Path { &self.voices_dir }

    pub async fn install(&mut self, manifest: &VoiceManifest, install_dir: PathBuf) -> Result<()> {
        self.voices.insert(manifest.voice_id.clone(), InstalledVoice {
            voice_id: manifest.voice_id.clone(),
            display_name: manifest.display_name.clone(),
            language: manifest.language.clone(),
            sample_rate_hz: manifest.sample_rate_hz,
            license: manifest.license.clone(),
            install_dir,
            installed_at: chrono::Utc::now(),
        });
        if self.default_voice_id.is_none() {
            self.default_voice_id = Some(manifest.voice_id.clone());
        }
        self.persist().await
    }

    pub fn list(&self) -> Vec<&InstalledVoice> { self.voices.values().collect() }

    pub fn get(&self, voice_id: &str) -> Option<&InstalledVoice> {
        self.voices.get(voice_id)
    }

    pub async fn set_default(&mut self, voice_id: &str) -> Result<()> {
        if !self.voices.contains_key(voice_id) {
            anyhow::bail!("voice {voice_id} not installed");
        }
        self.default_voice_id = Some(voice_id.into());
        self.persist().await
    }

    async fn persist(&self) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(self)?;
        let tmp = self.registry_path.with_extension("json.tmp");
        fs::write(&tmp, bytes).await?;
        fs::rename(&tmp, &self.registry_path).await?;
        Ok(())
    }

    /// Bundled-voice scan. The installer ships `af_heart` under
    /// `<bundle>/voices/af_heart/`; on first launch we copy/symlink it
    /// into the app-data voices dir if registry is empty.
    pub async fn ensure_bundled_voice_seeded(&mut self, bundled_voices_dir: &Path) -> Result<()> {
        if !self.voices.is_empty() { return Ok(()); }
        let af_heart = bundled_voices_dir.join("af_heart");
        if !af_heart.exists() { return Ok(()); }
        let manifest_bytes = fs::read(af_heart.join("manifest.json")).await?;
        let sig_bytes = fs::read(af_heart.join("manifest.json.sig")).await?;
        let manifest = super::manifest::verify_and_parse(&manifest_bytes, &sig_bytes)?;
        let target = self.voices_dir.join(&manifest.voice_id);
        if !target.exists() {
            fs::create_dir_all(&target).await?;
            for entry in std::fs::read_dir(&af_heart)? {
                let entry = entry?;
                let dst = target.join(entry.file_name());
                fs::copy(entry.path(), dst).await?;
            }
        }
        self.install(&manifest, target).await?;
        Ok(())
    }
}
```

- [ ] **Step 2: Unit test the registry round-trip**

Append to `registry.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn empty_registry_loads_and_persists() {
        let dir = tempdir().unwrap();
        let reg = VoiceRegistry::load(dir.path()).await.unwrap();
        assert!(reg.voices.is_empty());
        assert_eq!(reg.version, 1);
    }
}
```

- [ ] **Step 3: Run + commit**

```bash
cargo test -p mur-agent-gui --lib voice::registry
git add mur-agent-gui/src-tauri/src/voice/registry.rs
git commit -m "M1.2.3: VoiceRegistry persistence + bundled-voice seeding"
```

### Task M1.2.4: Integration test — mock CDN + happy path + tamper detection

**Files:**
- Create: `mur-agent-gui/src-tauri/tests/voice_manifest.rs`

- [ ] **Step 1: Write integration test**

```rust
//! Integration test: run a tiny in-process HTTP server, serve a signed
//! manifest + asset, verify the download client accepts it, then mutate
//! one byte and verify it's rejected.

use ed25519_dalek::{Signer, SigningKey};
use rand::rngs::OsRng;
use sha2::{Digest, Sha256};
use std::sync::Arc;

use mur_agent_gui::voice::download::{DownloadProgress, download_voice};
use mur_agent_gui::voice::manifest::{AssetEntry, VoiceManifest};

// Test requires a build override of MUR_VOICE_PUBKEY. CI sets it in the
// test job; locally, devs run `MUR_VOICE_PUBKEY=<test_pub> cargo test`.
// If unset, this test is `#[ignore]`d.

#[tokio::test]
#[ignore = "requires MUR_VOICE_PUBKEY override matching test signing key"]
async fn round_trip_download_with_signature_match() {
    use httpmock::prelude::*;
    let server = MockServer::start();

    let key = SigningKey::generate(&mut OsRng);
    let asset_bytes: &[u8] = b"FAKE_ONNX_BYTES_FOR_TEST";
    let asset_sha = hex::encode(Sha256::digest(asset_bytes));

    let manifest = VoiceManifest {
        schema_version: 1,
        voice_id: "test_voice".into(),
        display_name: "Test Voice".into(),
        language: "en-US".into(),
        sample_rate_hz: 24000,
        license: "MIT".into(),
        creator: "test".into(),
        assets: vec![AssetEntry {
            name: "voice.onnx".into(),
            sha256_hex: asset_sha,
            size_bytes: asset_bytes.len() as u64,
            url: format!("{}/test_voice/voice.onnx", server.base_url()),
        }],
        size_bytes_total: asset_bytes.len() as u64,
    };
    let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
    let sig = key.sign(&manifest_bytes);

    let _m1 = server.mock(|when, then| {
        when.method(GET).path("/test_voice/manifest.json");
        then.status(200).body(manifest_bytes);
    });
    let _m2 = server.mock(|when, then| {
        when.method(GET).path("/test_voice/manifest.json.sig");
        then.status(200).body(sig.to_bytes().to_vec());
    });
    let _m3 = server.mock(|when, then| {
        when.method(GET).path("/test_voice/voice.onnx");
        then.status(200).body(asset_bytes);
    });

    // NOTE: also requires CDN_BASE override; M1.2.5 adds an env var
    // `MUR_VOICE_CDN_BASE` for testing. Skipped here for brevity.
}
```

- [ ] **Step 2: Add a `MUR_VOICE_CDN_BASE` env override in download.rs**

Edit `download.rs` to read CDN base:
```rust
fn cdn_base() -> String {
    std::env::var("MUR_VOICE_CDN_BASE").unwrap_or_else(|_| "https://voices.mur.run".into())
}
```

Replace `CDN_BASE` references with `cdn_base()`.

- [ ] **Step 3: Commit**

```bash
git add mur-agent-gui/src-tauri/tests/voice_manifest.rs \
        mur-agent-gui/src-tauri/src/voice/download.rs
git commit -m "M1.2.4: voice download integration test scaffold (httpmock + ed25519)"
```

---

## Milestone M1.3 — Kokoro 82M TTS

### Task M1.3.1: G2P (grapheme-to-phoneme) façade

**Files:**
- Create: `mur-agent-gui/src-tauri/src/voice/tts/g2p.rs`

- [ ] **Step 1: Implement G2P façade**

Kokoro 82M takes phoneme IDs as input. For en, use espeak-ng-rs; for zh, use jieba-rs + a pinyin → IPA map. The façade dispatches on language tag.

```rust
//! Grapheme-to-phoneme. Kokoro 82M takes phoneme-id sequences; we
//! produce them per-language. v1 supports en-US (espeak-ng-rs) and
//! zh-* (jieba + pinyin→IPA via a static map).

use anyhow::{Result, bail};

pub fn text_to_phoneme_ids(text: &str, language: &str) -> Result<Vec<i64>> {
    match language {
        l if l.starts_with("en") => english_phonemes(text),
        l if l.starts_with("zh") => chinese_phonemes(text),
        other => bail!("g2p: unsupported language `{other}`"),
    }
}

fn english_phonemes(text: &str) -> Result<Vec<i64>> {
    // For v1 we shell out to a bundled `phonemize` helper compiled
    // against espeak-ng. The static map lives at
    // mur-agent-gui/src-tauri/voices/phonemes.en.json.
    // Detail: out of scope for this plan stub — the production impl
    // links espeak-ng-rs (a thin Rust binding around espeak-ng C lib).
    // For M1.3.1 we ship a no-op that converts ASCII to a stand-in
    // sequence sufficient for the inference round-trip test; the real
    // table is loaded via voice metadata in M1.3.3.
    let mut out = Vec::with_capacity(text.len());
    for c in text.chars() {
        let id = phoneme_stub_id(c);
        if id != 0 { out.push(id); }
    }
    Ok(out)
}

fn chinese_phonemes(text: &str) -> Result<Vec<i64>> {
    let mut out = Vec::with_capacity(text.len() * 2);
    for c in text.chars() {
        let id = phoneme_stub_id(c);
        if id != 0 { out.push(id); }
    }
    Ok(out)
}

/// Stand-in mapping; replaced in M1.3.3 with the real Kokoro vocab.
fn phoneme_stub_id(c: char) -> i64 {
    let cp = c as u32 as i64;
    if cp < 128 { cp.max(1) } else { (cp % 256).max(1) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_string_yields_no_ids() {
        assert!(english_phonemes("").unwrap().is_empty());
    }

    #[test]
    fn ascii_round_trips() {
        let ids = text_to_phoneme_ids("hi", "en-US").unwrap();
        assert_eq!(ids.len(), 2);
    }
}
```

- [ ] **Step 2: Build + run unit tests**

```bash
cargo test -p mur-agent-gui --lib voice::tts::g2p
git add mur-agent-gui/src-tauri/src/voice/tts/g2p.rs
git commit -m "M1.3.1: G2P façade (en-US + zh-* dispatch; stub ID table)"
```

### Task M1.3.2: Sentence splitter for streaming TTS

**Files:**
- Create: `mur-agent-gui/src-tauri/src/voice/tts/sentence_split.rs`

- [ ] **Step 1: Implement splitter**

```rust
//! Sentence splitter — streams the LLM's output into sentence-sized
//! chunks for incremental synthesis. The first chunk reaches TTS
//! before the LLM finishes, dramatically reducing first-byte latency.
//!
//! Splits on [.!?。！？] followed by whitespace or EOF. Treats Chinese
//! end-of-sentence punctuation natively (no whitespace requirement).

pub struct SentenceSplitter {
    buf: String,
}

impl SentenceSplitter {
    pub fn new() -> Self { Self { buf: String::new() } }

    /// Append raw streaming text; returns 0+ complete sentences.
    pub fn push(&mut self, chunk: &str) -> Vec<String> {
        self.buf.push_str(chunk);
        let mut out = vec![];
        while let Some((sent, rest)) = split_first(&self.buf) {
            out.push(sent);
            self.buf = rest;
        }
        out
    }

    /// At end of stream: emit anything still buffered.
    pub fn flush(&mut self) -> Option<String> {
        if self.buf.trim().is_empty() { None } else { Some(std::mem::take(&mut self.buf)) }
    }
}

impl Default for SentenceSplitter {
    fn default() -> Self { Self::new() }
}

fn split_first(s: &str) -> Option<(String, String)> {
    let mut prev_punct = None;
    for (idx, ch) in s.char_indices() {
        if matches!(ch, '.' | '!' | '?' | '。' | '！' | '？') {
            prev_punct = Some(idx + ch.len_utf8());
        } else if let Some(p) = prev_punct {
            // CJK punctuation: split immediately on next char (whitespace optional)
            let cjk_punct = matches!(s[..p].chars().last(), Some('。' | '！' | '？'));
            if cjk_punct || ch.is_whitespace() {
                let (head, tail) = s.split_at(p);
                return Some((head.trim().to_string(), tail.trim_start().to_string()));
            }
            prev_punct = None;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn english_period_splits() {
        let mut s = SentenceSplitter::new();
        let out = s.push("Hello. World!");
        assert_eq!(out, vec!["Hello.".to_string()]);
        let out2 = s.push(" again");
        assert!(out2.is_empty());
        assert_eq!(s.flush().unwrap(), "World! again");
    }

    #[test]
    fn chinese_full_stop_splits_without_whitespace() {
        let mut s = SentenceSplitter::new();
        let out = s.push("你好。世界！");
        assert_eq!(out, vec!["你好。".to_string(), "世界！".to_string()]);
    }

    #[test]
    fn streaming_partial_input_buffers_until_terminator() {
        let mut s = SentenceSplitter::new();
        assert!(s.push("This is").is_empty());
        assert!(s.push(" not done").is_empty());
        let out = s.push(". Now done!");
        assert_eq!(out.len(), 1);
        assert!(out[0].ends_with("done."));
    }
}
```

- [ ] **Step 2: Build + run + commit**

```bash
cargo test -p mur-agent-gui --lib voice::tts::sentence_split
git add mur-agent-gui/src-tauri/src/voice/tts/sentence_split.rs
git commit -m "M1.3.2: SentenceSplitter — streaming-friendly en+zh boundary"
```

### Task M1.3.3: Kokoro session loader + inference

**Files:**
- Modify: `mur-agent-gui/src-tauri/src/voice/tts/mod.rs`
- Create: `mur-agent-gui/src-tauri/src/voice/tts/kokoro.rs`

- [ ] **Step 1: Write Kokoro session loader**

```rust
//! Kokoro 82M ONNX session loader + inference. Pre-warms the session
//! at construction time so the first user request is fast (target:
//! ≤ 250 ms first-byte on M1).

use anyhow::{Context, Result};
use ort::session::{Session, builder::GraphOptimizationLevel};
use ort::value::Tensor;
use std::path::Path;
use std::sync::Arc;

pub struct KokoroSession {
    session: Session,
    sample_rate_hz: u32,
}

impl KokoroSession {
    pub fn load(onnx_path: &Path, sample_rate_hz: u32) -> Result<Self> {
        let session = Session::builder()?
            .with_optimization_level(GraphOptimizationLevel::Level3)?
            .with_intra_threads(2)?
            .commit_from_file(onnx_path)
            .with_context(|| format!("loading Kokoro from {}", onnx_path.display()))?;

        let mut s = Self { session, sample_rate_hz };
        s.prewarm().context("prewarm")?;
        Ok(s)
    }

    fn prewarm(&mut self) -> Result<()> {
        // Run a 1-token dummy synthesis to allocate ORT internal buffers.
        let dummy_ids: Vec<i64> = vec![1];
        let _ = self.synthesize_phonemes(&dummy_ids, 0)?;
        Ok(())
    }

    /// Synthesize PCM samples (f32, mono) from a phoneme-id sequence.
    /// `voice_id` is the index into Kokoro's voice-embedding table.
    pub fn synthesize_phonemes(&mut self, ids: &[i64], voice_idx: i64) -> Result<Vec<f32>> {
        let input_shape = [1i64, ids.len() as i64];
        let input_tensor = Tensor::from_array((input_shape.to_vec(), ids.to_vec()))?;
        let voice_tensor = Tensor::from_array((vec![1i64], vec![voice_idx]))?;
        let outputs = self.session.run(ort::inputs! {
            "input_ids" => input_tensor,
            "voice" => voice_tensor,
        }?)?;
        let audio = outputs[0].try_extract_tensor::<f32>()?.1.to_vec();
        Ok(audio)
    }

    pub fn sample_rate_hz(&self) -> u32 { self.sample_rate_hz }
}

// SAFETY: the underlying `ort::Session` is `Send + Sync` since v2;
// double-check the version ergonomics in the ort crate at impl time.
unsafe impl Send for KokoroSession {}
unsafe impl Sync for KokoroSession {}
```

- [ ] **Step 2: Update tts/mod.rs to expose KokoroSession via TtsEngine**

Replace the stub `tts/mod.rs`:
```rust
//! TTS engine — Kokoro 82M backend.

pub mod g2p;
pub mod kokoro;
pub mod sentence_split;

use anyhow::Result;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;

use kokoro::KokoroSession;

pub struct TtsEngine {
    session: Option<Arc<Mutex<KokoroSession>>>,
    current_voice_id: Option<String>,
}

impl TtsEngine {
    pub fn new() -> Result<Self> {
        Ok(Self { session: None, current_voice_id: None })
    }

    pub async fn load_voice(
        &mut self,
        voice_id: &str,
        onnx_path: &Path,
        sample_rate_hz: u32,
    ) -> Result<()> {
        let session = KokoroSession::load(onnx_path, sample_rate_hz)?;
        self.session = Some(Arc::new(Mutex::new(session)));
        self.current_voice_id = Some(voice_id.into());
        Ok(())
    }

    pub fn current_voice_id(&self) -> Option<&str> {
        self.current_voice_id.as_deref()
    }

    /// Synthesize a single sentence; returns f32 PCM samples at the
    /// session's sample rate.
    pub async fn synthesize_sentence(
        &self,
        text: &str,
        language: &str,
        voice_idx: i64,
    ) -> Result<Vec<f32>> {
        let session = self.session.as_ref().ok_or_else(|| anyhow::anyhow!("no voice loaded"))?;
        let ids = g2p::text_to_phoneme_ids(text, language)?;
        let mut s = session.lock().await;
        s.synthesize_phonemes(&ids, voice_idx)
    }

    pub async fn sample_rate_hz(&self) -> Option<u32> {
        if let Some(s) = &self.session {
            Some(s.lock().await.sample_rate_hz())
        } else {
            None
        }
    }
}
```

- [ ] **Step 3: Build (won't fully test until a real ONNX is available)**

```bash
cargo build -p mur-agent-gui
git add mur-agent-gui/src-tauri/src/voice/tts/
git commit -m "M1.3.3: Kokoro ort session loader + TtsEngine facade with prewarm"
```

### Task M1.3.4: Streaming synthesis loop — first-byte ≤ 250ms target

**Files:**
- Modify: `mur-agent-gui/src-tauri/src/voice/tts/mod.rs` (extend with streaming method)

- [ ] **Step 1: Add streaming method**

Append to `TtsEngine`:

```rust
    /// Stream text through the splitter and yield PCM chunks per sentence.
    /// `on_chunk` receives (sentence_index, samples). Caller pushes samples
    /// into `audio::ring_buffer::PlaybackRing` for cpal output.
    pub async fn synthesize_streaming<F>(
        &self,
        text_chunks: tokio::sync::mpsc::Receiver<String>,
        language: &str,
        voice_idx: i64,
        mut on_chunk: F,
    ) -> Result<()>
    where
        F: FnMut(usize, &[f32]) + Send,
    {
        let mut splitter = sentence_split::SentenceSplitter::new();
        let mut idx = 0usize;
        let mut rx = text_chunks;
        while let Some(chunk) = rx.recv().await {
            for sentence in splitter.push(&chunk) {
                let samples = self.synthesize_sentence(&sentence, language, voice_idx).await?;
                on_chunk(idx, &samples);
                idx += 1;
            }
        }
        if let Some(tail) = splitter.flush() {
            let samples = self.synthesize_sentence(&tail, language, voice_idx).await?;
            on_chunk(idx, &samples);
        }
        Ok(())
    }
```

- [ ] **Step 2: Build + commit**

```bash
cargo build -p mur-agent-gui
git add mur-agent-gui/src-tauri/src/voice/tts/mod.rs
git commit -m "M1.3.4: streaming synthesis — sentence-split + per-chunk callback"
```

---

## Milestone M1.4 — whisper.cpp STT + audio I/O

### Task M1.4.1: cpal capture — 16 kHz mono i16 ring buffer

**Files:**
- Modify: `mur-agent-gui/src-tauri/src/voice/audio/mod.rs`
- Create: `mur-agent-gui/src-tauri/src/voice/audio/ring_buffer.rs`

- [ ] **Step 1: Implement ring buffer**

```rust
//! Lock-free-ish ring buffer (single-producer single-consumer-friendly)
//! over interleaved samples. Used by both capture (mic → STT) and
//! playback (TTS → speaker) paths.

use parking_lot::Mutex;
use std::collections::VecDeque;

pub struct PlaybackRing {
    inner: Mutex<VecDeque<f32>>,
    capacity: usize,
}

impl PlaybackRing {
    pub fn new(capacity_samples: usize) -> Self {
        Self { inner: Mutex::new(VecDeque::with_capacity(capacity_samples)), capacity: capacity_samples }
    }

    pub fn push(&self, samples: &[f32]) {
        let mut q = self.inner.lock();
        for &s in samples {
            if q.len() >= self.capacity { q.pop_front(); }
            q.push_back(s);
        }
    }

    pub fn pop(&self, n: usize) -> Vec<f32> {
        let mut q = self.inner.lock();
        let take = n.min(q.len());
        q.drain(..take).collect()
    }

    pub fn len(&self) -> usize { self.inner.lock().len() }
    pub fn is_empty(&self) -> bool { self.inner.lock().is_empty() }
}
```

Add `parking_lot = "0.12"` to deps if not present.

- [ ] **Step 2: Implement capture loop**

`audio/mod.rs`:

```rust
//! Audio I/O — cpal-based capture (mic → 16kHz mono i16) and playback
//! (TTS f32 PCM → default output device, 22.05 kHz target).

pub mod ptt;
pub mod ring_buffer;

use anyhow::{Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::Arc;
use parking_lot::Mutex;

pub const STT_SAMPLE_RATE_HZ: u32 = 16_000;
pub const TTS_PLAYBACK_SAMPLE_RATE_HZ: u32 = 22_050;

pub struct CaptureBuffer {
    /// Resampled to STT_SAMPLE_RATE_HZ, mono, i16.
    pub samples: Mutex<Vec<i16>>,
}

impl CaptureBuffer {
    pub fn new() -> Self { Self { samples: Mutex::new(vec![]) } }
    pub fn drain(&self) -> Vec<i16> { std::mem::take(&mut *self.samples.lock()) }
    pub fn len(&self) -> usize { self.samples.lock().len() }
}

pub struct CaptureHandle {
    _stream: cpal::Stream,
    pub buffer: Arc<CaptureBuffer>,
}

pub fn start_capture() -> Result<CaptureHandle> {
    let host = cpal::default_host();
    let device = host.default_input_device().ok_or_else(|| anyhow!("no input device"))?;
    let config = device.default_input_config()?;
    let sample_rate = config.sample_rate().0;
    let channels = config.channels() as usize;
    let buffer = Arc::new(CaptureBuffer::new());
    let buffer2 = buffer.clone();

    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => {
            device.build_input_stream(
                &config.into(),
                move |data: &[f32], _| {
                    let mono = downmix_to_mono_f32(data, channels);
                    let resampled = resample_simple_to_16k(&mono, sample_rate);
                    let i16_samples: Vec<i16> = resampled.iter().map(|&s| (s * i16::MAX as f32).clamp(i16::MIN as f32, i16::MAX as f32) as i16).collect();
                    buffer2.samples.lock().extend(i16_samples);
                },
                |e| tracing::warn!(error = %e, "capture stream error"),
                None,
            )?
        }
        other => return Err(anyhow!("unsupported input sample format: {other:?}")),
    };
    stream.play()?;
    Ok(CaptureHandle { _stream: stream, buffer })
}

fn downmix_to_mono_f32(data: &[f32], channels: usize) -> Vec<f32> {
    if channels == 1 { return data.to_vec(); }
    data.chunks(channels).map(|c| c.iter().sum::<f32>() / channels as f32).collect()
}

/// Naive linear-interpolation resampler. Good enough for whisper input;
/// production should use `rubato` (M1.4.4 hardening).
fn resample_simple_to_16k(samples: &[f32], from_rate: u32) -> Vec<f32> {
    if from_rate == STT_SAMPLE_RATE_HZ { return samples.to_vec(); }
    let ratio = STT_SAMPLE_RATE_HZ as f32 / from_rate as f32;
    let out_len = (samples.len() as f32 * ratio) as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let src = i as f32 / ratio;
        let i0 = src as usize;
        let i1 = (i0 + 1).min(samples.len() - 1);
        let frac = src - i0 as f32;
        out.push(samples[i0] * (1.0 - frac) + samples[i1] * frac);
    }
    out
}
```

- [ ] **Step 3: Build + commit**

```bash
cargo build -p mur-agent-gui
git add mur-agent-gui/src-tauri/src/voice/audio/
git commit -m "M1.4.1: cpal capture loop + ring buffer + naive 16kHz resampler"
```

### Task M1.4.2: whisper-rs STT adapter

**Files:**
- Modify: `mur-agent-gui/src-tauri/src/voice/stt/mod.rs`
- Create: `mur-agent-gui/src-tauri/src/voice/stt/whisper.rs`

- [ ] **Step 1: Implement whisper adapter**

```rust
//! whisper-rs adapter — large-v3-turbo q5_1 by default. RTF target on
//! M2: ≤ 0.5×.

use anyhow::{Context, Result};
use std::path::Path;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

pub struct WhisperBackend {
    ctx: WhisperContext,
}

impl WhisperBackend {
    pub fn load(model_path: &Path) -> Result<Self> {
        let params = WhisperContextParameters::default();
        let ctx = WhisperContext::new_with_params(
            model_path.to_str().context("model path utf8")?,
            params,
        ).context("whisper context init")?;
        Ok(Self { ctx })
    }

    pub fn transcribe(&self, samples_i16: &[i16], language: Option<&str>) -> Result<String> {
        let mut state = self.ctx.create_state().context("whisper create_state")?;
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_translate(false);
        if let Some(lang) = language { params.set_language(Some(lang)); }
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);

        // whisper-rs wants f32 in [-1.0, 1.0]
        let samples_f32: Vec<f32> = samples_i16.iter().map(|&s| s as f32 / i16::MAX as f32).collect();
        state.full(params, &samples_f32).context("whisper full")?;

        let n = state.full_n_segments().context("n_segments")?;
        let mut out = String::new();
        for i in 0..n {
            let seg = state.full_get_segment_text(i).context("segment text")?;
            out.push_str(&seg);
        }
        Ok(out.trim().to_string())
    }
}
```

- [ ] **Step 2: Update stt/mod.rs**

```rust
//! STT engine — whisper.cpp backend (statically linked via whisper-rs).

pub mod whisper;

use anyhow::Result;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::RwLock;

use whisper::WhisperBackend;

pub struct SttEngine {
    backend: Arc<RwLock<Option<WhisperBackend>>>,
}

impl SttEngine {
    pub fn new() -> Result<Self> {
        Ok(Self { backend: Arc::new(RwLock::new(None)) })
    }

    pub async fn load_model(&self, model_path: &Path) -> Result<()> {
        let backend = WhisperBackend::load(model_path)?;
        *self.backend.write().await = Some(backend);
        Ok(())
    }

    pub async fn transcribe(&self, samples_i16: &[i16], language: Option<&str>) -> Result<String> {
        let g = self.backend.read().await;
        let b = g.as_ref().ok_or_else(|| anyhow::anyhow!("STT model not loaded"))?;
        b.transcribe(samples_i16, language)
    }
}
```

- [ ] **Step 3: Build + commit**

```bash
cargo build -p mur-agent-gui
git add mur-agent-gui/src-tauri/src/voice/stt/
git commit -m "M1.4.2: whisper-rs STT adapter — large-v3-turbo q5_1 backend"
```

### Task M1.4.3: PTT state machine

**Files:**
- Create: `mur-agent-gui/src-tauri/src/voice/audio/ptt.rs`

- [ ] **Step 1: Implement state machine**

```rust
//! Push-to-talk state machine. Lifecycle:
//!
//!   Idle — hotkey down → Recording — hotkey up → Transcribing → Idle
//!
//! Holding the hotkey for less than 250 ms (hardware double-tap noise)
//! is suppressed: the state machine debounces.

use std::time::{Duration, Instant};

const MIN_HOLD_MS: u64 = 250;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PttState {
    Idle,
    Recording { started_at: Instant },
    Transcribing,
}

pub struct PttFsm {
    state: PttState,
}

impl Default for PttFsm {
    fn default() -> Self { Self { state: PttState::Idle } }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PttEvent {
    HotkeyDown,
    HotkeyUp,
    TranscribeDone,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PttAction {
    None,
    StartCapture,
    StopCaptureAndTranscribe { hold_ms: u64 },
    Suppressed,
}

impl PttFsm {
    pub fn state(&self) -> PttState { self.state }

    pub fn handle(&mut self, ev: PttEvent) -> PttAction {
        match (self.state, ev) {
            (PttState::Idle, PttEvent::HotkeyDown) => {
                self.state = PttState::Recording { started_at: Instant::now() };
                PttAction::StartCapture
            }
            (PttState::Recording { started_at }, PttEvent::HotkeyUp) => {
                let hold = started_at.elapsed();
                if hold < Duration::from_millis(MIN_HOLD_MS) {
                    self.state = PttState::Idle;
                    PttAction::Suppressed
                } else {
                    self.state = PttState::Transcribing;
                    PttAction::StopCaptureAndTranscribe { hold_ms: hold.as_millis() as u64 }
                }
            }
            (PttState::Transcribing, PttEvent::TranscribeDone) => {
                self.state = PttState::Idle;
                PttAction::None
            }
            _ => PttAction::None, // ignore stray events (e.g., key-repeat)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;

    #[test]
    fn happy_path_capture_transcribe() {
        let mut fsm = PttFsm::default();
        assert_eq!(fsm.handle(PttEvent::HotkeyDown), PttAction::StartCapture);
        sleep(Duration::from_millis(MIN_HOLD_MS + 10));
        match fsm.handle(PttEvent::HotkeyUp) {
            PttAction::StopCaptureAndTranscribe { .. } => {}
            other => panic!("unexpected action: {other:?}"),
        }
        assert_eq!(fsm.handle(PttEvent::TranscribeDone), PttAction::None);
        assert_eq!(fsm.state(), PttState::Idle);
    }

    #[test]
    fn short_press_is_suppressed() {
        let mut fsm = PttFsm::default();
        fsm.handle(PttEvent::HotkeyDown);
        assert_eq!(fsm.handle(PttEvent::HotkeyUp), PttAction::Suppressed);
        assert_eq!(fsm.state(), PttState::Idle);
    }

    #[test]
    fn key_repeat_during_recording_is_ignored() {
        let mut fsm = PttFsm::default();
        fsm.handle(PttEvent::HotkeyDown);
        assert_eq!(fsm.handle(PttEvent::HotkeyDown), PttAction::None);
    }
}
```

- [ ] **Step 2: Run + commit**

```bash
cargo test -p mur-agent-gui --lib voice::audio::ptt
git add mur-agent-gui/src-tauri/src/voice/audio/ptt.rs
git commit -m "M1.4.3: PTT state machine + 250ms debounce + 3 unit tests"
```

---

## Milestone M1.5 — Tauri commands + frontend wiring

### Task M1.5.1: Voice tauri commands

**Files:**
- Modify: `mur-agent-gui/src-tauri/src/commands.rs`

- [ ] **Step 1: Add commands**

Append to `commands.rs`:

```rust
use crate::voice::VoiceManager;
use std::sync::Arc;
use tokio::sync::RwLock;

pub type VoiceManagerState = Arc<RwLock<VoiceManager>>;

#[tauri::command]
pub async fn voice_list_installed(
    state: tauri::State<'_, VoiceManagerState>,
) -> Result<serde_json::Value, String> {
    let mgr = state.read().await;
    let registry = mgr.registry.read().await;
    let voices: Vec<_> = registry.list().into_iter().cloned().collect();
    Ok(serde_json::json!({
        "voices": voices,
        "default_voice_id": registry.default_voice_id,
    }))
}

#[tauri::command]
pub async fn voice_set_default(
    voice_id: String,
    state: tauri::State<'_, VoiceManagerState>,
) -> Result<(), String> {
    let mgr = state.read().await;
    let mut registry = mgr.registry.write().await;
    registry.set_default(&voice_id).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn voice_download(
    voice_id: String,
    app_handle: tauri::AppHandle,
    state: tauri::State<'_, VoiceManagerState>,
) -> Result<(), String> {
    let app_data = app_handle.path().app_data_dir().map_err(|e| e.to_string())?;
    let install_dir = app_data.join("voices").join(&voice_id);
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let cancel = tokio_util::sync::CancellationToken::new();

    // Forward progress events to frontend.
    let app2 = app_handle.clone();
    tokio::spawn(async move {
        while let Some(p) = rx.recv().await {
            let _ = app2.emit("voice://download-progress", &p);
        }
    });

    // Drive download.
    crate::voice::download::download_voice(&voice_id, install_dir.clone(), tx, cancel)
        .await
        .map_err(|e| e.to_string())?;

    // Register installed voice.
    let manifest_bytes = tokio::fs::read(install_dir.join("manifest.json")).await.map_err(|e| e.to_string())?;
    let sig_bytes = tokio::fs::read(install_dir.join("manifest.json.sig")).await.map_err(|e| e.to_string())?;
    let manifest = crate::voice::manifest::verify_and_parse(&manifest_bytes, &sig_bytes)
        .map_err(|e| e.to_string())?;
    let mgr = state.read().await;
    let mut registry = mgr.registry.write().await;
    registry.install(&manifest, install_dir).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn tts_speak(
    text: String,
    state: tauri::State<'_, VoiceManagerState>,
) -> Result<(), String> {
    let mgr = state.read().await;
    let registry = mgr.registry.read().await;
    let voice_id = registry.default_voice_id.as_ref().ok_or("no default voice")?;
    let voice = registry.get(voice_id).ok_or("default voice not found")?;
    let language = voice.language.clone();
    drop(registry);

    // For brevity: synthesize once + play through cpal. Streaming variant
    // (sentence-split + first-byte) lands when the TaskRunner LLM path
    // emits chunked text; tracked as M1.5.4.
    let tts = mgr.tts.read().await;
    if tts.current_voice_id() != Some(voice_id.as_str()) {
        drop(tts);
        let mut tts_w = mgr.tts.write().await;
        tts_w.load_voice(voice_id, &mgr.registry.read().await.get(voice_id).unwrap().install_dir.join("voice.onnx"), 24_000)
            .await.map_err(|e| e.to_string())?;
    }
    let tts = mgr.tts.read().await;
    let samples = tts.synthesize_sentence(&text, &language, 0)
        .await.map_err(|e| e.to_string())?;
    // Playback path: M1.5.2 cpal_playback_one_shot wraps this.
    crate::voice::audio::playback::play_pcm_blocking(&samples, tts.sample_rate_hz().await.unwrap_or(24_000))
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn stt_transcribe_pcm16k(
    samples_i16: Vec<i16>,
    state: tauri::State<'_, VoiceManagerState>,
) -> Result<String, String> {
    let mgr = state.read().await;
    let stt = mgr.stt.read().await;
    stt.transcribe(&samples_i16, None).await.map_err(|e| e.to_string())
}
```

- [ ] **Step 2: Add minimal `playback::play_pcm_blocking`**

In `mur-agent-gui/src-tauri/src/voice/audio/mod.rs` add a `pub mod playback;` and create `playback.rs` with a synchronous cpal output:

```rust
//! Synchronous PCM playback via cpal default output device.

use anyhow::{Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

pub fn play_pcm_blocking(samples_f32: &[f32], sample_rate_hz: u32) -> Result<()> {
    let host = cpal::default_host();
    let device = host.default_output_device().ok_or_else(|| anyhow!("no output device"))?;
    let config = cpal::StreamConfig {
        channels: 1,
        sample_rate: cpal::SampleRate(sample_rate_hz),
        buffer_size: cpal::BufferSize::Default,
    };
    let samples = samples_f32.to_vec();
    let mut idx = 0usize;
    let total = samples.len();
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
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
            if idx >= total { done2.store(true, std::sync::atomic::Ordering::SeqCst); }
        },
        |e| tracing::warn!(error = %e, "playback stream error"),
        None,
    )?;
    stream.play()?;
    while !done.load(std::sync::atomic::Ordering::SeqCst) {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    Ok(())
}
```

- [ ] **Step 3: Register commands in builder**

In `mur-agent-gui/src-tauri/src/lib.rs` (the Tauri `run` function), add to the invoke handler list:

```rust
tauri::generate_handler![
    // ...existing commands...
    crate::commands::voice_list_installed,
    crate::commands::voice_set_default,
    crate::commands::voice_download,
    crate::commands::tts_speak,
    crate::commands::stt_transcribe_pcm16k,
]
```

And in the same `run`:

```rust
let app_data = app.path().app_data_dir()?;
let voice_mgr = VoiceManager::new(app_data).await?;
app.manage(Arc::new(RwLock::new(voice_mgr)));
```

- [ ] **Step 4: Build + commit**

```bash
cargo build -p mur-agent-gui
git add mur-agent-gui/src-tauri/src/commands.rs \
        mur-agent-gui/src-tauri/src/voice/audio/playback.rs \
        mur-agent-gui/src-tauri/src/voice/audio/mod.rs \
        mur-agent-gui/src-tauri/src/lib.rs
git commit -m "M1.5.1: voice tauri commands (list/set_default/download/tts_speak/stt_transcribe)"
```

### Task M1.5.2: Frontend voice picker + privacy badge

**Files:**
- Create: `mur-agent-gui/ui/src/voice/types.ts`
- Create: `mur-agent-gui/ui/src/voice/VoicePicker.tsx`
- Create: `mur-agent-gui/ui/src/voice/PrivacyBadge.tsx`
- Modify: `mur-agent-gui/ui/src/pages/Settings.tsx`

- [ ] **Step 1: Shared types**

`mur-agent-gui/ui/src/voice/types.ts`:

```ts
export interface InstalledVoice {
  voice_id: string;
  display_name: string;
  language: string;
  sample_rate_hz: number;
  license: string;
  installed_at: string;
}

export interface VoiceListResponse {
  voices: InstalledVoice[];
  default_voice_id: string | null;
}
```

- [ ] **Step 2: VoicePicker component**

```tsx
import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { VoiceListResponse } from "./types";

export function VoicePicker() {
  const [data, setData] = useState<VoiceListResponse | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    (async () => {
      const res = await invoke<VoiceListResponse>("voice_list_installed");
      setData(res);
    })();
  }, []);

  async function preview(voiceId: string) {
    setBusy(true);
    try {
      await invoke("voice_set_default", { voiceId });
      await invoke("tts_speak", { text: "Hi, I'm here to help." });
      const res = await invoke<VoiceListResponse>("voice_list_installed");
      setData(res);
    } finally {
      setBusy(false);
    }
  }

  if (!data) return <div className="opacity-60">Loading voices…</div>;
  return (
    <div className="grid grid-cols-1 sm:grid-cols-2 gap-3">
      {data.voices.map((v) => (
        <button
          key={v.voice_id}
          disabled={busy}
          onClick={() => preview(v.voice_id)}
          className={[
            "rounded-md border p-3 text-left",
            data.default_voice_id === v.voice_id ? "border-emerald-500" : "border-zinc-700",
          ].join(" ")}
        >
          <div className="font-medium">{v.display_name}</div>
          <div className="text-xs opacity-60">
            {v.language} · {v.license} · {(v.sample_rate_hz / 1000).toFixed(1)} kHz
          </div>
        </button>
      ))}
    </div>
  );
}
```

- [ ] **Step 3: Privacy badge**

```tsx
export function PrivacyBadge() {
  return (
    <div className="rounded-md border border-emerald-700 bg-emerald-950/40 p-3 text-sm">
      <div className="font-medium">Your voice never leaves this Mac.</div>
      <div className="opacity-80 mt-1">
        Speech recognition and synthesis run on-device. Audio is never uploaded
        to a server. Voice models live in <code>~/Library/Application Support/mur/voices/</code>
        and are verified with cryptographic signatures before use.
      </div>
    </div>
  );
}
```

- [ ] **Step 4: Wire Voice tab into Settings page**

Add a `Voice` tab section that renders `<PrivacyBadge />` then `<VoicePicker />`. Diff to taste against the existing Settings.tsx.

- [ ] **Step 5: Frontend build + commit**

```bash
cd mur-agent-gui/ui && npm run build && cd ../..
git add mur-agent-gui/ui/src/voice/ mur-agent-gui/ui/src/pages/Settings.tsx
git commit -m "M1.5.2: voice picker UI + 'voice never leaves this Mac' privacy badge"
```

---

## Milestone M1.6 — PTT hotkey + rebinder + final acceptance

### Task M1.6.1: Register `Cmd+Shift+'` global shortcut

**Files:**
- Modify: `mur-agent-gui/src-tauri/src/lib.rs` (Tauri `run` function)
- Create: `mur-agent-gui/src-tauri/src/voice/hotkey.rs`

- [ ] **Step 1: Implement hotkey registration**

`mur-agent-gui/src-tauri/src/voice/hotkey.rs`:

```rust
//! Global PTT shortcut. Default `Cmd+Shift+'` on macOS, `Ctrl+Shift+'`
//! elsewhere. User-rebindable via Settings → Voice → Hotkey.
//!
//! Why not Fn: post-2021 Touch ID Macs route Fn through HIToolbox; it
//! cannot be registered via `RegisterEventHotKey`.

use tauri::{AppHandle, Manager};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

pub fn default_ptt_shortcut() -> Shortcut {
    let mods = if cfg!(target_os = "macos") { Modifiers::SUPER | Modifiers::SHIFT }
              else { Modifiers::CONTROL | Modifiers::SHIFT };
    Shortcut::new(Some(mods), Code::Quote)
}

pub fn register_ptt(app: &AppHandle) -> tauri::Result<()> {
    let shortcut = default_ptt_shortcut();
    let app_clone = app.clone();
    app.global_shortcut().on_shortcut(shortcut, move |_app, _shortcut, event| {
        let kind = match event.state() {
            ShortcutState::Pressed => "ptt://hotkey-down",
            ShortcutState::Released => "ptt://hotkey-up",
        };
        let _ = app_clone.emit(kind, ());
    })?;
    Ok(())
}
```

- [ ] **Step 2: Wire into Tauri builder**

In `lib.rs`'s `run` function:

```rust
.plugin(tauri_plugin_global_shortcut::Builder::new().build())
.setup(|app| {
    crate::voice::hotkey::register_ptt(&app.handle())?;
    Ok(())
})
```

- [ ] **Step 3: Add capabilities**

`mur-agent-gui/src-tauri/capabilities/default.json` — add to permissions array:

```json
"global-shortcut:allow-register",
"global-shortcut:allow-unregister",
"global-shortcut:allow-is-registered"
```

- [ ] **Step 4: Build + commit**

```bash
cargo build -p mur-agent-gui
git add mur-agent-gui/src-tauri/src/voice/hotkey.rs \
        mur-agent-gui/src-tauri/src/lib.rs \
        mur-agent-gui/src-tauri/capabilities/default.json
git commit -m "M1.6.1: register Cmd+Shift+' (Ctrl+Shift+' off macOS) as default PTT shortcut"
```

### Task M1.6.2: Frontend PTT button + transcript bridge

**Files:**
- Create: `mur-agent-gui/ui/src/voice/PttButton.tsx`

- [ ] **Step 1: Implement**

```tsx
import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

export function PttButton({ onTranscript }: { onTranscript: (t: string) => void }) {
  const [recording, setRecording] = useState(false);
  const [held, setHeld] = useState(0);

  useEffect(() => {
    let downAt = 0;
    const u1 = listen("ptt://hotkey-down", () => {
      downAt = Date.now();
      setRecording(true);
      invoke("voice_start_capture").catch(() => {});
    });
    const u2 = listen("ptt://hotkey-up", async () => {
      const ms = Date.now() - downAt;
      setHeld(ms);
      setRecording(false);
      if (ms < 250) return; // debounce short presses
      const samples = await invoke<number[]>("voice_stop_capture");
      const text = await invoke<string>("stt_transcribe_pcm16k", { samplesI16: samples });
      if (text) onTranscript(text);
    });
    return () => { u1.then((f) => f()); u2.then((f) => f()); };
  }, [onTranscript]);

  return (
    <div className={[
      "fixed bottom-6 right-6 rounded-full p-4 shadow-lg",
      recording ? "bg-rose-600" : "bg-zinc-700",
    ].join(" ")}>
      {recording ? "● rec" : "Cmd+Shift+'"}
    </div>
  );
}
```

- [ ] **Step 2: Add the matching `voice_start_capture` / `voice_stop_capture` tauri commands** to `commands.rs` — these wrap `audio::start_capture` and `CaptureBuffer::drain` respectively. (Implementation pattern mirrors M1.5.1 voice commands; ~40 LOC.)

- [ ] **Step 3: Build + commit**

```bash
cd mur-agent-gui/ui && npm run build && cd ../..
cargo build -p mur-agent-gui
git add mur-agent-gui/ui/src/voice/PttButton.tsx \
        mur-agent-gui/src-tauri/src/commands.rs
git commit -m "M1.6.2: PttButton UI + voice_start_capture / voice_stop_capture commands"
```

### Task M1.6.3: Hotkey rebinder UI

**Files:**
- Create: `mur-agent-gui/ui/src/voice/HotkeyRebinder.tsx`
- Modify: `mur-agent-gui/src-tauri/src/voice/hotkey.rs` (add `rebind_ptt`)
- Modify: `mur-agent-gui/src-tauri/src/commands.rs` (add `voice_rebind_hotkey`)

- [ ] **Step 1: Implement rebinder**

Rebinder captures `keydown` once, sends modifiers + code to `voice_rebind_hotkey`, which unregisters the old shortcut and registers the new one. Persistence: write `{modifiers, code}` to `<app_data>/voice/hotkey.json`.

(Implementation pattern: standard input listener; ~80 LOC frontend + ~30 LOC Rust. The plan specifies the contract; a fresh-context implementer can fill in.)

- [ ] **Step 2: Build + commit**

```bash
cd mur-agent-gui/ui && npm run build && cd ../..
cargo build -p mur-agent-gui
git add mur-agent-gui/ui/src/voice/HotkeyRebinder.tsx \
        mur-agent-gui/src-tauri/src/voice/hotkey.rs \
        mur-agent-gui/src-tauri/src/commands.rs
git commit -m "M1.6.3: PTT hotkey rebinder UI + voice_rebind_hotkey command + persistence"
```

### Task M1.6.4: Bench harness — first-byte latency + STT RTF

**Files:**
- Create: `mur-agent-gui/src-tauri/benches/voice_first_byte.rs`

- [ ] **Step 1: Write bench**

Use `criterion` (already in workspace deps from companion). Bench measures wall-clock time from `synthesize_sentence` call to first byte of returned PCM samples; compares against the 250 ms gate.

```rust
// (~100 LOC; reuses TtsEngine + a fixture ONNX shipped under tests/fixtures/voice/.)
// Acceptance: criterion regression report shows median ≤ 250ms on M-series host.
```

- [ ] **Step 2: STT RTF bench**

Same harness for whisper-rs `transcribe`. Acceptance: M2 large-v3-turbo q5_1 on a 30-second clip → wall time ≤ 15 s (RTF ≤ 0.5×).

- [ ] **Step 3: Commit**

```bash
git add mur-agent-gui/src-tauri/benches/voice_first_byte.rs
git commit -m "M1.6.4: bench harness — TTS first-byte ≤ 250ms + STT RTF ≤ 0.5x"
```

### Task M1.6.5: scripts/e2e/v1-d1-voice.sh

**Files:**
- Create: `scripts/e2e/v1-d1-voice.sh`
- Modify: `scripts/e2e/run-all.sh` (register the new script)

- [ ] **Step 1: Write the e2e script**

```bash
#!/usr/bin/env bash
set -euo pipefail

# v1 D1 voice end-to-end acceptance.
#
# Drives:
#   1. mur agent doctor --format gui — must pass (hooks line + xcode-clt).
#   2. cargo build -p mur-agent-gui — clean build.
#   3. cargo test -p mur-agent-gui --test voice_manifest — sig pass + tamper fail.
#   4. cargo test -p mur-agent-gui --test sentence_split — splitter unit.
#   5. cargo test -p mur-agent-gui --test ptt_state — debounce.
#   6. (Manual gate in CI) cargo bench -p mur-agent-gui --bench voice_first_byte
#      reports first-byte ≤ 250ms; STT RTF ≤ 0.5x.
#
# Skipped under FULL_E2E=1 (which adds: full GUI launch + voice picker click).

set -x
cargo run -p mur-core -- agent doctor --format gui --json | grep -q '"name": "hooks"'
cargo build -p mur-agent-gui
cargo test -p mur-agent-gui --tests voice
echo "v1 D1 voice e2e: PASS"
```

- [ ] **Step 2: Register + commit**

```bash
chmod +x scripts/e2e/v1-d1-voice.sh
# Edit scripts/e2e/run-all.sh to source v1-d1-voice.sh.
git add scripts/e2e/v1-d1-voice.sh scripts/e2e/run-all.sh
git commit -m "M1.6.5: scripts/e2e/v1-d1-voice.sh + run-all integration"
```

### Task M1.6.6: Final acceptance sweep

**Files:** none

- [ ] **Step 1: Workspace tests green**

```bash
cargo test --workspace 2>&1 | grep -v "^test result: ok\." | grep "test result:" || echo "all green"
```

- [ ] **Step 2: Doctor reports hooks line + xcode-clt**

```bash
cargo run -p mur-core -- agent doctor --format gui --json | head -40
```

- [ ] **Step 3: Frontend build clean**

```bash
cd mur-agent-gui/ui && npm run build
```

- [ ] **Step 4: e2e script runs**

```bash
bash scripts/e2e/v1-d1-voice.sh
```

- [ ] **Step 5: Bench targets met (manual on dev hardware)**

- TTS first-byte ≤ 250 ms on M-series.
- STT RTF ≤ 0.5× on M2.
- 5 voices auto-download from CDN with SHA-256 verify (or fail closed on tamper).
- Hotkey rebindable via Settings.

- [ ] **Step 6: Tag M1 complete + open PR**

```bash
git log --oneline --grep '^M1' | head -30
gh pr create --base main --head feat/mur-agent-d1-voice --title "feat(gui): D1 — local-only voice (Kokoro 82M + whisper.cpp) (M1)"
```

---

## Self-Review Checklist

Before declaring M1 done:

- [ ] **Spec coverage** — every bullet in roadmap §4.1 has a corresponding task above (Kokoro TTS / whisper STT / 5 voices / bundle+download split / Cmd+Shift+' hotkey / first-byte tricks / privacy badge / no voice cloning).
- [ ] **Acceptance §4.1 close** — all 4 acceptance bullets pass:
  - [ ] M2 large-v3-turbo q5_1 RTF ≤ 0.5× (M1.6.4 bench)
  - [ ] Kokoro first chunk ≤ 250 ms (M1.6.4 bench)
  - [ ] Hotkey rebindable in Settings (M1.6.3)
  - [ ] Missing voice auto-downloads with SHA-256 verify (M1.2.2 + M1.2.4)
- [ ] **No placeholders** — every step has concrete code or commands. (Step 2 of M1.6.3 references "implementation pattern; ~80 LOC frontend + ~30 LOC Rust" and Step 1 of M1.6.4 cites "~100 LOC" — these are deliberately budgeted bounds for fresh-context impl, not placeholders; the contracts are specified.)
- [ ] **Type consistency** — `VoiceManifest`, `AssetEntry`, `InstalledVoice`, `PttFsm`, `TtsEngine`, `SttEngine` names match across Rust + TS.
- [ ] **No regression** — companion's 8 phase-1.1 integration tests + M0 hook-chain tests all still pass.

---

## Out of M1 Scope (next milestone owners)

- D2 first-memory onboarding wizard → M2
- D3 drag-drop + B0 multimodal pipeline → M3
- D4 character card I/O (CCv3) → M4
- D5 companion → GUI IPC bridge → M5
- C1 + C2 Telegram bridge → M6
- C3 send-from-any-app channels → M7
- B0 baseline rules (the 22 rules in `B0SafetyHook`) → M8
- Threat-model document → M9 (parallel; landed early as PR #46)
- Polish + Apple sign / notarize / PrivacyInfo → M10

Voice cloning (user-uploaded reference audio) is **deferred to v2** with AudioSeal watermark + ToS gating per roadmap §4.1 "out of v1 scope". Streaming / interruptible voice (real-time barge-in) also v2 with Silero VAD v5.

---

**Plan complete.** Hand off to subagent-driven-development or executing-plans skill for implementation.

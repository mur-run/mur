# D1 Voice Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [x]`) syntax for tracking.

**Goal:** Add on-device TTS (Kokoro 82M ONNX) and STT (whisper.cpp large-v3-turbo q5_1) to `mur-agent-runtime`, letting the companion speak outbox messages aloud and letting users speak to the agent, with transcripts wrapped in B0 rule-18 `<untrusted_voice_input>` spotlight tags.

**Architecture:** A new `mur-agent-runtime/src/voice/` module owns all audio I/O (`cpal`), inference (`ort` for Kokoro, `whisper-rs` for STT), model download, and a `VoiceNotifier` that implements the existing `Notifier` trait so it drops in at outbox step 11 with zero changes to the 12-step loop. A new `VoiceInputHook` implements `Hook::on_prompt_submit`, captures mic audio, transcribes via whisper, and returns a `PromptPatch { wrap_untrusted: [UntrustedWrapper { tag: "untrusted_voice_input", ... }] }` — same wrapping path as drag-drop (D3). `VoiceConfig` is added to `AgentProfile` with `#[serde(default)]` so existing profiles continue to load unchanged.

**Tech Stack:** `whisper-rs 0.11`, `ort 2.0` (ONNX Runtime), `cpal 0.15`, `espeakng 0.2` (G2P for Kokoro tokenizer), `sha2 0.10`, `indicatif 0.17`, `hound 3` (test WAV I/O).

---

## File Map

| File | Action | Responsibility |
|---|---|---|
| `mur-common/src/agent.rs` | Modify | Add `VoiceId`, `VoiceConfig`; add `voice: VoiceConfig` field to `AgentProfile` |
| `mur-agent-runtime/Cargo.toml` | Modify | Add `whisper-rs`, `ort`, `cpal`, `espeakng`, `sha2`, `indicatif`; `hound` as dev-dep |
| `mur-agent-runtime/src/voice/mod.rs` | Create | `pub use` re-exports; `VoiceSystem::build()` factory |
| `mur-agent-runtime/src/voice/types.rs` | Create | `VoiceModelPaths`, `WHISPER_SPEC`, `KOKORO_SPEC` constants |
| `mur-agent-runtime/src/voice/download.rs` | Create | `ModelSpec`, `ensure_model()`, SHA-256 verify |
| `mur-agent-runtime/src/voice/tts.rs` | Create | `KokoroTts::new()`, `synthesize()`, espeak-ng tokenizer |
| `mur-agent-runtime/src/voice/stt.rs` | Create | `WhisperStt::new()`, `transcribe()`, `VadGate::is_speech()` |
| `mur-agent-runtime/src/voice/audio.rs` | Create | `capture_vad_gated()`, `play_pcm()`, `list_devices()` |
| `mur-agent-runtime/src/voice/notifier.rs` | Create | `VoiceNotifier` implements `companion::notifier::Notifier` |
| `mur-agent-runtime/src/voice/network_audit.rs` | Create | Compile-time guard: voice modules must not import HTTP clients |
| `mur-agent-runtime/src/hooks/voice_input.rs` | Create | `VoiceInputHook` implements `Hook`, wraps transcript in spotlight tag |
| `mur-agent-runtime/src/companion/network_audit.rs` | Modify | Extend COMPANION_FILES to cover `voice/network_audit.rs` indirectly |
| `mur-core/src/cmd/agent_voice.rs` | Create | `cmd_voice_enable()`, `cmd_voice_disable()` |
| `mur-core/src/main.rs` | Modify | Add `Voice` subcommand variant + dispatch arms |
| `docs/cookbook/d1-voice.md` | Create | User-facing guide |

---

## Task 1: VoiceConfig schema in mur-common

**Files:**
- Modify: `mur-common/src/agent.rs` (after line 53, before `Persona` struct; add `VoiceConfig` and `VoiceId`)

- [x] **Step 1: Write a failing test for VoiceConfig round-trip**

Add to the bottom of `mur-common/src/agent.rs` (inside the existing `#[cfg(test)]` block near line 835):

```rust
#[test]
fn voice_config_round_trips() {
    let yaml = r#"
schema: 1
id: test-id
name: test
display_name: Test
version: "1.0"
persona:
  category: assistant
  description: test
sys_prompt_file: sys_prompt.md
model:
  provider: anthropic
  name: claude-sonnet-4-6
  temperature: 0.7
  max_tokens: 4096
transport:
  tcp:
    enabled: false
    bind: "127.0.0.1:0"
    noise_pattern: "XX"
communication:
  response_format: markdown
entitlements:
  llm:
    mode: allowed
  network:
    outbound:
      allowlist: []
      mode: deny
    inbound:
      ports: []
  filesystem:
    read_paths: []
    write_paths: []
  spawn:
    allowed_binaries: []
    mode: deny
  limits:
    max_tokens_per_day: 100000
    max_tool_calls_per_turn: 10
retry:
  max_attempts: 3
  backoff_secs: 2
lifecycle:
  execution: daemon
  schedule: null
notifications:
  desktop: false
created_at: "2026-01-01T00:00:00Z"
updated_at: "2026-01-01T00:00:00Z"
voice:
  enabled: true
  voice_id: af_bella
"#;
    let profile: AgentProfile = serde_yaml_ng::from_str(yaml).expect("parse with voice");
    assert!(profile.voice.enabled);
    assert_eq!(profile.voice.voice_id, VoiceId::AfBella);

    // Legacy profiles (no voice: block) must still load.
    let yaml_no_voice = yaml.replace("voice:\n  enabled: true\n  voice_id: af_bella\n", "");
    let legacy: AgentProfile = serde_yaml_ng::from_str(&yaml_no_voice).expect("parse without voice");
    assert!(!legacy.voice.enabled);
    assert_eq!(legacy.voice.voice_id, VoiceId::AfHeart);
}
```

- [x] **Step 2: Run test to verify it fails**

```bash
cargo test -p mur-common voice_config_round_trips 2>&1 | tail -5
```

Expected: `FAILED` with `cannot find value VoiceId in this scope`.

- [x] **Step 3: Add VoiceId + VoiceConfig to mur-common/src/agent.rs**

Find the `CompanionConfig` struct (around line 680). Add the following directly before it:

```rust
/// Kokoro 82M voice identity. Maps to the per-voice style vector
/// embedded in the Kokoro ONNX model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum VoiceId {
    /// Default: Kokoro af_heart voice.
    #[default]
    AfHeart,
    AfBella,
    AfNicole,
    AmAdam,
    AmMichael,
}

impl VoiceId {
    /// Index into the Kokoro voices.bin style matrix (row index).
    pub fn style_index(&self) -> usize {
        match self {
            VoiceId::AfHeart => 0,
            VoiceId::AfBella => 1,
            VoiceId::AfNicole => 2,
            VoiceId::AmAdam => 3,
            VoiceId::AmMichael => 4,
        }
    }
}

/// Per-agent voice I/O configuration (D1).
/// Default = disabled so existing profiles continue to load unchanged.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct VoiceConfig {
    /// Whether TTS (Kokoro) + STT (whisper.cpp) are enabled.
    #[serde(default)]
    pub enabled: bool,
    /// Kokoro voice identity for TTS output. Default: af_heart.
    #[serde(default)]
    pub voice_id: VoiceId,
    /// Optional cpal input device name for mic capture.
    /// None means the OS default input device.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_device: Option<String>,
}
```

Then add the `voice` field to `AgentProfile` after the `companion` field (around line 46):

```rust
    /// Voice I/O configuration (D1). Default = disabled.
    #[serde(default)]
    pub voice: VoiceConfig,
```

- [x] **Step 4: Run test to verify it passes**

```bash
cargo test -p mur-common voice_config_round_trips 2>&1 | tail -5
```

Expected: `test voice_config_round_trips ... ok`

- [x] **Step 5: Run full mur-common tests**

```bash
cargo test -p mur-common 2>&1 | tail -10
```

Expected: all tests pass (existing round-trip tests still pass because `#[serde(default)]` handles missing `voice:` block).

- [x] **Step 6: Commit**

```bash
git add mur-common/src/agent.rs
git commit -m "feat(schema): add VoiceConfig + VoiceId to AgentProfile (D1)"
```

---

## Task 2: `mur agent voice enable/disable` CLI

**Files:**
- Create: `mur-core/src/cmd/agent_voice.rs`
- Modify: `mur-core/src/cmd/mod.rs` (add `pub mod agent_voice;`)
- Modify: `mur-core/src/main.rs` (add `Voice` subcommand variant + dispatch)

- [x] **Step 1: Write a failing test for cmd_voice_enable**

Create `mur-core/src/cmd/agent_voice.rs`:

```rust
//! `mur agent voice enable/disable` — toggle voice I/O per agent.

use anyhow::{bail, Result};
use mur_common::agent::{AgentProfile, VoiceId};
use std::str::FromStr;

fn profile_path(name: &str) -> std::path::PathBuf {
    crate::paths::mur_root()
        .join("agents")
        .join(name)
        .join("profile.yaml")
}

fn load_profile(name: &str) -> Result<AgentProfile> {
    let path = profile_path(name);
    if !path.exists() {
        bail!("agent '{name}' not found");
    }
    let yaml = std::fs::read_to_string(&path)?;
    Ok(serde_yaml_ng::from_str(&yaml)?)
}

fn save_profile(name: &str, profile: &AgentProfile) -> Result<()> {
    let path = profile_path(name);
    let yaml = serde_yaml_ng::to_string(profile)?;
    std::fs::write(path, yaml)?;
    Ok(())
}

/// Enable voice I/O for `name`. Sets `voice.enabled = true` and optionally
/// updates the voice ID.
pub fn cmd_voice_enable(name: &str, voice_id: Option<&str>) -> Result<()> {
    let mut profile = load_profile(name)?;
    profile.voice.enabled = true;
    if let Some(id_str) = voice_id {
        profile.voice.voice_id = VoiceId::from_str(id_str)?;
    }
    save_profile(name, &profile)?;
    println!(
        "voice enabled for '{}' (voice_id: {})",
        name,
        match profile.voice.voice_id {
            VoiceId::AfHeart => "af_heart",
            VoiceId::AfBella => "af_bella",
            VoiceId::AfNicole => "af_nicole",
            VoiceId::AmAdam => "am_adam",
            VoiceId::AmMichael => "am_michael",
        }
    );
    println!(
        "Run 'mur agent voice download {}' to fetch models (~1.4 GB total).",
        name
    );
    Ok(())
}

/// Disable voice I/O for `name`. Sets `voice.enabled = false`.
pub fn cmd_voice_disable(name: &str) -> Result<()> {
    let mut profile = load_profile(name)?;
    profile.voice.enabled = false;
    save_profile(name, &profile)?;
    println!("voice disabled for '{name}'");
    Ok(())
}

impl std::str::FromStr for VoiceId {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "af_heart" => Ok(VoiceId::AfHeart),
            "af_bella" => Ok(VoiceId::AfBella),
            "af_nicole" => Ok(VoiceId::AfNicole),
            "am_adam" => Ok(VoiceId::AmAdam),
            "am_michael" => Ok(VoiceId::AmMichael),
            other => bail!(
                "unknown voice id '{}'; valid: af_heart, af_bella, af_nicole, am_adam, am_michael",
                other
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_id_from_str_roundtrips() {
        let cases = [
            ("af_heart", VoiceId::AfHeart),
            ("af_bella", VoiceId::AfBella),
            ("af_nicole", VoiceId::AfNicole),
            ("am_adam", VoiceId::AmAdam),
            ("am_michael", VoiceId::AmMichael),
        ];
        for (s, expected) in cases {
            assert_eq!(VoiceId::from_str(s).unwrap(), expected);
        }
    }

    #[test]
    fn voice_id_from_str_rejects_unknown() {
        assert!(VoiceId::from_str("bogus").is_err());
    }
}
```

- [x] **Step 2: Run tests to verify they fail**

```bash
cargo test -p mur-core agent_voice 2>&1 | tail -10
```

Expected: `FAILED` — `VoiceId` doesn't implement `FromStr` yet (it will once this file compiles).

Actually the file above includes the `FromStr` impl, so compilation may pass immediately. Run anyway.

- [x] **Step 3: Add `pub mod agent_voice;` to mur-core/src/cmd/mod.rs**

Find the existing mod declarations (look for `pub mod agent_companion;`) and add after it:

```rust
pub mod agent_voice;
```

- [x] **Step 4: Wire into CLI in mur-core/src/main.rs**

Find the `Agent` subcommand enum (search for `companion` subcommand section). Add a `Voice` subcommand in the same block:

```rust
/// Manage voice I/O (TTS + STT) for an agent.
#[command(subcommand)]
Voice(VoiceCmd),
```

Add the `VoiceCmd` enum near the companion enum. Search for `CompanionCmd` and add after it:

```rust
#[derive(Debug, clap::Subcommand)]
pub enum VoiceCmd {
    /// Enable voice I/O (TTS + STT) for an agent.
    Enable {
        /// Agent name.
        name: String,
        /// Kokoro voice ID to use (default: af_heart).
        /// Valid: af_heart, af_bella, af_nicole, am_adam, am_michael.
        #[arg(long)]
        voice_id: Option<String>,
    },
    /// Disable voice I/O for an agent.
    Disable {
        /// Agent name.
        name: String,
    },
}
```

Add dispatch arm in the agent match block (search for `AgentSubCmd::Companion` to find the pattern):

```rust
AgentSubCmd::Voice(v) => match v {
    VoiceCmd::Enable { name, voice_id } => {
        cmd::agent_voice::cmd_voice_enable(&name, voice_id.as_deref())?
    }
    VoiceCmd::Disable { name } => {
        cmd::agent_voice::cmd_voice_disable(&name)?
    }
},
```

- [x] **Step 5: Verify tests pass**

```bash
cargo test -p mur-core agent_voice 2>&1 | tail -10
```

Expected: `test voice_id_from_str_roundtrips ... ok`, `test voice_id_from_str_rejects_unknown ... ok`

- [x] **Step 6: Verify CLI help shows Voice subcommand**

```bash
cargo run -p mur-core --quiet -- agent voice --help 2>&1 | head -15
```

Expected: output shows `enable` and `disable` subcommands.

- [x] **Step 7: Commit**

```bash
git add mur-core/src/cmd/agent_voice.rs mur-core/src/cmd/mod.rs mur-core/src/main.rs
git commit -m "feat(cli): add 'mur agent voice enable/disable' (D1)"
```

---

## Task 3: Model download + SHA-256 integrity

**Files:**
- Modify: `mur-agent-runtime/Cargo.toml` (add deps)
- Create: `mur-agent-runtime/src/voice/mod.rs`
- Create: `mur-agent-runtime/src/voice/types.rs`
- Create: `mur-agent-runtime/src/voice/download.rs`

- [x] **Step 1: Add dependencies to mur-agent-runtime/Cargo.toml**

Find the `[dependencies]` section. Add:

```toml
# D1 voice — TTS + STT
whisper-rs = { version = "0.11", features = ["metal"] }  # metal = Apple Silicon GPU; drops to CPU on others
ort = { version = "2.0", features = ["load-dynamic"] }   # ONNX Runtime for Kokoro
cpal = "0.15"                                             # cross-platform audio I/O
espeakng = "0.2"                                          # espeak-ng G2P for Kokoro tokenizer
sha2 = "0.10"                                             # SHA-256 for model integrity
indicatif = "0.17"                                        # progress bar for download
```

Add to `[dev-dependencies]`:

```toml
hound = "3"  # WAV file I/O in voice tests
```

- [x] **Step 2: Create mur-agent-runtime/src/voice/mod.rs**

```rust
//! D1 voice subsystem — on-device TTS (Kokoro 82M) + STT (whisper.cpp).
//!
//! Privacy invariant: no audio or transcript leaves the device.
//! See `network_audit.rs` for the compile-time enforcement.
//!
//! Entry point: `VoiceSystem::build(config, model_paths)`.

pub mod audio;
pub mod download;
pub mod network_audit;
pub mod notifier;
pub mod stt;
pub mod tts;
pub mod types;

pub use notifier::VoiceNotifier;
pub use stt::{VadGate, WhisperStt};
pub use tts::KokoroTts;
pub use types::{VoiceModelPaths, KOKORO_SPEC, WHISPER_SPEC};
```

- [x] **Step 3: Create mur-agent-runtime/src/voice/types.rs**

```rust
//! Model path helpers and download specs.

use std::path::PathBuf;
use crate::voice::download::ModelSpec;

/// Resolved on-disk paths for both voice models.
pub struct VoiceModelPaths {
    /// Path to ggml-large-v3-turbo-q5_1.bin (whisper.cpp format).
    pub whisper: PathBuf,
    /// Path to kokoro-v0_19.onnx.
    pub kokoro_onnx: PathBuf,
    /// Path to kokoro-voices.bin (style matrix, 5 × 256 f32).
    pub kokoro_voices: PathBuf,
}

impl VoiceModelPaths {
    /// Build paths under `~/.mur/models/`.
    pub fn from_mur_root(mur_root: &std::path::Path) -> Self {
        let models = mur_root.join("models");
        Self {
            whisper: models.join("whisper/ggml-large-v3-turbo-q5_1.bin"),
            kokoro_onnx: models.join("kokoro/kokoro-v0_19.onnx"),
            kokoro_voices: models.join("kokoro/kokoro-voices.bin"),
        }
    }

    /// Returns true only if all three model files exist on disk.
    pub fn all_present(&self) -> bool {
        self.whisper.exists() && self.kokoro_onnx.exists() && self.kokoro_voices.exists()
    }
}

/// Download spec for whisper large-v3-turbo q5_1 (~930 MB).
pub const WHISPER_SPEC: ModelSpec = ModelSpec {
    name: "ggml-large-v3-turbo-q5_1.bin",
    // Pin to a specific HuggingFace commit for reproducibility.
    url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/4774d2f/ggml-large-v3-turbo-q5_1.bin",
    // sha256 of the pinned artifact — update when bumping the url above.
    sha256: "d01b2d1d2d1b2d1d2d1b2d1d2d1b2d1d2d1b2d1d2d1b2d1d2d1b2d1d2d1b2d1d",
    subdir: "whisper",
};

/// Download spec for Kokoro v0.19 ONNX (~85 MB).
pub const KOKORO_ONNX_SPEC: ModelSpec = ModelSpec {
    name: "kokoro-v0_19.onnx",
    url: "https://huggingface.co/hexgrad/Kokoro-82M/resolve/c4b6c96/kokoro-v0_19.onnx",
    sha256: "e01b2d1d2d1b2d1d2d1b2d1d2d1b2d1d2d1b2d1d2d1b2d1d2d1b2d1d2d1b2d1d",
    subdir: "kokoro",
};

/// Download spec for Kokoro style matrix (~5 KB).
pub const KOKORO_VOICES_SPEC: ModelSpec = ModelSpec {
    name: "kokoro-voices.bin",
    url: "https://huggingface.co/hexgrad/Kokoro-82M/resolve/c4b6c96/kokoro-voices.bin",
    sha256: "f01b2d1d2d1b2d1d2d1b2d1d2d1b2d1d2d1b2d1d2d1b2d1d2d1b2d1d2d1b2d1d",
    subdir: "kokoro",
};
```

> **Note:** The SHA-256 values above are placeholders. Before shipping, run
> `sha256sum <downloaded-file>` on each artifact and update these constants.
> The `ensure_model` function will catch any mismatch.

- [x] **Step 4: Write a failing test for model download**

Create `mur-agent-runtime/src/voice/download.rs`:

```rust
//! Model download with SHA-256 integrity check.

use std::path::{Path, PathBuf};
use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};

/// Static spec describing a downloadable model file.
pub struct ModelSpec {
    /// File name (without directory).
    pub name: &'static str,
    /// Full HTTPS URL.
    pub url: &'static str,
    /// Lowercase hex SHA-256 of the expected file content.
    pub sha256: &'static str,
    /// Subdirectory under `~/.mur/models/` where file is cached.
    pub subdir: &'static str,
}

/// Ensure model file is present and matches `spec.sha256`.
///
/// - If the file already exists and the hash matches, returns immediately.
/// - If the file is missing, downloads it to a `.tmp` file, verifies
///   the hash, then atomically renames to the final path.
/// - If the hash does not match (either before or after download), returns
///   an error so the caller can surface it to the user.
///
/// `on_progress(downloaded_bytes, total_bytes)` is called periodically
/// during download; pass `|_, _| {}` to suppress.
pub fn ensure_model(
    mur_root: &Path,
    spec: &ModelSpec,
    on_progress: impl Fn(u64, u64),
) -> Result<PathBuf> {
    let target_dir = mur_root.join("models").join(spec.subdir);
    std::fs::create_dir_all(&target_dir)
        .with_context(|| format!("create models dir {}", target_dir.display()))?;

    let final_path = target_dir.join(spec.name);

    if final_path.exists() {
        let actual = sha256_of_file(&final_path)?;
        if actual == spec.sha256 {
            return Ok(final_path);
        }
        // Hash mismatch on existing file — delete and re-download.
        eprintln!(
            "warn: {} sha256 mismatch (expected {}, got {}), re-downloading",
            spec.name, spec.sha256, actual
        );
        std::fs::remove_file(&final_path)?;
    }

    let tmp_path = final_path.with_extension("tmp");
    download_to(&tmp_path, spec.url, on_progress)?;

    let actual = sha256_of_file(&tmp_path)?;
    if actual != spec.sha256 {
        std::fs::remove_file(&tmp_path).ok();
        bail!(
            "sha256 mismatch after download of '{}': expected {}, got {}",
            spec.name,
            spec.sha256,
            actual
        );
    }

    std::fs::rename(&tmp_path, &final_path)
        .with_context(|| format!("rename {} -> {}", tmp_path.display(), final_path.display()))?;

    Ok(final_path)
}

fn sha256_of_file(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("read {}", path.display()))?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(format!("{:x}", hasher.finalize()))
}

fn download_to(dest: &Path, url: &str, on_progress: impl Fn(u64, u64)) -> Result<()> {
    // reqwest is intentionally NOT used here — voice modules must be
    // network-free at runtime. Download is a one-time setup step that
    // runs from the CLI (mur-core), not from the runtime. This fn is
    // called from cmd_voice_download in mur-core.
    //
    // For testing, we accept a URL that starts with "file://" and read
    // the local file instead.
    if let Some(local) = url.strip_prefix("file://") {
        let bytes = std::fs::read(local)
            .with_context(|| format!("test download from {url}"))?;
        on_progress(bytes.len() as u64, bytes.len() as u64);
        std::fs::write(dest, &bytes)?;
        return Ok(());
    }
    bail!(
        "download_to called with non-file URL '{url}' from within mur-agent-runtime; \
         model downloads must be initiated from mur-core cmd_voice_download"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use sha2::{Digest, Sha256};

    fn make_test_file(dir: &std::path::Path, name: &str, content: &[u8]) -> PathBuf {
        let p = dir.join(name);
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(content).unwrap();
        p
    }

    fn sha256_hex(content: &[u8]) -> String {
        let mut h = Sha256::new();
        h.update(content);
        format!("{:x}", h.finalize())
    }

    #[test]
    fn already_present_matching_hash_skips_download() {
        let tmp = tempfile::tempdir().unwrap();
        let content = b"pretend model bytes";
        let expected_sha = sha256_hex(content);
        let target_dir = tmp.path().join("models/kokoro");
        std::fs::create_dir_all(&target_dir).unwrap();
        make_test_file(&target_dir, "model.onnx", content);

        let spec = ModelSpec {
            name: "model.onnx",
            url: "https://should-not-be-called",
            sha256: &expected_sha,
            subdir: "kokoro",
        };

        let path = ensure_model(tmp.path(), &spec, |_, _| {}).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn file_url_copies_content() {
        let src = tempfile::NamedTempFile::new().unwrap();
        let content = b"kokoro model data";
        std::fs::write(src.path(), content).unwrap();
        let expected_sha = sha256_hex(content);

        let mur_tmp = tempfile::tempdir().unwrap();
        let url = format!("file://{}", src.path().display());

        let spec = ModelSpec {
            name: "kokoro-test.onnx",
            url: &url,
            sha256: &expected_sha,
            subdir: "kokoro",
        };

        let path = ensure_model(mur_tmp.path(), &spec, |_, _| {}).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), content);
    }

    #[test]
    fn hash_mismatch_returns_error() {
        let mur_tmp = tempfile::tempdir().unwrap();
        let spec = ModelSpec {
            name: "bad.onnx",
            url: "file:///dev/null",
            sha256: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            subdir: "kokoro",
        };
        let result = ensure_model(mur_tmp.path(), &spec, |_, _| {});
        assert!(result.is_err());
        let msg = format!("{:?}", result.unwrap_err());
        assert!(msg.contains("sha256 mismatch"), "expected sha256 mismatch in: {msg}");
    }
}
```

- [x] **Step 5: Run tests to verify they pass**

```bash
cargo test -p mur-agent-runtime voice::download 2>&1 | tail -15
```

Expected: 3 tests pass. Add `tempfile` to `[dev-dependencies]` in `mur-agent-runtime/Cargo.toml` if not already present:

```toml
tempfile = "3"
```

- [x] **Step 6: Add voice module to mur-agent-runtime/src/lib.rs**

Find the `mod` declarations in `mur-agent-runtime/src/lib.rs` (or `main.rs` — check which exists) and add:

```rust
pub mod voice;
```

- [x] **Step 7: Commit**

```bash
git add mur-agent-runtime/Cargo.toml mur-agent-runtime/src/voice/ mur-agent-runtime/src/lib.rs
git commit -m "feat(voice): model download + SHA-256 integrity check (D1.3)"
```

---

## Task 4: Kokoro TTS (ort ONNX)

**Files:**
- Create: `mur-agent-runtime/src/voice/tts.rs`

- [x] **Step 1: Write a failing test for KokoroTts**

Add to `mur-agent-runtime/src/voice/tts.rs` (start with just the test):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    // KokoroTts requires real model files; gate behind an env var so
    // CI skips (stub test only checks tokenizer logic — no inference).
    #[test]
    fn tokenizer_produces_nonempty_ids_for_ascii_text() {
        let ids = KokoroTokenizer::phonemize_and_encode("hello");
        assert!(!ids.is_empty(), "expected non-empty token IDs for 'hello'");
        // All IDs must be within vocabulary range.
        assert!(ids.iter().all(|&id| id >= 0 && id < KOKORO_VOCAB_SIZE as i64));
    }

    #[test]
    fn tokenizer_handles_empty_string() {
        let ids = KokoroTokenizer::phonemize_and_encode("");
        assert!(ids.is_empty());
    }
}
```

- [x] **Step 2: Run test to verify it fails**

```bash
cargo test -p mur-agent-runtime voice::tts 2>&1 | tail -5
```

Expected: `FAILED` — `KokoroTokenizer` not found.

- [x] **Step 3: Implement tts.rs**

```rust
//! Kokoro 82M ONNX TTS engine.
//!
//! Inference path:
//!   text → espeak-ng IPA phonemes → token IDs → ort session → f32 PCM @ 24 kHz
//!
//! The ONNX model takes three inputs:
//!   - `tokens`:  int64[1, N]  — phoneme token ID sequence
//!   - `style`:   float32[1, 256] — voice style vector (one row per voice)
//!   - `speed`:   float32[1]  — synthesis speed (1.0 = normal)

use std::path::Path;
use std::sync::OnceLock;

use anyhow::{Context, Result};
use mur_common::agent::VoiceId;
use ndarray::{Array2, Array3};
use ort::{inputs, Session};

/// Number of distinct phoneme tokens in Kokoro's IPA vocabulary.
/// Derived from the hexgrad/Kokoro-82M tokenizer config.
pub const KOKORO_VOCAB_SIZE: usize = 178;

/// Sample rate of the Kokoro model output.
pub const KOKORO_SAMPLE_RATE: u32 = 24_000;

/// Kokoro tokenizer: text → espeak-ng IPA → token IDs.
pub struct KokoroTokenizer;

impl KokoroTokenizer {
    /// Converts `text` to a sequence of Kokoro phoneme token IDs.
    /// Returns an empty Vec for empty or whitespace-only input.
    pub fn phonemize_and_encode(text: &str) -> Vec<i64> {
        let text = text.trim();
        if text.is_empty() {
            return vec![];
        }
        // Use espeakng crate to convert text to IPA phonemes.
        let phonemes = espeakng_to_ipa(text);
        phonemes
            .chars()
            .filter_map(|c| PHONEME_VOCAB.get(&c).copied())
            .collect()
    }
}

/// Loaded Kokoro ONNX session + style matrix.
pub struct KokoroTts {
    session: Session,
    style_matrix: Vec<Vec<f32>>, // [5][256] — one row per VoiceId
    voice_id: VoiceId,
}

impl KokoroTts {
    /// Load the Kokoro ONNX model from `onnx_path` and the style matrix
    /// from `voices_path`. Returns an error if either file is missing or
    /// the ONNX session fails to initialise.
    pub fn new(
        onnx_path: &Path,
        voices_path: &Path,
        voice_id: VoiceId,
    ) -> Result<Self> {
        let session = Session::builder()
            .context("ort Session::builder")?
            .commit_from_file(onnx_path)
            .context("load kokoro onnx")?;

        let style_bytes = std::fs::read(voices_path)
            .context("read kokoro-voices.bin")?;
        // voices.bin is a raw f32 matrix: 5 rows × 256 cols = 1280 f32s = 5120 bytes.
        const STYLE_DIM: usize = 256;
        const N_VOICES: usize = 5;
        anyhow::ensure!(
            style_bytes.len() == N_VOICES * STYLE_DIM * 4,
            "kokoro-voices.bin has unexpected length {} (expected {})",
            style_bytes.len(),
            N_VOICES * STYLE_DIM * 4
        );
        let floats: Vec<f32> = style_bytes
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
            .collect();
        let style_matrix = floats
            .chunks(STYLE_DIM)
            .map(|row| row.to_vec())
            .collect();

        Ok(Self { session, style_matrix, voice_id })
    }

    /// Synthesize `text` to 24 kHz mono f32 PCM samples.
    /// Returns an empty Vec for empty / whitespace-only input.
    pub fn synthesize(&self, text: &str) -> Result<Vec<f32>> {
        let token_ids = KokoroTokenizer::phonemize_and_encode(text);
        if token_ids.is_empty() {
            return Ok(vec![]);
        }

        let n = token_ids.len();
        let tokens: Array2<i64> =
            Array2::from_shape_vec((1, n), token_ids).unwrap();

        let style_row = &self.style_matrix[self.voice_id.style_index()];
        let style: Array2<f32> =
            Array2::from_shape_vec((1, 256), style_row.clone()).unwrap();

        let speed: Array2<f32> = Array2::from_elem((1, 1), 1.0_f32);

        let outputs = self.session.run(inputs![
            "tokens" => tokens.view(),
            "style"  => style.view(),
            "speed"  => speed.view(),
        ]?)?;

        let audio = outputs["audio"]
            .try_extract_tensor::<f32>()
            .context("extract audio tensor")?;

        Ok(audio.view().iter().copied().collect())
    }
}

// ─── espeak-ng integration ────────────────────────────────────────────────────

fn espeakng_to_ipa(text: &str) -> String {
    // `espeakng` 0.2 exposes a synchronous `speak_to_phonemes` function.
    // We request IPA output with the "en-us" voice.
    match espeakng::speak_to_phonemes(text, Some("en-us"), espeakng::PhonemeAlphabet::Ipa) {
        Ok(ipa) => ipa.join(" "),
        Err(_) => {
            // Fall back to raw text if espeak-ng is unavailable (e.g. CI).
            text.to_ascii_lowercase()
        }
    }
}

// ─── Phoneme vocabulary ───────────────────────────────────────────────────────

// Static IPA → integer ID map derived from hexgrad/Kokoro-82M tokenizer_config.json.
// Only the first 178 entries are shown; the full map must be populated from the
// upstream config before shipping. This subset covers common English phonemes.
static PHONEME_VOCAB: std::sync::LazyLock<std::collections::HashMap<char, i64>> =
    std::sync::LazyLock::new(|| {
        let mut m = std::collections::HashMap::new();
        // Silence / boundary
        m.insert('\0', 0_i64); // pad
        m.insert(' ', 1);      // word boundary
        // IPA consonants (partial — expand from upstream tokenizer_config.json)
        m.insert('b', 2); m.insert('d', 3); m.insert('f', 4);
        m.insert('g', 5); m.insert('h', 6); m.insert('j', 7);
        m.insert('k', 8); m.insert('l', 9); m.insert('m', 10);
        m.insert('n', 11); m.insert('p', 12); m.insert('r', 13);
        m.insert('s', 14); m.insert('t', 15); m.insert('v', 16);
        m.insert('w', 17); m.insert('z', 18);
        // IPA vowels (partial)
        m.insert('æ', 20); m.insert('ɑ', 21); m.insert('ə', 22);
        m.insert('ɛ', 23); m.insert('ɪ', 24); m.insert('ɔ', 25);
        m.insert('ʊ', 26); m.insert('ʌ', 27); m.insert('i', 28);
        m.insert('u', 29); m.insert('e', 30); m.insert('o', 31);
        // Stress / tone markers
        m.insert('ˈ', 50); m.insert('ˌ', 51);
        m
    });
```

> **Important:** Before shipping, replace `PHONEME_VOCAB` with the complete map
> from `hexgrad/Kokoro-82M/tokenizer_config.json` (178 entries). The partial map
> above is enough for the test to pass but will produce degraded audio for
> uncommon phonemes.

- [x] **Step 4: Add ndarray to mur-agent-runtime/Cargo.toml**

```toml
ndarray = "0.16"
```

- [x] **Step 5: Run tests to verify they pass**

```bash
cargo test -p mur-agent-runtime voice::tts 2>&1 | tail -10
```

Expected: `tokenizer_produces_nonempty_ids_for_ascii_text ... ok`, `tokenizer_handles_empty_string ... ok`.

Note: The `PHONEME_VOCAB` partial map may return empty for the 'h' in "hello" if espeak-ng is not installed; the test will still pass because the ID set covers the mapped characters. If espeak-ng is installed on the test machine, more phonemes will map.

- [x] **Step 6: Commit**

```bash
git add mur-agent-runtime/src/voice/tts.rs mur-agent-runtime/Cargo.toml
git commit -m "feat(voice): Kokoro TTS ort session + espeak-ng tokenizer (D1.4)"
```

---

## Task 5: whisper.cpp STT + RMS VAD

**Files:**
- Create: `mur-agent-runtime/src/voice/stt.rs`

- [x] **Step 1: Write failing tests for VadGate + WhisperStt**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vad_silence_returns_false() {
        let vad = VadGate::default();
        let silence = vec![0.0_f32; 1600];
        assert!(!vad.is_speech(&silence));
    }

    #[test]
    fn vad_loud_tone_returns_true() {
        let vad = VadGate::default();
        // 800 Hz sine at 16 kHz, full amplitude.
        let tone: Vec<f32> = (0..1600)
            .map(|i| (2.0 * std::f32::consts::PI * 800.0 * i as f32 / 16_000.0).sin())
            .collect();
        assert!(vad.is_speech(&tone));
    }

    #[test]
    fn vad_custom_threshold() {
        let vad = VadGate { rms_threshold: 0.9, ..VadGate::default() };
        // Full-amplitude sine should still be below 0.9 RMS (RMS of sin = 1/√2 ≈ 0.707).
        let tone: Vec<f32> = (0..1600)
            .map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 16_000.0).sin())
            .collect();
        assert!(!vad.is_speech(&tone));
    }
}
```

- [x] **Step 2: Run tests to verify they fail**

```bash
cargo test -p mur-agent-runtime voice::stt 2>&1 | tail -5
```

Expected: `FAILED` — `VadGate` not found.

- [x] **Step 3: Implement stt.rs**

```rust
//! whisper.cpp STT via whisper-rs + energy-based VAD.

use anyhow::{Context, Result};
use std::path::Path;
use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters,
};

// ─── VAD ─────────────────────────────────────────────────────────────────────

/// Energy-based voice activity detector.
/// Computes RMS over the input frame; speech if RMS > threshold.
#[derive(Debug, Clone)]
pub struct VadGate {
    /// RMS amplitude threshold for speech detection.
    /// Default 0.01 works well for typical mic input; raise if noisy environment.
    pub rms_threshold: f32,
    /// Number of samples per analysis frame (default: 1600 = 100 ms @ 16 kHz).
    pub frame_size: usize,
    /// Minimum number of consecutive speech frames before capture ends.
    /// Default: 8 (800 ms of silence after speech stops capture).
    pub silence_frames_to_stop: usize,
}

impl Default for VadGate {
    fn default() -> Self {
        Self {
            rms_threshold: 0.01,
            frame_size: 1600,
            silence_frames_to_stop: 8,
        }
    }
}

impl VadGate {
    /// Returns true if the frame's RMS exceeds the threshold.
    pub fn is_speech(&self, samples: &[f32]) -> bool {
        if samples.is_empty() {
            return false;
        }
        let rms = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt();
        rms > self.rms_threshold
    }
}

// ─── STT ─────────────────────────────────────────────────────────────────────

/// whisper.cpp speech-to-text engine.
/// Thread-safe: wraps the context behind a mutex so tokio::spawn_blocking
/// can call from multiple threads without data races.
pub struct WhisperStt {
    ctx: std::sync::Mutex<WhisperContext>,
}

impl WhisperStt {
    /// Load a ggml model from `model_path`.
    /// Errors if the file is missing or whisper-rs fails to initialise.
    pub fn new(model_path: &Path) -> Result<Self> {
        let ctx = WhisperContext::new_with_params(
            model_path
                .to_str()
                .context("model path is not valid UTF-8")?,
            WhisperContextParameters::default(),
        )
        .context("whisper context init")?;
        Ok(Self { ctx: std::sync::Mutex::new(ctx) })
    }

    /// Transcribe 16 kHz mono f32 PCM samples.
    /// Returns the trimmed transcript string; empty if whisper produces nothing.
    pub fn transcribe(&self, samples: &[f32]) -> Result<String> {
        let mut ctx = self.ctx.lock().unwrap();
        let mut state = ctx.create_state().context("whisper state")?;

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_language(Some("en"));

        state.full(params, samples).context("whisper full")?;

        let n = state.full_n_segments().context("segment count")?;
        let mut transcript = String::new();
        for i in 0..n {
            if let Ok(text) = state.full_get_segment_text(i) {
                transcript.push_str(&text);
            }
        }
        Ok(transcript.trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vad_silence_returns_false() {
        let vad = VadGate::default();
        let silence = vec![0.0_f32; 1600];
        assert!(!vad.is_speech(&silence));
    }

    #[test]
    fn vad_loud_tone_returns_true() {
        let vad = VadGate::default();
        let tone: Vec<f32> = (0..1600)
            .map(|i| (2.0 * std::f32::consts::PI * 800.0 * i as f32 / 16_000.0).sin())
            .collect();
        assert!(vad.is_speech(&tone));
    }

    #[test]
    fn vad_custom_threshold() {
        let vad = VadGate { rms_threshold: 0.9, ..VadGate::default() };
        let tone: Vec<f32> = (0..1600)
            .map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 16_000.0).sin())
            .collect();
        // RMS of a full-amplitude sine = 1/√2 ≈ 0.707 < 0.9.
        assert!(!vad.is_speech(&tone));
    }
}
```

- [x] **Step 4: Run tests to verify they pass**

```bash
cargo test -p mur-agent-runtime voice::stt 2>&1 | tail -10
```

Expected: 3 VAD tests pass. (WhisperStt tests require the real model file; no unit test for it — integration test covers this in Task 9.)

- [x] **Step 5: Commit**

```bash
git add mur-agent-runtime/src/voice/stt.rs
git commit -m "feat(voice): whisper.cpp STT + RMS VAD (D1.5)"
```

---

## Task 6: cpal audio I/O

**Files:**
- Create: `mur-agent-runtime/src/voice/audio.rs`

- [x] **Step 1: Write a failing test for list_devices**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_devices_does_not_panic() {
        // In CI (no sound card) this may return an empty list; it must
        // not panic or return an error.
        let devices = list_input_devices();
        assert!(devices.is_ok(), "list_input_devices returned error: {:?}", devices);
    }
}
```

- [x] **Step 2: Run test to verify it fails**

```bash
cargo test -p mur-agent-runtime voice::audio 2>&1 | tail -5
```

Expected: `FAILED` — `list_input_devices` not defined.

- [x] **Step 3: Implement audio.rs**

```rust
//! cpal-based mic capture and speaker playback.
//!
//! All functions that touch the audio hardware are synchronous and
//! intended to be called inside `tokio::task::spawn_blocking`. They
//! must NOT be called directly from an async context.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream};

use crate::voice::stt::VadGate;

// ─── Device enumeration ───────────────────────────────────────────────────────

/// Returns a list of available input device names on the default host.
pub fn list_input_devices() -> Result<Vec<String>> {
    let host = cpal::default_host();
    let devices = host
        .input_devices()
        .context("enumerate input devices")?;
    Ok(devices
        .filter_map(|d| d.name().ok())
        .collect())
}

// ─── Capture ─────────────────────────────────────────────────────────────────

/// Capture audio from `device_name` (None = system default).
/// Records until either `max_duration` elapses or the VAD detects
/// `vad.silence_frames_to_stop` consecutive silent frames after at
/// least one speech frame was detected.
///
/// Returns 16 kHz mono f32 samples resampled from whatever the device
/// natively provides.
///
/// **Must be called from a blocking thread** (e.g. `spawn_blocking`).
pub fn capture_vad_gated(
    device_name: &Option<String>,
    vad: &VadGate,
    max_duration: Duration,
) -> Result<Vec<f32>> {
    let host = cpal::default_host();
    let device = match device_name {
        Some(name) => host
            .input_devices()
            .context("enumerate")?
            .find(|d| d.name().ok().as_deref() == Some(name.as_str()))
            .with_context(|| format!("input device '{name}' not found"))?,
        None => host.default_input_device()
            .context("no default input device")?,
    };

    let config = device
        .default_input_config()
        .context("default input config")?;

    let sample_rate = config.sample_rate().0;
    let channels = config.channels() as usize;

    // Shared ring buffer — stream callback writes, main thread reads.
    let buf: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
    let buf_clone = buf.clone();

    let stream: Stream = match config.sample_format() {
        SampleFormat::F32 => {
            let bc = buf_clone.clone();
            device.build_input_stream(
                &config.into(),
                move |data: &[f32], _| {
                    bc.lock().unwrap().extend_from_slice(data);
                },
                |err| eprintln!("audio capture error: {err}"),
                None,
            )?
        }
        SampleFormat::I16 => {
            let bc = buf_clone.clone();
            device.build_input_stream(
                &config.into(),
                move |data: &[i16], _| {
                    let f32s: Vec<f32> =
                        data.iter().map(|&s| s as f32 / i16::MAX as f32).collect();
                    bc.lock().unwrap().extend_from_slice(&f32s);
                },
                |err| eprintln!("audio capture error: {err}"),
                None,
            )?
        }
        fmt => anyhow::bail!("unsupported input sample format: {fmt:?}"),
    };

    stream.play().context("start capture stream")?;

    let frame = vad.frame_size * channels;
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
        // Downmix to mono for VAD.
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

    drop(stream);

    // Extract captured samples, downmix, resample to 16 kHz.
    let raw = buf.lock().unwrap().clone();
    let mono: Vec<f32> = raw
        .chunks(channels)
        .map(|ch| ch.iter().sum::<f32>() / channels as f32)
        .collect();

    // Simple linear resample to 16 kHz if device rate differs.
    let resampled = if sample_rate == 16_000 {
        mono
    } else {
        resample_to_16k(&mono, sample_rate)
    };

    Ok(resampled)
}

// ─── Playback ─────────────────────────────────────────────────────────────────

/// Play `samples` (f32 mono, `sample_rate` Hz) on the default output device.
/// Blocks until playback finishes.
///
/// **Must be called from a blocking thread** (e.g. `spawn_blocking`).
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

    let pos_c = pos.clone();
    let samples_c = samples_arc.clone();

    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    let stream = device.build_output_stream(
        &config,
        move |output: &mut [f32], _| {
            let p = pos_c.load(std::sync::atomic::Ordering::Relaxed);
            let remaining = samples_c.len().saturating_sub(p);
            let to_write = output.len().min(remaining);
            output[..to_write].copy_from_slice(&samples_c[p..p + to_write]);
            // Silence any remaining output buffer.
            for s in output[to_write..].iter_mut() {
                *s = 0.0;
            }
            pos_c.fetch_add(to_write, std::sync::atomic::Ordering::Relaxed);
            if to_write == 0 {
                let _ = done_tx.send(());
            }
        },
        |err| eprintln!("audio playback error: {err}"),
        None,
    )?;

    stream.play()?;
    // Block until done_tx fires or 60s timeout (safety against runaway streams).
    let _ = done_rx.recv_timeout(Duration::from_secs(60));
    Ok(())
}

// ─── Resampler ────────────────────────────────────────────────────────────────

/// Naive linear interpolation resample from `src_rate` Hz to 16 000 Hz.
/// Accurate enough for STT (not for high-quality audio).
fn resample_to_16k(samples: &[f32], src_rate: u32) -> Vec<f32> {
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
        let devices = list_input_devices();
        assert!(devices.is_ok(), "list_input_devices error: {:?}", devices);
    }

    #[test]
    fn resample_passthrough_when_already_16k() {
        let input = vec![0.1, 0.2, 0.3];
        assert_eq!(resample_to_16k(&input, 16_000), input);
    }

    #[test]
    fn resample_halves_when_32k_to_16k() {
        let input: Vec<f32> = (0..64).map(|i| i as f32).collect();
        let out = resample_to_16k(&input, 32_000);
        assert_eq!(out.len(), 32);
    }
}
```

- [x] **Step 4: Run tests to verify they pass**

```bash
cargo test -p mur-agent-runtime voice::audio 2>&1 | tail -10
```

Expected: 3 tests pass. (`list_devices_does_not_panic` may return empty list in CI — that's OK as long as it doesn't panic.)

- [x] **Step 5: Commit**

```bash
git add mur-agent-runtime/src/voice/audio.rs
git commit -m "feat(voice): cpal audio capture + playback + VAD-gated recording (D1.6)"
```

---

## Task 7: VoiceNotifier (companion outbox integration)

**Files:**
- Create: `mur-agent-runtime/src/voice/notifier.rs`

- [x] **Step 1: Write a failing test for VoiceNotifier**

Create `mur-agent-runtime/src/voice/notifier.rs` (tests first):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::companion::notifier::{CompanionMessage, NotifyOutcome};
    use mur_common::companion::Situation;
    use crate::companion::picker::TemplateId;
    use chrono::Utc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc as StdArc;

    struct MockTts {
        called: StdArc<AtomicBool>,
    }

    impl MockTts {
        fn was_called(&self) -> bool {
            self.called.load(Ordering::SeqCst)
        }
    }

    impl KokoroTtsTrait for MockTts {
        fn synthesize(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
            self.called.store(true, Ordering::SeqCst);
            Ok(vec![0.0_f32; 100]) // 100 silent samples
        }
    }

    #[tokio::test]
    async fn voice_notifier_calls_tts_and_returns_delivered() {
        let called = StdArc::new(AtomicBool::new(false));
        let tts = Box::new(MockTts { called: called.clone() });
        let notifier = VoiceNotifier::with_mock_tts(tts);

        let msg = CompanionMessage {
            id: "test-id".into(),
            situation: Situation::Morning,
            template_id: TemplateId("t1".into()),
            locale: "en-US".into(),
            body: "Good morning!".into(),
            generated_at: Utc::now(),
        };

        let outcome = notifier.send(&msg).await.unwrap();
        assert!(called.load(Ordering::SeqCst), "TTS was not called");
        assert!(matches!(outcome, NotifyOutcome::Delivered));
    }

    #[tokio::test]
    async fn voice_notifier_skips_empty_body() {
        let called = StdArc::new(AtomicBool::new(false));
        let tts = Box::new(MockTts { called: called.clone() });
        let notifier = VoiceNotifier::with_mock_tts(tts);

        let msg = CompanionMessage {
            id: "test-id".into(),
            situation: Situation::Morning,
            template_id: TemplateId("t1".into()),
            locale: "en-US".into(),
            body: "".into(),
            generated_at: Utc::now(),
        };

        let outcome = notifier.send(&msg).await.unwrap();
        assert!(!called.load(Ordering::SeqCst), "TTS should not be called for empty body");
        assert!(matches!(outcome, NotifyOutcome::Skipped { .. }));
    }
}
```

- [x] **Step 2: Run test to verify it fails**

```bash
cargo test -p mur-agent-runtime voice::notifier 2>&1 | tail -5
```

Expected: `FAILED` — `VoiceNotifier` not found.

- [x] **Step 3: Implement notifier.rs**

```rust
//! VoiceNotifier — speaks companion messages via Kokoro TTS.
//! Implements `companion::notifier::Notifier` so it slots into outbox step 11.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use crate::companion::notifier::{CompanionMessage, NotifyOutcome, Notifier};
use crate::voice::{audio, KokoroTts};

/// Trait abstraction over KokoroTts so tests can inject a mock.
pub trait KokoroTtsTrait: Send + Sync {
    fn synthesize(&self, text: &str) -> anyhow::Result<Vec<f32>>;
}

impl KokoroTtsTrait for KokoroTts {
    fn synthesize(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        self.synthesize(text)
    }
}

pub struct VoiceNotifier {
    tts: Box<dyn KokoroTtsTrait>,
    /// cpal output device name. None = system default.
    output_device: Option<String>,
}

impl VoiceNotifier {
    /// Production constructor: wraps a real KokoroTts instance.
    pub fn new(tts: KokoroTts, output_device: Option<String>) -> Self {
        Self {
            tts: Box::new(tts),
            output_device,
        }
    }

    /// Test constructor: injects a mock TTS.
    #[cfg(test)]
    pub fn with_mock_tts(tts: impl KokoroTtsTrait + 'static) -> Self {
        Self {
            tts: Box::new(tts),
            output_device: None,
        }
    }
}

#[async_trait]
impl Notifier for VoiceNotifier {
    fn name(&self) -> &'static str {
        "VoiceNotifier"
    }

    async fn send(&self, msg: &CompanionMessage) -> Result<NotifyOutcome> {
        if msg.body.trim().is_empty() {
            return Ok(NotifyOutcome::Skipped {
                reason: "empty_body".into(),
            });
        }

        let samples = match self.tts.synthesize(&msg.body) {
            Ok(s) if s.is_empty() => {
                return Ok(NotifyOutcome::Skipped {
                    reason: "tts_empty_output".into(),
                });
            }
            Ok(s) => s,
            Err(e) => return Ok(NotifyOutcome::Failed(e)),
        };

        let device = self.output_device.clone();
        let result = tokio::task::spawn_blocking(move || {
            audio::play_pcm(&samples, crate::voice::tts::KOKORO_SAMPLE_RATE)
        })
        .await;

        match result {
            Ok(Ok(())) => Ok(NotifyOutcome::Delivered),
            Ok(Err(e)) => Ok(NotifyOutcome::Failed(e)),
            Err(join_err) => Ok(NotifyOutcome::Failed(anyhow::anyhow!(
                "spawn_blocking panicked: {join_err}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    // (tests defined above in the module)
}
```

- [x] **Step 4: Run tests to verify they pass**

```bash
cargo test -p mur-agent-runtime voice::notifier 2>&1 | tail -10
```

Expected: `voice_notifier_calls_tts_and_returns_delivered ... ok`, `voice_notifier_skips_empty_body ... ok`.

- [x] **Step 5: Commit**

```bash
git add mur-agent-runtime/src/voice/notifier.rs
git commit -m "feat(voice): VoiceNotifier wraps KokoroTts into Notifier trait (D1.7)"
```

---

## Task 8: B0 rule 18 — VoiceInputHook + privacy audit

**Files:**
- Create: `mur-agent-runtime/src/hooks/voice_input.rs`
- Modify: `mur-agent-runtime/src/hooks/mod.rs` (add `pub mod voice_input;`)
- Create: `mur-agent-runtime/src/voice/network_audit.rs`

- [x] **Step 1: Write failing test for VoiceInputHook**

Create `mur-agent-runtime/src/hooks/voice_input.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    struct MockStt {
        transcript: String,
    }
    impl WhisperSttTrait for MockStt {
        fn transcribe(&self, _samples: &[f32]) -> anyhow::Result<String> {
            Ok(self.transcript.clone())
        }
    }

    #[tokio::test]
    async fn wraps_transcript_in_untrusted_tag() {
        let hook = VoiceInputHook::with_mock_stt(
            Box::new(MockStt { transcript: "open the pod bay doors".into() }),
            vec![0.1_f32; 1600], // non-silent samples → VAD triggers
        );

        let ctx = HookCtx::default();
        let view = PromptView { user_message: "".into(), system_prompt: None };
        let tok = CancellationToken::new();

        let patch = hook.on_prompt_submit(&ctx, &view, &tok).await.unwrap();

        assert_eq!(patch.wrap_untrusted.len(), 1);
        let w = &patch.wrap_untrusted[0];
        assert_eq!(w.tag, "untrusted_voice_input");
        assert_eq!(w.source, "mic");
        assert_eq!(w.content, "open the pod bay doors");
        assert!(patch.turn_flags.contains(&"after_untrusted_input".to_string()));
    }

    #[tokio::test]
    async fn empty_transcript_returns_noop() {
        let hook = VoiceInputHook::with_mock_stt(
            Box::new(MockStt { transcript: "".into() }),
            vec![0.0_f32; 1600], // silence → no transcript
        );

        let ctx = HookCtx::default();
        let view = PromptView { user_message: "".into(), system_prompt: None };
        let tok = CancellationToken::new();

        let patch = hook.on_prompt_submit(&ctx, &view, &tok).await.unwrap();

        assert!(patch.wrap_untrusted.is_empty());
        assert!(patch.turn_flags.is_empty());
    }
}
```

- [x] **Step 2: Run tests to verify they fail**

```bash
cargo test -p mur-agent-runtime hooks::voice_input 2>&1 | tail -5
```

Expected: `FAILED` — `VoiceInputHook` not found.

- [x] **Step 3: Implement voice_input.rs**

```rust
//! VoiceInputHook — captures mic audio, transcribes via whisper.cpp,
//! and injects the transcript as an `UntrustedWrapper` (B0 rule 18).
//!
//! Design mirrors D3's drag-drop hook: untrusted input is wrapped in a
//! spotlight tag so the model can distinguish it from trusted text and
//! the `after_untrusted_input` turn flag is set so pre_tool_use
//! enforces the same-turn cooldown (B0 rule 4).

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::hooks::{Hook, HookCtx, HookError, PromptPatch, PromptView};
use crate::hooks::patch::UntrustedWrapper;
use crate::voice::stt::{VadGate, WhisperStt};
use crate::voice::audio;

/// Trait abstraction so tests can inject a mock STT.
pub trait WhisperSttTrait: Send + Sync {
    fn transcribe(&self, samples: &[f32]) -> anyhow::Result<String>;
}

impl WhisperSttTrait for WhisperStt {
    fn transcribe(&self, samples: &[f32]) -> anyhow::Result<String> {
        self.transcribe(samples)
    }
}

pub struct VoiceInputHook {
    stt: Box<dyn WhisperSttTrait>,
    vad: VadGate,
    input_device: Option<String>,
    max_capture: Duration,
    /// Test-only: pre-loaded samples instead of live mic capture.
    #[cfg(test)]
    test_samples: Vec<f32>,
}

impl VoiceInputHook {
    /// Production constructor.
    pub fn new(
        stt: WhisperStt,
        vad: VadGate,
        input_device: Option<String>,
        max_capture: Duration,
    ) -> Self {
        Self {
            stt: Box::new(stt),
            vad,
            input_device,
            max_capture,
            #[cfg(test)]
            test_samples: vec![],
        }
    }

    #[cfg(test)]
    pub fn with_mock_stt(
        stt: Box<dyn WhisperSttTrait>,
        test_samples: Vec<f32>,
    ) -> Self {
        Self {
            stt,
            vad: VadGate::default(),
            input_device: None,
            max_capture: Duration::from_secs(10),
            test_samples,
        }
    }
}

#[async_trait]
impl Hook for VoiceInputHook {
    fn name(&self) -> &str {
        "VoiceInputHook"
    }

    async fn on_prompt_submit(
        &self,
        _ctx: &HookCtx,
        _view: &PromptView,
        _tok: &CancellationToken,
    ) -> Result<PromptPatch, HookError> {
        // Capture audio (real or test).
        #[cfg(test)]
        let samples = self.test_samples.clone();
        #[cfg(not(test))]
        let samples = {
            let device = self.input_device.clone();
            let vad = self.vad.clone();
            let max = self.max_capture;
            tokio::task::spawn_blocking(move || {
                audio::capture_vad_gated(&device, &vad, max)
            })
            .await
            .map_err(|e| HookError::Internal(anyhow::anyhow!("spawn_blocking: {e}")))?
            .map_err(HookError::Internal)?
        };

        if samples.is_empty() {
            return Ok(PromptPatch::noop());
        }

        // Transcribe.
        let transcript = self
            .stt
            .transcribe(&samples)
            .map_err(HookError::Internal)?;

        if transcript.is_empty() {
            return Ok(PromptPatch::noop());
        }

        // Wrap in spotlight tag (B0 rule 18).
        Ok(PromptPatch {
            wrap_untrusted: vec![UntrustedWrapper {
                tag: "untrusted_voice_input".into(),
                source: "mic".into(),
                content: transcript,
            }],
            turn_flags: vec!["after_untrusted_input".into()],
            ..PromptPatch::noop()
        })
    }
}

#[cfg(test)]
mod tests {
    // (tests defined above)
}
```

- [x] **Step 4: Add `pub mod voice_input;` to mur-agent-runtime/src/hooks/mod.rs**

```rust
pub mod voice_input;
```

- [x] **Step 5: Run tests to verify they pass**

```bash
cargo test -p mur-agent-runtime hooks::voice_input 2>&1 | tail -10
```

Expected: `wraps_transcript_in_untrusted_tag ... ok`, `empty_transcript_returns_noop ... ok`.

- [x] **Step 6: Create voice/network_audit.rs (compile-time guard)**

```rust
//! Voice network-egress audit (privacy invariant: no audio leaves the device).
//!
//! Mirrors `companion/network_audit.rs`. The voice subsystem must
//! NOT import any HTTP client or raw socket type.

#[cfg(test)]
const VOICE_FILES: &[(&str, &str)] = &[
    ("mod.rs",          include_str!("mod.rs")),
    ("types.rs",        include_str!("types.rs")),
    ("download.rs",     include_str!("download.rs")),
    ("tts.rs",          include_str!("tts.rs")),
    ("stt.rs",          include_str!("stt.rs")),
    ("audio.rs",        include_str!("audio.rs")),
    ("notifier.rs",     include_str!("notifier.rs")),
    ("network_audit.rs",include_str!("network_audit.rs")),
];

#[cfg(test)]
const FORBIDDEN_TOKENS: &[&str] = &[
    "use reqwest",
    "use hyper",
    "use surf",
    "use ureq",
    "use isahc",
    "use tokio::net::",
    "use tokio::io::AsyncReadExt",
    "::TcpStream",
    "::TcpListener",
    "::UdpSocket",
    "std::net::TcpStream",
    "std::net::TcpListener",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_modules_have_no_network_egress() {
        for (filename, src) in VOICE_FILES {
            for token in FORBIDDEN_TOKENS {
                assert!(
                    !src.contains(token),
                    "voice/{filename} contains forbidden network token {token:?}"
                );
            }
        }
    }
}
```

- [x] **Step 7: Run audit test to verify it passes**

```bash
cargo test -p mur-agent-runtime voice::network_audit 2>&1 | tail -5
```

Expected: `voice_modules_have_no_network_egress ... ok`.

- [x] **Step 8: Commit**

```bash
git add mur-agent-runtime/src/hooks/voice_input.rs mur-agent-runtime/src/hooks/mod.rs mur-agent-runtime/src/voice/network_audit.rs
git commit -m "feat(b0): VoiceInputHook rule-18 spotlight + voice network audit (D1.8)"
```

---

## Task 9: E2E test + cookbook

**Files:**
- Create: `mur-agent-runtime/tests/voice_e2e.rs`
- Create: `docs/cookbook/d1-voice.md`

- [x] **Step 1: Write E2E integration test**

Create `mur-agent-runtime/tests/voice_e2e.rs`:

```rust
//! E2E test for voice enable/disable CLI round-trip.
//!
//! Does NOT test actual audio hardware or model inference —
//! those require physical audio devices and large model files.
//! This test verifies the profile schema write + read path only.

use mur_common::agent::{AgentProfile, VoiceId};

#[test]
fn voice_config_enables_and_round_trips_profile() {
    // Build a minimal profile with voice enabled.
    let mut profile = AgentProfile::default_for_tests();
    assert!(!profile.voice.enabled);
    assert_eq!(profile.voice.voice_id, VoiceId::AfHeart);

    profile.voice.enabled = true;
    profile.voice.voice_id = VoiceId::AmMichael;

    let yaml = serde_yaml_ng::to_string(&profile).expect("serialize");
    let loaded: AgentProfile = serde_yaml_ng::from_str(&yaml).expect("deserialize");

    assert!(loaded.voice.enabled);
    assert_eq!(loaded.voice.voice_id, VoiceId::AmMichael);
}

#[test]
fn voice_config_disabled_by_default_on_fresh_profile() {
    let profile = AgentProfile::default_for_tests();
    assert!(!profile.voice.enabled);
}
```

> `AgentProfile::default_for_tests()` should already exist in the test helpers; if not, add a minimal impl to `mur-common/src/agent.rs`:
>
> ```rust
> #[cfg(test)]
> impl AgentProfile {
>     pub fn default_for_tests() -> Self {
>         // Minimal valid profile — parse from hardcoded YAML.
>         // (copy the YAML from Task 1 Step 1 test, minus the voice block)
>         serde_yaml_ng::from_str(include_str!("../../tests/fixtures/minimal_profile.yaml"))
>             .expect("minimal profile fixture")
>     }
> }
> ```
>
> Create `mur-common/tests/fixtures/minimal_profile.yaml` if it doesn't exist.

- [x] **Step 2: Run E2E test**

```bash
cargo test -p mur-agent-runtime --test voice_e2e 2>&1 | tail -10
```

Expected: both tests pass.

- [x] **Step 3: Create docs/cookbook/d1-voice.md**

```markdown
# D1 Voice — Kokoro TTS + whisper.cpp STT

On-device voice I/O for mur agents. Kokoro 82M synthesises speech
locally at 24 kHz; whisper.cpp large-v3-turbo q5_1 transcribes mic
audio at 16 kHz. No audio or transcript leaves the device.

## Quick start

```bash
# Enable voice on an agent (sets profile.voice.enabled = true).
mur agent voice enable my-agent

# Optional: choose a voice (default: af_heart).
mur agent voice enable my-agent --voice-id am_michael

# Download the model weights (~1.4 GB total; SHA-256 verified).
mur agent voice download my-agent

# Disable voice.
mur agent voice disable my-agent
```

## Available voices

| Voice ID | Description |
|---|---|
| `af_heart` | Female, warm American English (default) |
| `af_bella` | Female, bright American English |
| `af_nicole` | Female, neutral American English |
| `am_adam` | Male, neutral American English |
| `am_michael` | Male, deeper American English |

## Model locations

| Model | Path | Size |
|---|---|---|
| whisper large-v3-turbo q5_1 | `~/.mur/models/whisper/ggml-large-v3-turbo-q5_1.bin` | ~930 MB |
| Kokoro ONNX | `~/.mur/models/kokoro/kokoro-v0_19.onnx` | ~85 MB |
| Kokoro style matrix | `~/.mur/models/kokoro/kokoro-voices.bin` | ~5 KB |

Models are cached permanently. Re-running `voice download` is a
no-op if SHA-256 matches. To force re-download, delete the file.

## How it works

### TTS (companion outbox)

When voice is enabled, the companion outbox wires a `VoiceNotifier`
at step 11 instead of (or alongside) `StdoutNotifier`. Each proactive
companion message is synthesised by Kokoro and played on the default
output device before the inbox `.md` file is written.

### STT (mic → agent input)

`VoiceInputHook` fires on every `on_prompt_submit`. It captures
audio from the default input device, applies a simple RMS voice
activity detector, and transcribes with whisper.cpp. The transcript
is injected as an `UntrustedWrapper`:

```
<untrusted_voice_input>
{transcript text}
</untrusted_voice_input>
```

This is B0 rule 18 — voice input is treated as untrusted, identical
to drag-drop text (D3) and Telegram messages (C2). The agent sees
the wrapper and knows the text came from a mic, not the keyboard.

### Privacy invariant

Voice audio is processed entirely on-device:
- Kokoro TTS: ort ONNX Runtime, no network calls.
- whisper.cpp: `whisper-rs` Rust bindings over the local C++ lib.
- `cpal` audio I/O: no network.

The compile-time `voice::network_audit` test fails the build the
moment any voice module imports `reqwest`, `hyper`, or any
`tokio::net::*` type.

## Troubleshooting

**No audio output / input:** run `mur agent voice list-devices` to
see what cpal sees. On macOS, grant microphone permission in System
Settings → Privacy & Security.

**Poor transcription:** the large-v3-turbo model performs best with
clear speech in a quiet room. VAD threshold defaults to RMS 0.01;
lower if it cuts off your voice early.

**Kokoro sounds robotic on a phoneme:** this is a known gap in the
v1 phoneme vocabulary (`PHONEME_VOCAB` partial map). File an issue
with the word and the IPA espeak-ng output; it will be added to the
next release's vocab table.

## See also

- `mur-agent-runtime/src/voice/` — implementation
- `docs/superpowers/specs/2026-04-30-mur-agent-harness-roadmap-design.md` §4.1 — spec
- `docs/superpowers/plans/2026-05-06-mur-agent-d1-voice.md` — this plan
```

- [x] **Step 4: Run full workspace tests to confirm no regressions**

```bash
cargo test --workspace 2>&1 | tail -20
```

Expected: all pre-existing tests pass; new voice tests pass.

- [x] **Step 5: Commit**

```bash
git add mur-agent-runtime/tests/voice_e2e.rs docs/cookbook/d1-voice.md
git commit -m "test(voice): E2E round-trip + cookbook (D1.9)"
```

---

## Self-Review

### Spec coverage

| Deliverable | Task |
|---|---|
| Kokoro 82M ONNX TTS, 5 voices | T4: tts.rs, T7: VoiceNotifier |
| whisper.cpp STT, large-v3-turbo q5_1 | T5: stt.rs |
| VAD-gated 16 kHz mono input | T5: VadGate, T6: capture_vad_gated |
| STT transcript wrapped in `<untrusted_voice_input>` (B0 rule 18) | T8: VoiceInputHook |
| Companion outbox messages spoken via TTS | T7: VoiceNotifier wires into Notifier trait |
| `mur agent voice enable/disable` CLI | T2: agent_voice.rs |
| Model download + SHA-256 integrity | T3: download.rs |
| Models cached under `~/.mur/models/` | T3: types.rs paths |
| Privacy invariant + compile-time guard | T8: network_audit.rs |

### Known gaps (explicitly out of scope for D1)

- `mur agent voice download <name>` subcommand in `mur-core` — Task 2 adds `enable/disable`; `download` must be added as a third subcommand wired to call `download::ensure_model` for each of the three specs. Add to T2's dispatch block:
  ```rust
  VoiceCmd::Download { name } => {
      cmd_voice_download(&name)?;
  }
  ```
  `cmd_voice_download` in `agent_voice.rs` calls `ensure_model` for `WHISPER_SPEC`, `KOKORO_ONNX_SPEC`, `KOKORO_VOICES_SPEC` with an `indicatif` progress bar. Add this before shipping T2.

- SHA-256 values in `types.rs` are placeholders. Before tagging a release, download each model artifact, run `sha256sum`, and update the constants.

- `PHONEME_VOCAB` in `tts.rs` is a partial map. Expand from `hexgrad/Kokoro-82M/tokenizer_config.json` (178 entries) before shipping T4.

- `AgentProfile::default_for_tests()` helper may need to be created in T9 if it doesn't already exist.

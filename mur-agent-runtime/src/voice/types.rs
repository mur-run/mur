//! Model path helpers and download specs.

use std::path::PathBuf;
use crate::voice::download::ModelSpec;

/// Resolved on-disk paths for both voice models.
#[derive(Debug)]
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
///
/// SHA-256 is a placeholder — update to the real artifact hash before shipping.
pub const WHISPER_SPEC: ModelSpec = ModelSpec {
    name: "ggml-large-v3-turbo-q5_1.bin",
    url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/4774d2f/ggml-large-v3-turbo-q5_1.bin",
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

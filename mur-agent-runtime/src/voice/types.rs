//! Model path helpers and download specs.
//!
//! The whisper STT weights and the Kokoro TTS ONNX model are downloaded
//! verbatim from Hugging Face. The Kokoro **voice style matrix**
//! (`kokoro-voices.bin`) is not an upstream artifact — it is built locally by
//! the CLI by concatenating five per-voice `[512, 256]` style tensors (one per
//! [`mur_common::agent::VoiceId`], in `style_index()` order). See
//! `mur-core` `cmd::agent_voice::cmd_voice_download`.

use crate::voice::download::ModelSpec;
use std::path::PathBuf;

/// Number of voices packed into `kokoro-voices.bin` (matches `VoiceId`).
pub const N_VOICES: usize = 5;
/// Rows per voice style tensor (Kokoro v0.19 token-length buckets).
pub const STYLE_ROWS: usize = 512;
/// Style vector dimension.
pub const STYLE_DIM: usize = 256;
/// Total byte length of a valid `kokoro-voices.bin` (`5 × 512 × 256 × f32`).
pub const VOICES_BIN_LEN: usize = N_VOICES * STYLE_ROWS * STYLE_DIM * 4;

/// Resolved on-disk paths for the voice models.
#[derive(Debug)]
pub struct VoiceModelPaths {
    /// Path to ggml-large-v3-turbo-q5_0.bin (whisper.cpp format).
    pub whisper: PathBuf,
    /// Path to kokoro-v0_19.onnx.
    pub kokoro_onnx: PathBuf,
    /// Path to kokoro-voices.bin (style matrix, 5 × 512 × 256 f32).
    pub kokoro_voices: PathBuf,
}

impl VoiceModelPaths {
    /// Build paths under `~/.mur/models/`.
    pub fn from_mur_root(mur_root: &std::path::Path) -> Self {
        let models = mur_root.join("models");
        Self {
            whisper: models.join("whisper/ggml-large-v3-turbo-q5_0.bin"),
            kokoro_onnx: models.join("kokoro/kokoro-v0_19.onnx"),
            kokoro_voices: models.join("kokoro/kokoro-voices.bin"),
        }
    }

    /// Returns true only if all three model files exist on disk.
    pub fn all_present(&self) -> bool {
        self.whisper.exists() && self.kokoro_onnx.exists() && self.kokoro_voices.exists()
    }
}

/// Download spec for whisper large-v3-turbo q5_0 (~574 MB).
///
/// whisper.cpp autodetects the quantization from the GGML header, so the
/// on-disk file name is cosmetic; q5_0 is the smallest turbo build upstream
/// ships (there is no q5_1 artifact).
pub const WHISPER_SPEC: ModelSpec = ModelSpec {
    name: "ggml-large-v3-turbo-q5_0.bin",
    url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q5_0.bin",
    sha256: "394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2",
    subdir: "whisper",
};

/// Download spec for the Kokoro v0.19 ONNX model (quantized, ~92 MB).
///
/// Saved on disk as `kokoro-v0_19.onnx`. Inputs: `tokens` int64[1,N],
/// `style` f32[1,256], `speed` f32[1,1]; output `audio` f32[samples].
pub const KOKORO_ONNX_SPEC: ModelSpec = ModelSpec {
    name: "kokoro-v0_19.onnx",
    url: "https://huggingface.co/onnx-community/Kokoro-82M-ONNX/resolve/main/onnx/model_quantized.onnx",
    sha256: "0d55b15d4b735d61a21b0105136bc81b8768c4db94753193c19354fa863cd556",
    subdir: "kokoro",
};

/// Per-voice style tensors that are concatenated into `kokoro-voices.bin`.
///
/// **Order is significant**: the array index must equal
/// [`mur_common::agent::VoiceId::style_index`]. Each source file is a
/// `[512, 256]` f32 tensor (524288 bytes).
///
/// The default `AfHeart` slot is backed by the v0.19 `af` voice: af_heart is a
/// v1.0 voice and has no v0.19 tensor, and Kokoro style vectors are not
/// interchangeable across model versions (they live in the decoder's latent
/// space). `af` is af_heart's v0.19 lineage (the default American female), so
/// it is the correct version-consistent choice for the v0.19 ONNX model.
/// When/if this stack moves to v1.0 ONNX, swap in the real `af_heart.bin`.
pub const KOKORO_VOICE_SOURCES: [ModelSpec; N_VOICES] = [
    // slot 0 — VoiceId::AfHeart (served by v0.19 `af`)
    ModelSpec {
        name: "af.bin",
        url: "https://huggingface.co/onnx-community/Kokoro-82M-ONNX/resolve/main/voices/af.bin",
        sha256: "a4f11d9d055a12bfa0db2668a3e4f0ef8fd1f1ccca69494479718e44dbf9e41a",
        subdir: "kokoro/voices",
    },
    // slot 1 — VoiceId::AfBella
    ModelSpec {
        name: "af_bella.bin",
        url: "https://huggingface.co/onnx-community/Kokoro-82M-ONNX/resolve/main/voices/af_bella.bin",
        sha256: "38e12d4b9b31a751282ab9154fb083dad3df3a749772687cde7873da107160fa",
        subdir: "kokoro/voices",
    },
    // slot 2 — VoiceId::AfNicole
    ModelSpec {
        name: "af_nicole.bin",
        url: "https://huggingface.co/onnx-community/Kokoro-82M-ONNX/resolve/main/voices/af_nicole.bin",
        sha256: "f27666996f2d227711e97ff27374099df6f619f3bfb60cd67fc0d114f5384d13",
        subdir: "kokoro/voices",
    },
    // slot 3 — VoiceId::AmAdam
    ModelSpec {
        name: "am_adam.bin",
        url: "https://huggingface.co/onnx-community/Kokoro-82M-ONNX/resolve/main/voices/am_adam.bin",
        sha256: "6d5255a4b4803f594bfa0c0d7539c4d8bb0829c7f190f0f6a8fa0afa0023b6e4",
        subdir: "kokoro/voices",
    },
    // slot 4 — VoiceId::AmMichael
    ModelSpec {
        name: "am_michael.bin",
        url: "https://huggingface.co/onnx-community/Kokoro-82M-ONNX/resolve/main/voices/am_michael.bin",
        sha256: "9c3be118019ddb41b6b529a7f75c7a3dc92613f573ca03b037748b9383b0d9d0",
        subdir: "kokoro/voices",
    },
];

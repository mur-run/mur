//! Voice manifest — signed JSON describing a voice asset bundle.
//!
//! Hosted at `<MUR_VOICE_CDN_BASE>/<voice_id>/manifest.json` with a
//! detached Ed25519 signature at `manifest.json.sig`.
//!
//! The verifying public key is **pinned at build time** via the
//! `MUR_VOICE_PUBKEY` env var. Rotation requires a binary release.
//! Dev builds get a placeholder via `build.rs`; production releases
//! MUST override `MUR_VOICE_PUBKEY` in CI before the Tauri build.
//!
//! Schema is shared between voice packs and the STT model bundle —
//! see roadmap §4.1 + the M1 plan §M1.2.1 for the design rationale.

use anyhow::{Context, Result, bail};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Compile-time pinned voice-manifest verifying key (multibase z…).
pub const PINNED_VOICE_PUBKEY: &str = env!("MUR_VOICE_PUBKEY");

/// Tagged union of asset-bundle manifests. New `kind` values are
/// additive and must default-deserialise on existing clients.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum AssetBundle {
    Voice(VoiceManifest),
    SttModel(SttModelManifest),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceManifest {
    pub schema_version: u32,
    pub voice_id: String,
    pub display_name: String,
    /// BCP-47, e.g. `en-US` or `zh-TW`.
    pub language: String,
    /// Kokoro is 24 kHz; we resample to 22.05 kHz for playback.
    pub sample_rate_hz: u32,
    /// SPDX license id, e.g. "MIT".
    pub license: String,
    pub creator: String,
    pub assets: Vec<AssetEntry>,
    pub size_bytes_total: u64,
}

/// STT model manifest. Same signing + SHA-256 + atomic-rename
/// download path as voices, but lives under a separate registry slot
/// (`<app_data>/voices/_stt/<model_id>/`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SttModelManifest {
    pub schema_version: u32,
    pub model_id: String,
    pub display_name: String,
    /// "whisper.cpp" only in v1.
    pub backend: String,
    /// BCP-47 list of languages the model supports well.
    pub languages: Vec<String>,
    pub license: String,
    pub assets: Vec<AssetEntry>,
    pub size_bytes_total: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetEntry {
    pub name: String,
    /// Lowercase hex of the asset's SHA-256.
    pub sha256_hex: String,
    pub size_bytes: u64,
    /// Absolute https URL.
    pub url: String,
}

/// Verify the detached signature against `manifest_bytes` using the
/// pinned pubkey. Returns the parsed bundle on success.
pub fn verify_and_parse(manifest_bytes: &[u8], sig_bytes: &[u8]) -> Result<AssetBundle> {
    verify_signature(manifest_bytes, sig_bytes)?;
    serde_json::from_slice(manifest_bytes).context("voice manifest is not valid JSON")
}

/// Just verify the signature without parsing — useful when the caller
/// already deserialises into a specific variant.
pub fn verify_signature(manifest_bytes: &[u8], sig_bytes: &[u8]) -> Result<()> {
    if sig_bytes.len() != 64 {
        bail!(
            "voice manifest signature must be 64 bytes; got {}",
            sig_bytes.len()
        );
    }
    let pubkey = decode_pinned_pubkey()?;
    let mut sig_arr = [0u8; 64];
    sig_arr.copy_from_slice(sig_bytes);
    let signature = Signature::from_bytes(&sig_arr);
    pubkey
        .verify(manifest_bytes, &signature)
        .context("voice manifest signature verification failed")?;
    Ok(())
}

/// SHA-256 hex of `bytes`. Used to verify each downloaded asset against
/// the manifest entry.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn decode_pinned_pubkey() -> Result<VerifyingKey> {
    let raw = multibase_decode_z(PINNED_VOICE_PUBKEY)?;
    if raw.len() != 32 {
        bail!(
            "pinned voice pubkey must decode to 32 bytes; got {}",
            raw.len()
        );
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&raw);
    VerifyingKey::from_bytes(&arr).context("invalid pinned voice pubkey")
}

fn multibase_decode_z(s: &str) -> Result<Vec<u8>> {
    let s = s
        .strip_prefix('z')
        .context("expected multibase z prefix on voice pubkey")?;
    bs58::decode(s)
        .into_vec()
        .context("invalid base58 in voice pubkey")
}

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
        // Exercises the signing path; end-to-end pubkey pinning is
        // exercised by integration tests with MUR_VOICE_PUBKEY override.
        let mut csprng = OsRng;
        let key = SigningKey::generate(&mut csprng);
        let payload = br#"{"kind":"voice","schema_version":1,"voice_id":"x","display_name":"X","language":"en-US","sample_rate_hz":24000,"license":"MIT","creator":"test","assets":[],"size_bytes_total":0}"#;
        let sig = key.sign(payload);
        assert!(key.verifying_key().verify(payload, &sig).is_ok());
    }

    #[test]
    fn signature_wrong_length_rejected() {
        let r = verify_signature(b"hello", &[0u8; 32]);
        assert!(r.is_err());
        assert!(format!("{r:?}").contains("64 bytes"));
    }

    #[test]
    fn parse_voice_manifest_round_trip() {
        let m = AssetBundle::Voice(VoiceManifest {
            schema_version: 1,
            voice_id: "af_heart".into(),
            display_name: "Af Heart".into(),
            language: "en-US".into(),
            sample_rate_hz: 24000,
            license: "MIT".into(),
            creator: "hexgrad".into(),
            assets: vec![AssetEntry {
                name: "voice.onnx".into(),
                sha256_hex: "abc".into(),
                size_bytes: 100,
                url: "https://x".into(),
            }],
            size_bytes_total: 100,
        });
        let s = serde_json::to_vec(&m).unwrap();
        let parsed: AssetBundle = serde_json::from_slice(&s).unwrap();
        match parsed {
            AssetBundle::Voice(v) => assert_eq!(v.voice_id, "af_heart"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn parse_stt_model_manifest_round_trip() {
        let m = AssetBundle::SttModel(SttModelManifest {
            schema_version: 1,
            model_id: "whisper-large-v3-turbo-q5_1".into(),
            display_name: "Whisper Large v3 Turbo (q5_1)".into(),
            backend: "whisper.cpp".into(),
            languages: vec!["en".into(), "zh".into()],
            license: "MIT".into(),
            assets: vec![],
            size_bytes_total: 0,
        });
        let s = serde_json::to_vec(&m).unwrap();
        let parsed: AssetBundle = serde_json::from_slice(&s).unwrap();
        match parsed {
            AssetBundle::SttModel(v) => assert_eq!(v.backend, "whisper.cpp"),
            _ => panic!("wrong variant"),
        }
    }
}

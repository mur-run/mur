use crate::hub::style_preset::StylePreset;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// How the pet's frames are produced. `Ai` means real LLM-rendered `.webp`
/// images are on disk; `Vector` means no image model was available and the pet
/// renders the built-in vector mascot (`PetFace`) instead — no `.webp` written.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum RenderMode {
    /// Real LLM-rendered art on disk. Default keeps legacy manifests as `Ai`.
    #[default]
    Ai,
    /// Built-in vector mascot; no rendered `.webp` files.
    Vector,
}

/// Cached record of the last successful expression render for one agent.
/// Written as `<agent_dir>/expressions/manifest.json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PresetManifest {
    pub preset_id: String,
    pub rendered_at: DateTime<Utc>,
    /// SHA-256 hex of the preset YAML at render time; used to detect staleness.
    pub sha256: String,
    /// Expression IDs that were successfully rendered.
    pub expressions: Vec<String>,
    /// Whether `.webp` art exists (`Ai`) or the vector mascot is used (`Vector`).
    /// `#[serde(default)]` keeps pre-existing manifests deserialising as `Ai`.
    #[serde(default)]
    pub mode: RenderMode,
}

/// Path to the manifest file for `agent_dir`.
pub fn manifest_path(agent_dir: &Path) -> PathBuf {
    agent_dir.join("expressions").join("manifest.json")
}

/// SHA-256 hex of the preset's canonical YAML serialization.
pub fn compute_preset_hash(preset: &StylePreset) -> String {
    let yaml = serde_yaml_ng::to_string(preset).expect("preset serialization must not fail");
    let mut hasher = Sha256::new();
    hasher.update(yaml.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Returns `true` when the manifest's recorded hash no longer matches the
/// current preset definition — i.e. a re-render is needed.
pub fn manifest_stale(manifest: &PresetManifest, preset: &StylePreset) -> bool {
    manifest.sha256 != compute_preset_hash(preset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::preset_loader::default_blob;

    #[test]
    fn manifest_not_stale_when_unchanged() {
        let preset = default_blob();
        let hash = compute_preset_hash(&preset);
        let manifest = PresetManifest {
            preset_id: preset.id.clone(),
            rendered_at: Utc::now(),
            sha256: hash,
            expressions: vec!["idle".into()],
            mode: RenderMode::Ai,
        };
        assert!(!manifest_stale(&manifest, &preset));
    }

    #[test]
    fn manifest_stale_when_hash_differs() {
        let preset = default_blob();
        let manifest = PresetManifest {
            preset_id: preset.id.clone(),
            rendered_at: Utc::now(),
            sha256: "aabbccddeeff".into(),
            expressions: vec!["idle".into()],
            mode: RenderMode::Ai,
        };
        assert!(manifest_stale(&manifest, &preset));
    }

    #[test]
    fn hash_changes_when_preset_description_changes() {
        let mut preset = default_blob();
        let hash_before = compute_preset_hash(&preset);
        preset.description = "modified description".into();
        let hash_after = compute_preset_hash(&preset);
        assert_ne!(hash_before, hash_after);
    }

    #[test]
    fn manifest_path_correct() {
        let agent_dir = Path::new("/home/user/.mur/agents/alice");
        let path = manifest_path(agent_dir);
        assert_eq!(
            path,
            Path::new("/home/user/.mur/agents/alice/expressions/manifest.json")
        );
    }

    #[test]
    fn manifest_json_round_trip() {
        let preset = default_blob();
        let manifest = PresetManifest {
            preset_id: preset.id.clone(),
            rendered_at: DateTime::from_timestamp(0, 0).unwrap(),
            sha256: compute_preset_hash(&preset),
            expressions: vec!["idle".into(), "smile".into()],
            mode: RenderMode::Ai,
        };
        let json = serde_json::to_string(&manifest).expect("serialize");
        let back: PresetManifest = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(manifest, back);
    }

    /// Manifests written before `mode` existed must deserialise as `Ai` so
    /// previously-rendered agents keep showing their `.webp` art.
    #[test]
    fn legacy_manifest_without_mode_defaults_to_ai() {
        let json = r#"{"preset_id":"chiikawa","rendered_at":"2026-01-01T00:00:00Z","sha256":"deadbeef","expressions":["idle"]}"#;
        let m: PresetManifest = serde_json::from_str(json).expect("deserialize legacy");
        assert_eq!(m.mode, RenderMode::Ai);
    }

    /// Vector-mode manifests round-trip with the lowercase tag.
    #[test]
    fn vector_mode_round_trips() {
        let preset = default_blob();
        let manifest = PresetManifest {
            preset_id: preset.id.clone(),
            rendered_at: DateTime::from_timestamp(0, 0).unwrap(),
            sha256: compute_preset_hash(&preset),
            expressions: vec![],
            mode: RenderMode::Vector,
        };
        let json = serde_json::to_string(&manifest).expect("serialize");
        assert!(json.contains("\"mode\":\"vector\""));
        let back: PresetManifest = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(manifest, back);
    }
}

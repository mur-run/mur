use crate::hub::style_preset::StylePreset;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

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
        };
        let json = serde_json::to_string(&manifest).expect("serialize");
        let back: PresetManifest = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(manifest, back);
    }
}

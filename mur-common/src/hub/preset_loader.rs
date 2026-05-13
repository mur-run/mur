use crate::hub::style_preset::StylePreset;
use anyhow::Result;
use std::path::Path;

const BUILTIN_YAMLS: &[&str] = &[
    include_str!("builtin_presets/chiikawa.yaml"),
    include_str!("builtin_presets/sanrio-pastel.yaml"),
    include_str!("builtin_presets/sumikko.yaml"),
    include_str!("builtin_presets/shimeji-retro.yaml"),
    include_str!("builtin_presets/vtuber-soft.yaml"),
    include_str!("builtin_presets/family-photo.yaml"),
    include_str!("builtin_presets/default-blob.yaml"),
];

/// Returns all 7 built-in presets (including `default-blob`), parsed at
/// compile time via `include_str!`. Panics if any YAML is malformed — that
/// would be a build-time bug, not a runtime failure.
pub fn load_builtin_presets() -> Vec<StylePreset> {
    BUILTIN_YAMLS
        .iter()
        .map(|s| serde_yaml_ng::from_str(s).expect("malformed builtin preset YAML"))
        .collect()
}

/// Loads user-defined presets from `<hub_dir>/presets/*.yaml`.
/// Returns an empty vec (not an error) if the directory does not exist.
pub fn load_user_presets(hub_dir: &Path) -> Result<Vec<StylePreset>> {
    let presets_dir = hub_dir.join("presets");
    if !presets_dir.exists() {
        return Ok(vec![]);
    }
    let mut presets = Vec::new();
    for entry in std::fs::read_dir(&presets_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("yaml") {
            let content = std::fs::read_to_string(&path)?;
            let preset: StylePreset = serde_yaml_ng::from_str(&content)
                .map_err(|e| anyhow::anyhow!("failed to parse preset {:?}: {}", path, e))?;
            presets.push(preset);
        }
    }
    Ok(presets)
}

/// Built-in presets first, then user presets from `<hub_dir>/presets/`.
pub fn load_all_presets(hub_dir: &Path) -> Result<Vec<StylePreset>> {
    let mut all = load_builtin_presets();
    all.extend(load_user_presets(hub_dir)?);
    Ok(all)
}

/// Finds a preset by `id`. Built-in presets take priority over user presets.
/// Returns an error if no preset with that id exists.
pub fn find_preset(id: &str, hub_dir: &Path) -> Result<StylePreset> {
    for preset in load_builtin_presets() {
        if preset.id == id {
            return Ok(preset);
        }
    }
    for preset in load_user_presets(hub_dir)? {
        if preset.id == id {
            return Ok(preset);
        }
    }
    anyhow::bail!("preset '{}' not found", id)
}

/// Always returns the `default-blob` built-in preset. Never fails.
pub fn default_blob() -> StylePreset {
    serde_yaml_ng::from_str(include_str!("builtin_presets/default-blob.yaml"))
        .expect("default-blob preset is malformed")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_presets_count_and_ids() {
        let presets = load_builtin_presets();
        assert_eq!(presets.len(), 7, "expected 7 built-in presets");
        let ids: Vec<&str> = presets.iter().map(|p| p.id.as_str()).collect();
        assert!(ids.contains(&"chiikawa"), "missing chiikawa");
        assert!(ids.contains(&"sanrio-pastel"), "missing sanrio-pastel");
        assert!(ids.contains(&"sumikko"), "missing sumikko");
        assert!(ids.contains(&"shimeji-retro"), "missing shimeji-retro");
        assert!(ids.contains(&"vtuber-soft"), "missing vtuber-soft");
        assert!(ids.contains(&"family-photo"), "missing family-photo");
        assert!(ids.contains(&"default-blob"), "missing default-blob");
    }

    #[test]
    fn builtin_presets_yaml_round_trip() {
        use crate::hub::style_preset::StylePreset;
        for preset in load_builtin_presets() {
            let yaml = serde_yaml_ng::to_string(&preset).expect("serialize");
            let back: StylePreset = serde_yaml_ng::from_str(&yaml).expect("deserialize");
            assert_eq!(preset, back, "round-trip failed for preset '{}'", preset.id);
        }
    }

    #[test]
    fn default_blob_is_chibi() {
        use crate::hub::style_preset::PresetFamily;
        let blob = default_blob();
        assert_eq!(blob.id, "default-blob");
        assert_eq!(blob.family, PresetFamily::Chibi);
    }

    #[test]
    fn find_preset_returns_builtin() {
        let tmp = tempfile::tempdir().unwrap();
        let preset = find_preset("chiikawa", tmp.path()).unwrap();
        assert_eq!(preset.id, "chiikawa");
    }

    #[test]
    fn find_preset_unknown_errors() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(find_preset("nonexistent-preset", tmp.path()).is_err());
    }

    #[test]
    fn load_user_presets_empty_dir_ok() {
        let tmp = tempfile::tempdir().unwrap();
        let result = load_user_presets(tmp.path()).unwrap();
        assert!(result.is_empty());
    }
}

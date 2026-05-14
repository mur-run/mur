//! `import_preset_file` + `import_preset_url` Tauri commands.
//!
//! Both validate the YAML as a `StylePreset` then copy it to
//! `~/.mur/hub/presets/<id>.yaml`.  The preset ID is taken from the
//! parsed struct (not the filename) so it survives renames.

use std::fs;
use std::path::PathBuf;

use mur_common::hub::style_preset::StylePreset;

fn presets_dir() -> PathBuf {
    std::env::var("MUR_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| dirs::home_dir().unwrap_or_default().join(".mur"))
        .join("hub/presets")
}

fn install_preset(yaml: &str) -> Result<String, String> {
    let preset: StylePreset = serde_yaml_ng::from_str(yaml)
        .map_err(|e| format!("invalid preset YAML: {e}"))?;

    let dir = presets_dir();
    fs::create_dir_all(&dir).map_err(|e| format!("create presets dir: {e}"))?;

    let dest = dir.join(format!("{}.yaml", preset.id));
    fs::write(&dest, yaml.as_bytes()).map_err(|e| format!("write preset: {e}"))?;

    Ok(preset.id)
}

/// Import a StylePreset YAML from a local file path.
#[tauri::command]
pub fn import_preset_file(path: String) -> Result<String, String> {
    let yaml = fs::read_to_string(&path)
        .map_err(|e| format!("read {path}: {e}"))?;
    install_preset(&yaml)
}

/// Import a StylePreset YAML from a URL (synchronous fetch via reqwest blocking).
#[tauri::command]
pub fn import_preset_url(url: String) -> Result<String, String> {
    let yaml = reqwest::blocking::get(&url)
        .map_err(|e| format!("fetch {url}: {e}"))?
        .text()
        .map_err(|e| format!("read body: {e}"))?;
    install_preset(&yaml)
}

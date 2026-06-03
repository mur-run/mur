//! `import_preset_file` + `import_preset_url` Tauri commands.
//!
//! Both validate the YAML as a `StylePreset` then copy it to
//! `~/.mur/hub/presets/<id>.yaml`.  The preset ID is taken from the
//! parsed struct (not the filename) so it survives renames.

use std::fs;
use std::path::PathBuf;

use mur_common::hub::style_preset::StylePreset;

/// Presets are tiny YAML documents; cap remote fetches well above any real
/// preset to bound memory from a hostile/oversized URL.
const MAX_PRESET_BYTES: u64 = 1024 * 1024; // 1 MiB
/// Bound a remote preset fetch so a slow/hung server can't block the IPC thread.
const PRESET_FETCH_TIMEOUT_SECS: u64 = 15;

fn presets_dir() -> PathBuf {
    std::env::var("MUR_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| dirs::home_dir().unwrap_or_default().join(".mur"))
        .join("hub/presets")
}

fn install_preset(yaml: &str) -> Result<String, String> {
    let preset: StylePreset =
        serde_yaml_ng::from_str(yaml).map_err(|e| format!("invalid preset YAML: {e}"))?;

    // The id becomes a path component (`<id>.yaml`); validate it with the
    // canonical safe-name rules so a crafted preset (e.g. fetched from an
    // arbitrary URL) can't escape presets_dir via `..` / absolute / separators.
    mur_common::agent_name::validate_agent_name(&preset.id)
        .map_err(|e| format!("invalid preset id: {e}"))?;

    let dir = presets_dir();
    fs::create_dir_all(&dir).map_err(|e| format!("create presets dir: {e}"))?;

    let dest = dir.join(format!("{}.yaml", preset.id));
    fs::write(&dest, yaml.as_bytes()).map_err(|e| format!("write preset: {e}"))?;

    Ok(preset.id)
}

/// Import a StylePreset YAML from a local file path.
#[tauri::command]
pub fn import_preset_file(path: String) -> Result<String, String> {
    let yaml = fs::read_to_string(&path).map_err(|e| format!("read {path}: {e}"))?;
    install_preset(&yaml)
}

/// Import a StylePreset YAML from a URL (synchronous fetch via reqwest blocking).
#[tauri::command]
pub fn import_preset_url(url: String) -> Result<String, String> {
    // Require HTTPS: rejects cleartext and file:// / other schemes, keeping the
    // fetch surface to TLS endpoints only.
    if !url
        .trim_start()
        .to_ascii_lowercase()
        .starts_with("https://")
    {
        return Err("preset URL must use https".into());
    }
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(PRESET_FETCH_TIMEOUT_SECS))
        .build()
        .map_err(|e| format!("http client: {e}"))?;
    let resp = client
        .get(&url)
        .send()
        .map_err(|e| format!("fetch {url}: {e}"))?;

    // Bound the body so a hostile URL can't exhaust memory.
    use std::io::Read;
    let mut buf = Vec::new();
    resp.take(MAX_PRESET_BYTES + 1)
        .read_to_end(&mut buf)
        .map_err(|e| format!("read body: {e}"))?;
    if buf.len() as u64 > MAX_PRESET_BYTES {
        return Err(format!("preset exceeds {MAX_PRESET_BYTES} bytes"));
    }
    let yaml = String::from_utf8(buf).map_err(|e| format!("preset body not UTF-8: {e}"))?;
    install_preset(&yaml)
}

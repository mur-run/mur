//! Theme loader. Reads theme dirs from the bundle, validates the
//! schema, exposes list/activate. P1.5 wires the appearance subscriber
//! and the runtime tray-icon swap; this scaffold provides the shape.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemeDef {
    pub schema_version: u32,
    pub name: String,
    #[serde(default)]
    pub display_name: BTreeMap<String, String>,
    pub kind: String,
    #[serde(default)]
    pub match_system: bool,
    pub colors: BTreeMap<String, String>,
    #[serde(default)]
    pub icons: BTreeMap<String, String>,
}

pub fn list() -> Result<Vec<crate::commands::ThemeInfo>> {
    let mut out = Vec::new();
    for dir in iter_theme_dirs()? {
        let theme = load_def(&dir)?;
        let display_name = theme
            .display_name
            .get("default")
            .cloned()
            .unwrap_or_else(|| theme.name.clone());
        out.push(crate::commands::ThemeInfo {
            name: theme.name,
            display_name,
            kind: theme.kind,
        });
    }
    Ok(out)
}

pub fn activate(name: &str) -> Result<()> {
    // P1.5: swap tray icon, dock icon, broadcast theme event to webview.
    // Scaffold: just verify the theme dir exists.
    let dir = themes_root()?.join(name);
    anyhow::ensure!(dir.exists(), "theme '{name}' not found in bundle");
    Ok(())
}

fn themes_root() -> Result<PathBuf> {
    // In dev (`cargo tauri dev`) themes/ is next to the binary at
    // mur-agent-gui/src-tauri/themes/. In a packaged bundle Tauri
    // copies bundle.resources to OS-specific places — this scaffold
    // walks up from CARGO_MANIFEST_DIR for now; P1.5 will rebase on
    // Tauri's resource resolver.
    Ok(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("themes"))
}

fn iter_theme_dirs() -> Result<Vec<PathBuf>> {
    let root = themes_root()?;
    let mut dirs = Vec::new();
    for entry in std::fs::read_dir(&root)
        .with_context(|| format!("read {}", root.display()))?
        .flatten()
    {
        let path = entry.path();
        if path.is_dir() && path.join("theme.json").exists() {
            dirs.push(path);
        }
    }
    dirs.sort();
    Ok(dirs)
}

fn load_def(dir: &PathBuf) -> Result<ThemeDef> {
    let path = dir.join("theme.json");
    let body = std::fs::read_to_string(&path)
        .with_context(|| format!("read {}", path.display()))?;
    serde_json::from_str(&body).with_context(|| format!("parse {}", path.display()))
}

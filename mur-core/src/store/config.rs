//! Configuration loading and management.

use anyhow::{Context, Result};
use mur_common::config::Config;
use std::fs;
use std::path::PathBuf;

/// Load config from ~/.mur/config.yaml, creating defaults if not exists.
pub fn load_config() -> Result<Config> {
    let path = config_path();

    if !path.exists() {
        // Create default config
        let config = Config::default();
        save_config(&config)?;
        return Ok(config);
    }

    let content = fs::read_to_string(&path)
        .with_context(|| format!("Failed to read config: {}", path.display()))?;
    let config: Config = serde_yaml::from_str(&content)
        .with_context(|| format!("Failed to parse config: {}", path.display()))?;
    Ok(config)
}

/// Save config to ~/.mur/config.yaml.
pub fn save_config(config: &Config) -> Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let yaml = serde_yaml::to_string(config)?;
    fs::write(&path, yaml)?;
    Ok(())
}

fn config_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("~"))
        .join(".mur")
        .join("config.yaml")
}

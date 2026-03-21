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

/// Save config to ~/.mur/config.yaml with helpful comments.
pub fn save_config(config: &Config) -> Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    // Serialize to YAML, then prepend a header with model recommendations
    let yaml = serde_yaml::to_string(config)?;
    let header = r#"# MUR Configuration
# Docs: https://github.com/mur-run/mur
#
# LLM Model Recommendations:
#
#   Anthropic:
#     Best quality:  claude-opus-4       ($15/$75 per 1M tokens)
#     Best value:    claude-sonnet-4     ($3/$15 per 1M tokens)
#     Budget:        claude-haiku-3.5    ($0.80/$4 per 1M tokens)
#
#   OpenAI:
#     Best quality:  gpt-4o             ($2.50/$10 per 1M tokens)
#     Best value:    gpt-4o-mini        ($0.15/$0.60 per 1M tokens)
#
#   Gemini:
#     Best quality:  gemini-2.5-pro     ($1.25/$10 per 1M tokens)
#     Best value:    gemini-2.5-flash   ($0.15/$0.60 per 1M tokens)
#
#   OpenRouter (any model via one API key):
#     Best quality:  anthropic/claude-sonnet-4
#     Best value:    google/gemini-2.5-flash
#
#   Ollama (free, local):
#     Best quality:  qwen3:32b (needs 20GB+ RAM)
#     Best value:    llama3.2:3b

"#;
    fs::write(&path, format!("{}{}", header, yaml))?;
    Ok(())
}

fn config_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("~"))
        .join(".mur")
        .join("config.yaml")
}

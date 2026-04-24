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
#   Anthropic (provider: anthropic):
#     Best quality:  claude-opus-4-6               ($15/$75 per 1M tokens)
#     Best value:    claude-sonnet-4-6             ($3/$15 per 1M tokens)
#     Budget:        claude-haiku-4-5-20251001     ($0.80/$4 per 1M tokens)
#
#   OpenAI (provider: openai):
#     Best quality:  gpt-4o                        ($2.50/$10 per 1M tokens)
#     Best value:    gpt-4o-mini                   ($0.15/$0.60 per 1M tokens)
#
#   Gemini (provider: gemini):
#     Best quality:  gemini-2.5-pro                ($1.25/$10 per 1M tokens)
#     Best value:    gemini-2.5-flash              ($0.15/$0.60 per 1M tokens)
#
#   OpenRouter (provider: openai, set openai_url to https://openrouter.ai/api/v1):
#     Best quality:  anthropic/claude-sonnet-4     ($3/$15 per 1M tokens)
#     Best value:    google/gemini-2.5-flash       ($0.15/$0.60 per 1M tokens)
#
#   Ollama (provider: ollama):
#     Best quality:  qwen3:32b                     (free, needs 20GB+ RAM)
#     Best value:    llama3.2:3b                    (free, needs 4GB RAM)
#
#   Copy the exact model name into llm.model below.
#
#   Default is Opus for best extraction quality (used only a few times/day).
#   To save cost, switch to Sonnet: llm.model: claude-sonnet-4-6

"#;
    fs::write(&path, format!("{}{}", header, yaml))?;
    Ok(())
}

fn config_path() -> PathBuf {
    // Honor MUR_HOME (authoritative, cross-platform) the same way
    // `conversations::paths::mur_root` does. On Windows, `dirs::home_dir()`
    // calls `SHGetKnownFolderPath` and ignores `HOME`/`USERPROFILE` env
    // overrides, so integration tests that override only `HOME` would read
    // the host's real `~/.mur/config.yaml` without this MUR_HOME check.
    if let Ok(p) = std::env::var("MUR_HOME")
        && !p.is_empty()
    {
        return PathBuf::from(p).join("config.yaml");
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("~"))
        .join(".mur")
        .join("config.yaml")
}

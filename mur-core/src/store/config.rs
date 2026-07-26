//! Configuration loading and management.

use anyhow::{Context, Result};
use mur_common::config::Config;
use std::fs;
use std::path::{Path, PathBuf};

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
    save_config_at(&config_path(), config)
}

/// Serialise `config`, then restore any top-level block the typed `Config`
/// has no field for.
///
/// `Config` cannot round-trip what it cannot parse, so serialising the struct
/// alone silently deletes user blocks — `research_gateway` was measurably lost
/// on every `mur sleep` (#778). Typed fields win; only keys absent from the
/// new document are carried over from the old one.
fn merge_over_existing(path: &Path, config: &Config) -> Result<String> {
    let mut out = serde_yaml::to_value(config)?;
    let existing: Option<serde_yaml::Value> = fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_yaml::from_str(&s).ok());

    if let (Some(serde_yaml::Value::Mapping(old)), serde_yaml::Value::Mapping(new)) =
        (existing, &mut out)
    {
        for (k, v) in old {
            if !new.contains_key(&k) {
                new.insert(k, v);
            }
        }
    }
    Ok(serde_yaml::to_string(&out)?)
}

/// Save config to an explicit path (same YAML + header as [`save_config`]).
/// Exists so callers with an explicit `MUR_HOME` (tests, per-agent CLI
/// handlers) can persist config without going through the process-global
/// `MUR_HOME` env var / `dirs::home_dir()` resolution.
pub fn save_config_at(path: &Path, config: &Config) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    // Serialize to YAML, then prepend a header with model recommendations
    let yaml = merge_over_existing(path, config)?;
    let header = r#"# MUR Configuration
# Docs: https://github.com/mur-run/mur
#
# LLM Model Recommendations:
#
#   Anthropic (provider: anthropic):
#     Best quality:  claude-opus-5                 ($5/$25 per 1M tokens)
#     Best value:    claude-sonnet-5               ($3/$15 per 1M tokens)
#     Budget:        claude-haiku-4-5-20251001     ($1/$5 per 1M tokens)
#
#   OpenAI (provider: openai):
#     Best quality:  gpt-5.6-sol                   ($5/$30 per 1M tokens)
#     Balanced:      gpt-5.6-terra                 ($2.50/$15 per 1M tokens)
#     Best value:    gpt-5.4-mini                  ($0.75/$4.50 per 1M tokens)
#
#   Gemini (provider: gemini):
#     Best quality:  gemini-3.1-pro-preview        ($2/$12 per 1M tokens)
#     Balanced:      gemini-3.6-flash              ($1.50/$7.50 per 1M tokens)
#     Best value:    gemini-3.5-flash-lite         ($0.30/$2.50 per 1M tokens)
#
#   OpenRouter (provider: openai, set openai_url to https://openrouter.ai/api/v1):
#     Best quality:  anthropic/claude-sonnet-5     ($3/$15 per 1M tokens)
#     Best value:    google/gemini-3.5-flash-lite  ($0.30/$2.50 per 1M tokens)
#
#   Ollama (provider: ollama):
#     Best quality:  gemma4:31b                    (free, needs 24GB+ RAM)
#     Best value:    qwen3.5:4b                    (free, needs 4GB RAM)
#
#   Copy the exact model name into llm.model below.
#
#   Default is Opus for best extraction quality (used only a few times/day).
#   To save cost, switch to Sonnet: llm.model: claude-sonnet-5

"#;
    fs::write(path, format!("{}{}", header, yaml))?;
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

#[cfg(test)]
mod save_roundtrip_tests {
    use super::*;
    use mur_common::config::Config;

    /// `Config` cannot round-trip a block it has no field for, so serialising
    /// the struct alone deletes it. Measured on a real config: a hand-written
    /// `research_gateway` block vanished on the next `mur sleep`.
    #[test]
    fn save_preserves_blocks_the_typed_config_does_not_know() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.yaml");
        std::fs::write(
            &path,
            "research_gateway:\n  brave_api_key_ref: keychain:mur/brave\n",
        )
        .unwrap();

        let cfg = Config::load_or_default(&path);
        save_config_at(&path, &cfg).unwrap();

        let back = std::fs::read_to_string(&path).unwrap();
        assert!(back.contains("research_gateway"), "block dropped:\n{back}");
        assert!(
            back.contains("keychain:mur/brave"),
            "value dropped:\n{back}"
        );
    }

    /// The typed fields must still win — an unknown-block merge that also
    /// resurrected stale known values would be a different bug.
    #[test]
    fn typed_fields_still_overwrite_the_old_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.yaml");
        std::fs::write(&path, "session:\n  retention_days: 3\nkeep_me: yes\n").unwrap();

        let mut cfg = Config::load_or_default(&path);
        cfg.session.retention_days = 99;
        save_config_at(&path, &cfg).unwrap();

        let back = std::fs::read_to_string(&path).unwrap();
        assert!(back.contains("99"), "typed field not written:\n{back}");
        assert!(back.contains("keep_me"), "unknown key dropped:\n{back}");
    }
}

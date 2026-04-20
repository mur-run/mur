//! Conversations archive — local-only, cross-source record of every AI
//! coding-assistant and chat-platform interaction.
//!
//! See `docs/superpowers/specs/2026-04-19-mur-conversations-design.md`.

pub mod audit;
pub mod blob;
pub mod index;
pub mod ingest;
pub mod migrate;
pub mod ollama;
pub mod paths;
pub mod retention;
pub mod retrieve;
pub mod store;

/// Read mur's config.yaml `conversations.enabled` flag. Defaults to `false`
/// when the file is missing or the key is absent — keeping legacy behavior
/// untouched for users who haven't opted in yet.
pub fn is_enabled() -> anyhow::Result<bool> {
    let Some(home) = dirs::home_dir() else {
        return Ok(false);
    };
    let cfg_path = home.join(".mur").join("config.yaml");
    if !cfg_path.exists() {
        return Ok(false);
    }
    let text = std::fs::read_to_string(&cfg_path)?;
    let doc: serde_yaml::Value = serde_yaml::from_str(&text)?;
    Ok(doc
        .get("conversations")
        .and_then(|c| c.get("enabled"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false))
}

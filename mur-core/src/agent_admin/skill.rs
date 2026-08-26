//! Skill admin operations for a single agent.

use anyhow::Result;

use super::error::{AgentAdminError, AgentAdminResult};
use crate::cmd::agent;

// ─── mutators ─────────────────────────────────────────────────────

/// Install a skill onto an agent; returns the installed id
/// (`skills/<manifest name>`), which is not necessarily derived from the
/// source filename.
pub fn add(name: &str, source: &str) -> Result<String> {
    agent::cmd_skill_add(name, source)
}

pub fn remove(name: &str, query: &str) -> Result<()> {
    agent::cmd_skill_remove(name, query)
}

// ─── queries ──────────────────────────────────────────────────────

/// List the skill ids declared in `profile.yaml`. The GUI fetches the
/// markdown bodies separately via `show()`.
pub fn list(name: &str) -> Result<Vec<String>> {
    let (_path, profile) = agent::load_profile_for_edit(name)?;
    Ok(profile.skills)
}

/// Read a single skill's markdown body. Returns the file contents
/// verbatim; the caller is expected to render the markdown.
pub fn show(name: &str, query: &str) -> AgentAdminResult<String> {
    let mur_home = agent::resolve_mur_home()?;
    let skills_dir = mur_home.join("agents").join(name).join("skills");
    let candidate = skills_dir.join(format!("{query}.md"));
    if candidate.exists() {
        return std::fs::read_to_string(&candidate).map_err(|source| AgentAdminError::Io {
            path: candidate,
            source,
        });
    }
    // Fall back to fuzzy: case-insensitive prefix on filename.
    if let Ok(rd) = std::fs::read_dir(&skills_dir) {
        let needle = query.to_lowercase();
        for entry in rd.flatten() {
            let path = entry.path();
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str())
                && stem.to_lowercase().starts_with(&needle)
            {
                return std::fs::read_to_string(&path)
                    .map_err(|source| AgentAdminError::Io { path, source });
            }
        }
    }
    Err(AgentAdminError::NotFound {
        agent: name.to_string(),
        kind: "skill",
        query: query.to_string(),
    })
}

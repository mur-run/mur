//! System-prompt admin operations for a single agent.

use anyhow::{Context, Result};
use std::fs;

use crate::cmd::agent;

// ─── mutators ─────────────────────────────────────────────────────

/// Set the system prompt body. Either `content` (inline string) or
/// `file` (path to read from) must be `Some`.
pub fn set(name: &str, content: Option<&str>, file: Option<&str>) -> Result<()> {
    agent::cmd_prompt_set(name, content, file)
}

// ─── queries ──────────────────────────────────────────────────────

/// Read the current system-prompt body. Used by the GUI System Prompt
/// tab (Monaco editor) to display the file contents.
pub fn get(name: &str) -> Result<String> {
    let path = agent::prompt_path_for(name)?;
    fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))
}

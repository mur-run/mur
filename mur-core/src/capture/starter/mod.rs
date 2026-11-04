//! Project-type detection: the primary language of a directory.
//!
//! This module used to generate starter *patterns* from a built-in template
//! bank (plus a dependency extractor to pick templates) on first contact with
//! a project. That went with the Pattern type (Tier C3); what remains is the
//! language probe used by `mur sync` status output.

use std::path::Path;

use serde::{Deserialize, Serialize};

// ─── Project Detection ──────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    Rust,
    Swift,
    JavaScript,
    TypeScript,
    Python,
    Go,
    Php,
    Ruby,
}

impl Language {
    pub fn as_str(&self) -> &'static str {
        match self {
            Language::Rust => "rust",
            Language::Swift => "swift",
            Language::JavaScript => "javascript",
            Language::TypeScript => "typescript",
            Language::Python => "python",
            Language::Go => "go",
            Language::Php => "php",
            Language::Ruby => "ruby",
        }
    }
}

impl std::fmt::Display for Language {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Detect the primary project language from files in the given directory.
pub fn detect_language(dir: &Path) -> Option<Language> {
    // Order matters: more specific first
    if dir.join("Cargo.toml").exists() {
        Some(Language::Rust)
    } else if dir.join("Package.swift").exists() {
        Some(Language::Swift)
    } else if dir.join("go.mod").exists() {
        Some(Language::Go)
    } else if dir.join("composer.json").exists() {
        Some(Language::Php)
    } else if dir.join("Gemfile").exists() {
        Some(Language::Ruby)
    } else if dir.join("pyproject.toml").exists() || dir.join("requirements.txt").exists() {
        Some(Language::Python)
    } else if dir.join("package.json").exists() {
        // Check for TypeScript
        let is_ts = dir.join("tsconfig.json").exists();
        if is_ts {
            Some(Language::TypeScript)
        } else {
            Some(Language::JavaScript)
        }
    } else {
        None
    }
}

/// Detect language and return its display name (for status messages).
pub fn detect_language_name(cwd: &Path) -> Option<String> {
    detect_language(cwd).map(|l| l.to_string())
}

#[cfg(test)]
mod tests;

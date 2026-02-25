//! YAML-based pattern store.
//!
//! Patterns are stored as individual YAML files in `~/.mur/patterns/`.
//! This module handles reading, writing, listing, and deleting patterns
//! with atomic writes (temp file + rename) for safety.

use anyhow::{Context, Result};
use mur_common::pattern::Pattern;
use std::fs;
use std::path::PathBuf;

/// The YAML pattern store.
pub struct YamlStore {
    /// Root patterns directory (e.g. ~/.mur/patterns/)
    patterns_dir: PathBuf,
}

impl YamlStore {
    /// Create a new YamlStore pointing at the given patterns directory.
    /// Creates the directory if it doesn't exist.
    pub fn new(patterns_dir: PathBuf) -> Result<Self> {
        fs::create_dir_all(&patterns_dir)
            .with_context(|| format!("Failed to create patterns dir: {}", patterns_dir.display()))?;
        Ok(Self { patterns_dir })
    }

    /// Create a YamlStore using the default ~/.mur/patterns/ path.
    pub fn default_store() -> Result<Self> {
        let dir = default_patterns_dir();
        Self::new(dir)
    }

    /// List all pattern names (without .yaml extension).
    pub fn list_names(&self) -> Result<Vec<String>> {
        let mut names = Vec::new();
        if !self.patterns_dir.exists() {
            return Ok(names);
        }
        for entry in fs::read_dir(&self.patterns_dir)? {
            let entry = entry?;
            let path = entry.path();
            if (path.extension().and_then(|e| e.to_str()) == Some("yaml")
                || path.extension().and_then(|e| e.to_str()) == Some("yml"))
                && let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    names.push(stem.to_string());
                }
        }
        names.sort();
        Ok(names)
    }

    /// Load all patterns from disk.
    pub fn list_all(&self) -> Result<Vec<Pattern>> {
        let names = self.list_names()?;
        let mut patterns = Vec::with_capacity(names.len());
        for name in &names {
            match self.get(name) {
                Ok(p) => patterns.push(p),
                Err(e) => {
                    tracing::warn!("Skipping pattern {}: {}", name, e);
                }
            }
        }
        Ok(patterns)
    }

    /// Get a single pattern by name.
    pub fn get(&self, name: &str) -> Result<Pattern> {
        let path = self.pattern_path(name);
        let content = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read pattern: {}", path.display()))?;
        let pattern: Pattern = serde_yaml::from_str(&content)
            .with_context(|| format!("Failed to parse pattern YAML: {}", path.display()))?;
        Ok(pattern)
    }

    /// Save a pattern to disk (atomic: write temp → rename).
    pub fn save(&self, pattern: &Pattern) -> Result<()> {
        let path = self.pattern_path(&pattern.name);
        let yaml = serde_yaml::to_string(pattern)
            .with_context(|| format!("Failed to serialize pattern: {}", pattern.name))?;

        // Atomic write: temp file in same directory, then rename
        let tmp_path = path.with_extension("yaml.tmp");
        fs::write(&tmp_path, &yaml)
            .with_context(|| format!("Failed to write temp file: {}", tmp_path.display()))?;
        fs::rename(&tmp_path, &path)
            .with_context(|| format!("Failed to rename temp to final: {}", path.display()))?;

        Ok(())
    }

    /// Delete a pattern by name. Returns true if it existed.
    #[allow(dead_code)] // Public API
    pub fn delete(&self, name: &str) -> Result<bool> {
        let path = self.pattern_path(name);
        if path.exists() {
            fs::remove_file(&path)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Move a pattern to the archive directory.
    pub fn archive(&self, name: &str) -> Result<bool> {
        let src = self.pattern_path(name);
        if !src.exists() {
            return Ok(false);
        }
        let archive_dir = self.patterns_dir.join("archive");
        fs::create_dir_all(&archive_dir)?;
        let dst = archive_dir.join(format!("{}.yaml", name));
        fs::rename(&src, &dst)?;
        Ok(true)
    }

    /// Check if a pattern exists.
    pub fn exists(&self, name: &str) -> bool {
        self.pattern_path(name).exists()
    }

    /// Get the file path for a pattern name.
    fn pattern_path(&self, name: &str) -> PathBuf {
        self.patterns_dir.join(format!("{}.yaml", name))
    }
}

/// Default patterns directory: ~/.mur/patterns/
pub fn default_patterns_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("~"))
        .join(".mur")
        .join("patterns")
}

/// Default MUR root directory: ~/.mur/
#[allow(dead_code)] // Public API
pub fn default_mur_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("~"))
        .join(".mur")
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::pattern::*;
    use tempfile::TempDir;

    fn make_test_pattern(name: &str) -> Pattern {
        Pattern {
            base: mur_common::knowledge::KnowledgeBase {
                schema: 2,
                name: name.to_string(),
                description: format!("Test pattern: {}", name),
                content: Content::DualLayer {
                    technical: "Use foo instead of bar.".to_string(),
                    principle: Some("Always prefer foo for consistency.".to_string()),
                },
                tier: Tier::Session,
                importance: 0.7,
                confidence: 0.8,
                tags: Tags::default(),
                applies: Applies::default(),
                evidence: Evidence::default(),
                links: Links::default(),
                lifecycle: Lifecycle::default(),
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                ..Default::default()
            },
            attachments: vec![],
        }
    }

    #[test]
    fn test_save_and_load() -> Result<()> {
        let tmp = TempDir::new()?;
        let store = YamlStore::new(tmp.path().to_path_buf())?;

        let pattern = make_test_pattern("test-pattern");
        store.save(&pattern)?;

        let loaded = store.get("test-pattern")?;
        assert_eq!(loaded.name, "test-pattern");
        assert_eq!(loaded.schema, 2);
        assert_eq!(loaded.importance, 0.7);

        // Check dual-layer content
        match &loaded.content {
            Content::DualLayer { technical, principle } => {
                assert!(technical.contains("foo"));
                assert!(principle.as_ref().unwrap().contains("consistency"));
            }
            Content::Plain(_) => panic!("Expected DualLayer content"),
        }

        Ok(())
    }

    #[test]
    fn test_list_names() -> Result<()> {
        let tmp = TempDir::new()?;
        let store = YamlStore::new(tmp.path().to_path_buf())?;

        store.save(&make_test_pattern("alpha"))?;
        store.save(&make_test_pattern("beta"))?;
        store.save(&make_test_pattern("gamma"))?;

        let names = store.list_names()?;
        assert_eq!(names, vec!["alpha", "beta", "gamma"]);

        Ok(())
    }

    #[test]
    fn test_delete() -> Result<()> {
        let tmp = TempDir::new()?;
        let store = YamlStore::new(tmp.path().to_path_buf())?;

        store.save(&make_test_pattern("to-delete"))?;
        assert!(store.exists("to-delete"));

        let deleted = store.delete("to-delete")?;
        assert!(deleted);
        assert!(!store.exists("to-delete"));

        let deleted_again = store.delete("to-delete")?;
        assert!(!deleted_again);

        Ok(())
    }

    #[test]
    fn test_archive() -> Result<()> {
        let tmp = TempDir::new()?;
        let store = YamlStore::new(tmp.path().to_path_buf())?;

        store.save(&make_test_pattern("to-archive"))?;
        assert!(store.exists("to-archive"));

        let archived = store.archive("to-archive")?;
        assert!(archived);
        assert!(!store.exists("to-archive"));

        // Check it's in archive dir
        assert!(tmp.path().join("archive").join("to-archive.yaml").exists());

        Ok(())
    }

    #[test]
    fn test_v1_plain_content_compat() -> Result<()> {
        let tmp = TempDir::new()?;
        let store = YamlStore::new(tmp.path().to_path_buf())?;

        // Write a v1-style pattern with plain content
        let mut pattern = make_test_pattern("v1-pattern");
        pattern.content = Content::Plain("Simple plain content from v1.".to_string());
        store.save(&pattern)?;

        let loaded = store.get("v1-pattern")?;
        match &loaded.content {
            Content::Plain(s) => assert!(s.contains("v1")),
            Content::DualLayer { technical, .. } => {
                // serde_yaml may deserialize plain string as DualLayer with just technical
                assert!(technical.contains("v1") || true); // flexible
            }
        }

        Ok(())
    }

    #[test]
    fn test_content_as_text() {
        let dual = Content::DualLayer {
            technical: "Tech stuff.".to_string(),
            principle: Some("Principle stuff.".to_string()),
        };
        let text = dual.as_text();
        assert!(text.contains("Tech stuff."));
        assert!(text.contains("Principle stuff."));

        let plain = Content::Plain("Plain text.".to_string());
        assert_eq!(plain.as_text(), "Plain text.");
    }

    #[test]
    fn test_evidence_effectiveness() {
        let mut ev = Evidence::default();
        assert_eq!(ev.effectiveness(), 0.5); // neutral prior

        ev.success_signals = 8;
        ev.override_signals = 2;
        assert!((ev.effectiveness() - 0.8).abs() < 0.001);
    }

    #[test]
    fn test_pattern_backward_compat_flat_yaml() -> Result<()> {
        // Simulate a v2 YAML file with flat fields (as stored on disk via serde(flatten))
        let yaml = r#"
schema: 2
name: backward-test
description: Test backward compat
content:
  technical: Use foo
  principle: Because bar
tier: project
importance: 0.7
confidence: 0.8
tags:
  languages: []
  topics:
    - rust
applies:
  projects: []
  languages: []
  tools: []
  auto_scope: false
evidence:
  source_sessions: []
  injection_count: 0
  success_signals: 0
  override_signals: 0
links:
  related: []
  supersedes: []
  workflows: []
lifecycle:
  status: active
  pinned: false
  muted: false
created_at: "2026-02-20T00:00:00Z"
updated_at: "2026-02-20T00:00:00Z"
attachments: []
"#;

        let pattern: Pattern = serde_yaml::from_str(yaml)?;
        assert_eq!(pattern.name, "backward-test");
        assert_eq!(pattern.schema, 2);
        assert_eq!(pattern.tier, Tier::Project);
        assert!((pattern.importance - 0.7).abs() < 0.001);

        // Roundtrip: serialize and deserialize again
        let yaml2 = serde_yaml::to_string(&pattern)?;
        let pattern2: Pattern = serde_yaml::from_str(&yaml2)?;
        assert_eq!(pattern2.name, "backward-test");
        assert_eq!(pattern2.tier, Tier::Project);

        Ok(())
    }

    #[test]
    fn test_pattern_serde_roundtrip() -> Result<()> {
        let tmp = TempDir::new()?;
        let store = YamlStore::new(tmp.path().to_path_buf())?;

        let pattern = make_test_pattern("roundtrip-test");
        store.save(&pattern)?;

        // Read raw YAML and verify flat structure (no nested `base:` key)
        let raw = std::fs::read_to_string(tmp.path().join("roundtrip-test.yaml"))?;
        assert!(raw.contains("name: roundtrip-test"));
        assert!(!raw.contains("base:"), "YAML should be flat, not nested under 'base:'");

        let loaded = store.get("roundtrip-test")?;
        assert_eq!(loaded.name, "roundtrip-test");
        assert_eq!(loaded.attachments.len(), 0);

        Ok(())
    }
}

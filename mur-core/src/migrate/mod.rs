//! Migration from Mur v1 (Go) patterns to v2 (Rust) schema.

use anyhow::{Context, Result};
use mur_common::pattern::*;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// A v1 pattern as stored by the Go version.
/// Uses serde(flatten) to absorb any unknown fields.
#[derive(Debug, serde::Deserialize)]
struct V1Pattern {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    domain: String,
    #[serde(default)]
    category: String,
    #[serde(default)]
    tags: V1Tags,
    #[serde(default)]
    confidence: f64,
    #[serde(default)]
    #[allow(dead_code)] // needed for v1 deserialization compatibility
    team_shared: bool,
    #[serde(default)]
    created_at: String,
    #[serde(default)]
    updated_at: String,
    // Absorb unknown fields (id, security, etc.)
    #[serde(flatten)]
    _extra: std::collections::HashMap<String, serde_yaml::Value>,
}

/// v1 tags can be either a list of strings or a map
#[derive(Debug, Default, serde::Deserialize)]
#[serde(untagged)]
enum V1Tags {
    List(Vec<String>),
    Map(std::collections::HashMap<String, serde_yaml::Value>),
    #[default]
    Empty,
}

impl V1Tags {
    fn to_vec(&self) -> Vec<String> {
        match self {
            V1Tags::List(v) => v.clone(),
            V1Tags::Map(m) => m.keys().cloned().collect(),
            V1Tags::Empty => vec![],
        }
    }
}

/// Migration result summary.
#[derive(Debug, Default)]
pub struct MigrateResult {
    pub migrated: usize,
    pub already_v2: usize,
    pub skipped: usize,
    pub errors: Vec<String>,
}

/// Migrate all v1 patterns in a directory to v2 schema.
pub fn migrate_directory(patterns_dir: &Path) -> Result<MigrateResult> {
    let mut result = MigrateResult::default();

    if !patterns_dir.exists() {
        return Ok(result);
    }

    let entries: Vec<PathBuf> = fs::read_dir(patterns_dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "yaml" || ext == "yml"))
        .collect();

    for path in entries {
        match migrate_single_file(&path) {
            Ok(MigrateFileResult::Migrated) => result.migrated += 1,
            Ok(MigrateFileResult::AlreadyV2) => result.already_v2 += 1,
            Ok(MigrateFileResult::Skipped(reason)) => {
                result.skipped += 1;
                result.errors.push(format!("{}: {}", path.display(), reason));
            }
            Err(e) => {
                result.skipped += 1;
                result.errors.push(format!("{}: {}", path.display(), e));
            }
        }
    }

    Ok(result)
}

enum MigrateFileResult {
    Migrated,
    AlreadyV2,
    Skipped(String),
}

fn migrate_single_file(path: &Path) -> Result<MigrateFileResult> {
    let raw = fs::read_to_string(path).context("reading file")?;

    // Check if already v2
    if raw.contains("schema: 2") {
        return Ok(MigrateFileResult::AlreadyV2);
    }

    // Try parsing as v1
    let v1: V1Pattern = serde_yaml::from_str(&raw)
        .with_context(|| format!("parsing v1 pattern: {}", path.display()))?;

    // Skip empty content
    if v1.content.trim().is_empty() && v1.description.trim().is_empty() {
        return Ok(MigrateFileResult::Skipped("empty content".into()));
    }

    // Convert to v2
    let v2 = convert_v1_to_v2(&v1)?;

    // Write back (atomic: write to temp, rename)
    let yaml = serde_yaml::to_string(&v2)?;
    let tmp = path.with_extension("yaml.tmp");
    fs::write(&tmp, &yaml)?;
    fs::rename(&tmp, path)?;

    Ok(MigrateFileResult::Migrated)
}

fn convert_v1_to_v2(v1: &V1Pattern) -> Result<Pattern> {
    let created = parse_datetime(&v1.created_at).unwrap_or_else(chrono::Utc::now);
    let updated = parse_datetime(&v1.updated_at).unwrap_or_else(chrono::Utc::now);

    // Map v1 tags + domain to v2 tags
    let mut topics = v1.tags.to_vec();
    if !v1.domain.is_empty() && v1.domain != "dev" {
        topics.push(v1.domain.clone());
    }
    if !v1.category.is_empty() && v1.category != "pattern" {
        topics.push(v1.category.clone());
    }
    topics.sort();
    topics.dedup();

    let description = if v1.description.is_empty() {
        v1.name.replace('-', " ")
    } else {
        v1.description.clone()
    };

    Ok(Pattern {
        schema: 2,
        name: v1.name.clone(),
        description,
        content: Content::DualLayer {
            technical: v1.content.clone(),
            principle: None,
        },
        tier: Tier::Session, // default, user can promote later
        importance: 0.5,
        confidence: if v1.confidence > 0.0 { v1.confidence } else { 0.5 },
        tags: Tags {
            languages: vec![],
            topics,
            extra: HashMap::new(),
        },
        applies: Applies::default(),
        evidence: Evidence {
            first_seen: Some(created),
            ..Evidence::default()
        },
        links: Links::default(),
        lifecycle: Lifecycle::default(),
        created_at: created,
        updated_at: updated,
    })
}

fn parse_datetime(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    if s.is_empty() {
        return None;
    }
    // Try various formats
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .ok()
        .or_else(|| {
            chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S")
                .ok()
                .map(|dt| dt.and_utc())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_v1_pattern(dir: &Path, name: &str, content: &str) {
        let yaml = format!(
            "name: {}\ndescription: Test pattern\ncontent: |\n  {}\ndomain: dev\ncategory: pattern\ntags: []\nconfidence: 0.6\nteam_shared: false\ncreated_at: \"2026-02-16T07:54:16+08:00\"\nupdated_at: \"2026-02-16T07:54:16+08:00\"\n",
            name, content
        );
        fs::write(dir.join(format!("{}.yaml", name)), yaml).unwrap();
    }

    #[test]
    fn test_migrate_v1_patterns() {
        let tmp = TempDir::new().unwrap();
        write_v1_pattern(tmp.path(), "test-pattern", "Use foo for bar");
        write_v1_pattern(tmp.path(), "another-one", "Do X then Y");

        let result = migrate_directory(tmp.path()).unwrap();
        assert_eq!(result.migrated, 2);
        assert_eq!(result.already_v2, 0);

        // Verify converted file
        let content = fs::read_to_string(tmp.path().join("test-pattern.yaml")).unwrap();
        assert!(content.contains("schema: 2"));
        assert!(content.contains("technical:"));
    }

    #[test]
    fn test_skip_already_v2() {
        let tmp = TempDir::new().unwrap();
        let v2 = "schema: 2\nname: v2-pattern\ndescription: Already v2\ncontent:\n  technical: hello\ntier: session\nimportance: 0.5\nconfidence: 0.5\n";
        fs::write(tmp.path().join("v2-pattern.yaml"), v2).unwrap();

        let result = migrate_directory(tmp.path()).unwrap();
        assert_eq!(result.already_v2, 1);
        assert_eq!(result.migrated, 0);
    }

    #[test]
    fn test_skip_empty_content() {
        let tmp = TempDir::new().unwrap();
        let yaml = "name: empty\ndescription: \"\"\ncontent: \"\"\ndomain: dev\ncategory: pattern\ntags: []\nconfidence: 0.6\nteam_shared: false\ncreated_at: \"\"\nupdated_at: \"\"\n";
        fs::write(tmp.path().join("empty.yaml"), yaml).unwrap();

        let result = migrate_directory(tmp.path()).unwrap();
        assert_eq!(result.skipped, 1);
    }

    #[test]
    fn test_idempotent_migration() {
        let tmp = TempDir::new().unwrap();
        write_v1_pattern(tmp.path(), "idem", "content here");

        let r1 = migrate_directory(tmp.path()).unwrap();
        assert_eq!(r1.migrated, 1);

        let r2 = migrate_directory(tmp.path()).unwrap();
        assert_eq!(r2.already_v2, 1);
        assert_eq!(r2.migrated, 0);
    }

    #[test]
    fn test_convert_v1_to_v2() {
        let v1 = V1Pattern {
            name: "test".into(),
            description: "A test".into(),
            content: "Do the thing".into(),
            domain: "dev".into(),
            category: "convention".into(),
            tags: V1Tags::List(vec!["swift".into()]),
            confidence: 0.8,
            team_shared: false,
            created_at: "2026-02-16T07:54:16+08:00".into(),
            updated_at: "2026-02-16T07:54:16+08:00".into(),
            _extra: std::collections::HashMap::new(),
        };

        let v2 = convert_v1_to_v2(&v1).unwrap();
        assert_eq!(v2.schema, 2);
        assert_eq!(v2.name, "test");
        assert_eq!(v2.tier, Tier::Session);
        assert_eq!(v2.confidence, 0.8);
        assert!(v2.tags.topics.contains(&"convention".to_string()));
        assert!(v2.tags.topics.contains(&"swift".to_string()));
        match &v2.content {
            Content::DualLayer { technical, principle } => {
                assert_eq!(technical, "Do the thing");
                assert!(principle.is_none());
            }
            _ => panic!("expected DualLayer"),
        }
    }
}

//! MUR Knowledge Exchange Format (MKEF) — import/export patterns
//! for cross-tool knowledge sharing.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use mur_common::knowledge::KnowledgeBase;
use mur_common::pattern::{Content, Origin, OriginTrigger, Pattern, PatternKind, Tier};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use super::yaml::YamlStore;

/// MKEF entry — the portable knowledge exchange format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MkefEntry {
    pub mkef_version: u32,
    pub id: String,
    pub kind: String,
    pub content: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub origin: Option<MkefOrigin>,
    #[serde(default)]
    pub scope: Option<MkefScope>,
    #[serde(default)]
    pub lifecycle: Option<MkefLifecycle>,
    #[serde(default)]
    pub privacy: MkefPrivacy,
    /// Forward-compatible: ignore unknown fields
    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, serde_yaml::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MkefOrigin {
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub platform: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MkefScope {
    #[serde(default)]
    pub projects: Vec<String>,
    #[serde(default)]
    pub languages: Vec<String>,
    #[serde(default)]
    pub tools: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MkefLifecycle {
    #[serde(default)]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub confidence: Option<f64>,
    #[serde(default)]
    pub importance: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum MkefPrivacy {
    #[default]
    Public,
    Private,
}

/// Parse an MKEF YAML file.
pub fn parse_mkef(yaml_str: &str) -> Result<MkefEntry> {
    serde_yaml::from_str(yaml_str).context("Failed to parse MKEF entry")
}

/// Convert an MKEF entry to a MUR Pattern.
///
/// Scope is fully restored into `pattern.applies` so that project / language /
/// tool constraints survive a round-trip through MKEF export → import.
pub fn mkef_to_pattern(entry: &MkefEntry) -> Pattern {
    let kind = match entry.kind.as_str() {
        "preference" => Some(PatternKind::Preference),
        "behavioral" => Some(PatternKind::Behavioral),
        "procedure" => Some(PatternKind::Procedure),
        "fact" => Some(PatternKind::Fact),
        "technical" => Some(PatternKind::Technical),
        _ => None,
    };

    let origin = entry.origin.as_ref().map(|o| Origin {
        source: o.source.clone().unwrap_or_else(|| "mkef-import".into()),
        trigger: OriginTrigger::UserExplicit,
        user: o.user.clone(),
        platform: o.platform.clone(),
        confidence: 0.8,
    });

    let description = entry
        .description
        .clone()
        .unwrap_or_else(|| entry.content.chars().take(100).collect());

    let lc = entry.lifecycle.as_ref();

    // Restore scope → applies so round-trips preserve project/language/tool constraints.
    let applies = entry
        .scope
        .as_ref()
        .map(|s| mur_common::pattern::Applies {
            projects: s.projects.clone(),
            languages: s.languages.clone(),
            tools: s.tools.clone(),
            auto_scope: false,
        })
        .unwrap_or_default();

    Pattern {
        base: KnowledgeBase {
            schema: 2,
            name: entry.id.clone(),
            description,
            content: Content::Plain(entry.content.clone()),
            tier: Tier::Session,
            importance: lc.and_then(|l| l.importance).unwrap_or(0.5),
            confidence: lc.and_then(|l| l.confidence).unwrap_or(0.7),
            created_at: lc.and_then(|l| l.created_at).unwrap_or_else(Utc::now),
            updated_at: Utc::now(),
            applies,
            ..Default::default()
        },
        kind,
        origin,
        attachments: vec![],
    }
}

/// Convert a MUR Pattern to an MKEF entry.
///
/// Privacy: patterns bound to a specific user (via `origin.user`) are marked
/// `Private` so they are not accidentally published when sharing MKEF files.
/// All other patterns are `Public` by default.
pub fn pattern_to_mkef(pattern: &Pattern) -> MkefEntry {
    let kind = match pattern.effective_kind() {
        PatternKind::Preference => "preference",
        PatternKind::Behavioral => "behavioral",
        PatternKind::Procedure => "procedure",
        PatternKind::Fact => "fact",
        PatternKind::Technical => "technical",
    };

    let origin = pattern.origin.as_ref().map(|o| MkefOrigin {
        source: Some(o.source.clone()),
        user: o.user.clone(),
        platform: o.platform.clone(),
    });

    // Patterns with a bound user are private by default — they should not be
    // published without explicit opt-in.
    let privacy = if pattern.origin.as_ref().is_some_and(|o| o.user.is_some()) {
        MkefPrivacy::Private
    } else {
        MkefPrivacy::Public
    };

    // Only export non-empty applies scopes
    let scope = if pattern.applies.projects.is_empty()
        && pattern.applies.languages.is_empty()
        && pattern.applies.tools.is_empty()
    {
        None
    } else {
        Some(MkefScope {
            projects: pattern.applies.projects.clone(),
            languages: pattern.applies.languages.clone(),
            tools: pattern.applies.tools.clone(),
        })
    };

    MkefEntry {
        mkef_version: 1,
        id: pattern.name.clone(),
        kind: kind.to_string(),
        content: pattern.content.as_text().to_string(),
        description: Some(pattern.description.clone()),
        origin,
        scope,
        lifecycle: Some(MkefLifecycle {
            created_at: Some(pattern.created_at),
            confidence: Some(pattern.confidence),
            importance: Some(pattern.importance),
        }),
        privacy,
        extra: Default::default(),
    }
}

/// Import an MKEF file into the pattern store.
/// Returns the pattern name. Skips if already exists (by id).
pub fn import_mkef_file(path: &Path, store: &YamlStore) -> Result<Option<String>> {
    let yaml_str = fs::read_to_string(path)
        .with_context(|| format!("Failed to read MKEF file: {}", path.display()))?;
    let entry = parse_mkef(&yaml_str)?;

    // Skip if already exists
    if store.get(&entry.id).is_ok() {
        return Ok(None);
    }

    let pattern = mkef_to_pattern(&entry);
    store.save(&pattern)?;
    Ok(Some(entry.id))
}

/// Import all MKEF files from a directory.
pub fn import_mkef_dir(dir: &Path, store: &YamlStore) -> Result<Vec<String>> {
    let mut imported = Vec::new();
    if !dir.exists() {
        return Ok(imported);
    }

    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "yaml" || e == "yml")
            && let Some(name) = import_mkef_file(&path, store)?
        {
            imported.push(name);
        }
    }
    Ok(imported)
}

/// Export a pattern to an MKEF file in the exchange directory.
/// Always writes the file regardless of privacy setting.
/// Callers that want to share patterns publicly should use
/// [`export_mkef_public`] which skips private patterns.
pub fn export_mkef(pattern: &Pattern, exchange_dir: &Path) -> Result<PathBuf> {
    fs::create_dir_all(exchange_dir)?;
    let entry = pattern_to_mkef(pattern);
    let yaml = serde_yaml::to_string(&entry)?;
    let path = exchange_dir.join(format!("{}.yaml", pattern.name));
    fs::write(&path, &yaml)?;
    Ok(path)
}

/// Export a pattern only when it is `Public`.  Returns `None` and skips
/// writing when the pattern would be marked `Private` (i.e., user-bound).
/// Use this when bulk-exporting for community sharing.
#[allow(dead_code)]
pub fn export_mkef_public(pattern: &Pattern, exchange_dir: &Path) -> Result<Option<PathBuf>> {
    let entry = pattern_to_mkef(pattern);
    if entry.privacy == MkefPrivacy::Private {
        return Ok(None);
    }
    fs::create_dir_all(exchange_dir)?;
    let yaml = serde_yaml::to_string(&entry)?;
    let path = exchange_dir.join(format!("{}.yaml", pattern.name));
    fs::write(&path, &yaml)?;
    Ok(Some(path))
}

/// Get the default exchange directory.
pub fn default_exchange_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("~"))
        .join(".mur")
        .join("exchange")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_parse_mkef_valid() {
        let yaml = r#"
mkef_version: 1
id: prefer-chinese
kind: preference
content: "Always respond in Traditional Chinese"
description: "User language preference"
privacy: public
"#;
        let entry = parse_mkef(yaml).unwrap();
        assert_eq!(entry.id, "prefer-chinese");
        assert_eq!(entry.kind, "preference");
        assert_eq!(entry.privacy, MkefPrivacy::Public);
    }

    #[test]
    fn test_parse_mkef_with_unknown_fields() {
        let yaml = r#"
mkef_version: 1
id: test-forward-compat
kind: fact
content: "Some fact"
future_field: "should be ignored"
another_unknown: 42
"#;
        let entry = parse_mkef(yaml).unwrap();
        assert_eq!(entry.id, "test-forward-compat");
        assert!(entry.extra.contains_key("future_field"));
    }

    #[test]
    fn test_parse_mkef_invalid() {
        let yaml = "not: valid: mkef";
        assert!(parse_mkef(yaml).is_err());
    }

    #[test]
    fn test_mkef_to_pattern() {
        let entry = MkefEntry {
            mkef_version: 1,
            id: "test-pattern".into(),
            kind: "preference".into(),
            content: "Use dark mode".into(),
            description: Some("Dark mode preference".into()),
            origin: Some(MkefOrigin {
                source: Some("commander".into()),
                user: Some("david".into()),
                platform: None,
            }),
            scope: None,
            lifecycle: Some(MkefLifecycle {
                confidence: Some(0.9),
                importance: Some(0.8),
                created_at: None,
            }),
            privacy: MkefPrivacy::Public,
            extra: Default::default(),
        };

        let pattern = mkef_to_pattern(&entry);
        assert_eq!(pattern.name, "test-pattern");
        assert_eq!(pattern.effective_kind(), PatternKind::Preference);
        assert!(pattern.origin.is_some());
        assert!((pattern.confidence - 0.9).abs() < 0.001);
    }

    #[test]
    fn test_pattern_to_mkef_roundtrip() {
        use mur_common::pattern::Applies;
        let pattern = Pattern {
            base: KnowledgeBase {
                name: "roundtrip-test".into(),
                description: "Test roundtrip".into(),
                content: Content::Plain("Content here".into()),
                confidence: 0.85,
                importance: 0.7,
                applies: Applies {
                    projects: vec!["my-project".into()],
                    languages: vec!["rust".into()],
                    tools: vec!["claude-code".into()],
                    auto_scope: false,
                },
                ..Default::default()
            },
            kind: Some(PatternKind::Procedure),
            origin: None,
            attachments: vec![],
        };

        let mkef = pattern_to_mkef(&pattern);
        assert_eq!(mkef.id, "roundtrip-test");
        assert_eq!(mkef.kind, "procedure");
        assert_eq!(mkef.privacy, MkefPrivacy::Public); // no user-bound origin → public

        // Scope must be exported
        let scope = mkef.scope.as_ref().expect("scope should be exported");
        assert_eq!(scope.projects, vec!["my-project"]);
        assert_eq!(scope.languages, vec!["rust"]);
        assert_eq!(scope.tools, vec!["claude-code"]);

        // Scope must be restored on import
        let back = mkef_to_pattern(&mkef);
        assert_eq!(back.name, "roundtrip-test");
        assert_eq!(back.effective_kind(), PatternKind::Procedure);
        assert_eq!(back.applies.projects, vec!["my-project"]);
        assert_eq!(back.applies.languages, vec!["rust"]);
        assert_eq!(back.applies.tools, vec!["claude-code"]);
    }

    #[test]
    fn test_user_bound_pattern_is_private() {
        use mur_common::pattern::{Origin, OriginTrigger};
        let pattern = Pattern {
            base: KnowledgeBase {
                name: "alice-pref".into(),
                description: "Alice pref".into(),
                content: Content::Plain("Alice likes dark mode".into()),
                ..Default::default()
            },
            kind: Some(PatternKind::Preference),
            origin: Some(Origin {
                source: "commander".into(),
                trigger: OriginTrigger::UserExplicit,
                user: Some("alice".into()),
                platform: None,
                confidence: 1.0,
            }),
            attachments: vec![],
        };

        let mkef = pattern_to_mkef(&pattern);
        assert_eq!(
            mkef.privacy,
            MkefPrivacy::Private,
            "User-bound patterns must be marked Private"
        );
    }

    #[test]
    fn test_export_mkef_public_skips_private() -> Result<()> {
        use mur_common::pattern::{Origin, OriginTrigger};
        let tmp = TempDir::new()?;
        let exchange_dir = tmp.path().join("exchange");

        let private_pattern = Pattern {
            base: KnowledgeBase {
                name: "user-specific".into(),
                description: "Private pref".into(),
                content: Content::Plain("Only for david".into()),
                ..Default::default()
            },
            kind: Some(PatternKind::Preference),
            origin: Some(Origin {
                source: "commander".into(),
                trigger: OriginTrigger::UserExplicit,
                user: Some("david".into()),
                platform: None,
                confidence: 1.0,
            }),
            attachments: vec![],
        };

        let result = export_mkef_public(&private_pattern, &exchange_dir)?;
        assert!(result.is_none(), "Private pattern must not be exported");
        assert!(
            !exchange_dir.exists(),
            "Exchange dir must not be created for private patterns"
        );

        let public_pattern = Pattern {
            base: KnowledgeBase {
                name: "shared-pattern".into(),
                description: "Public pattern".into(),
                content: Content::Plain("Universal advice".into()),
                ..Default::default()
            },
            kind: Some(PatternKind::Technical),
            origin: None,
            attachments: vec![],
        };

        let result = export_mkef_public(&public_pattern, &exchange_dir)?;
        assert!(result.is_some(), "Public pattern must be exported");
        assert!(result.unwrap().exists());

        Ok(())
    }

    #[test]
    fn test_import_export_file() -> Result<()> {
        let tmp = TempDir::new()?;
        let store = YamlStore::new(tmp.path().join("patterns"))?;
        let exchange_dir = tmp.path().join("exchange");

        let pattern = Pattern {
            base: KnowledgeBase {
                name: "export-test".into(),
                description: "Test export".into(),
                content: Content::Plain("Exported content".into()),
                ..Default::default()
            },
            kind: Some(PatternKind::Fact),
            origin: None,
            attachments: vec![],
        };
        store.save(&pattern)?;

        let path = export_mkef(&pattern, &exchange_dir)?;
        assert!(path.exists());

        let store2 = YamlStore::new(tmp.path().join("patterns2"))?;
        let imported = import_mkef_file(&path, &store2)?;
        assert_eq!(imported, Some("export-test".into()));

        let loaded = store2.get("export-test")?;
        assert_eq!(loaded.effective_kind(), PatternKind::Fact);

        // Import again — should skip
        let imported2 = import_mkef_file(&path, &store2)?;
        assert_eq!(imported2, None);

        Ok(())
    }

    #[test]
    fn test_import_dir() -> Result<()> {
        let tmp = TempDir::new()?;
        let exchange_dir = tmp.path().join("exchange");
        fs::create_dir_all(&exchange_dir)?;

        let yaml1 = "mkef_version: 1\nid: dir-test-1\nkind: fact\ncontent: fact one\n";
        let yaml2 = "mkef_version: 1\nid: dir-test-2\nkind: preference\ncontent: pref two\n";
        fs::write(exchange_dir.join("one.yaml"), yaml1)?;
        fs::write(exchange_dir.join("two.yaml"), yaml2)?;

        let store = YamlStore::new(tmp.path().join("patterns"))?;
        let imported = import_mkef_dir(&exchange_dir, &store)?;
        assert_eq!(imported.len(), 2);

        Ok(())
    }
}

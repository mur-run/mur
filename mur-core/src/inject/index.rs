use mur_common::pattern::{LifecycleStatus, Pattern};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityEntry {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityIndex {
    pub entries: Vec<CapabilityEntry>,
    pub project: Option<String>,
}

/// Format a capability index as an L0 markdown injection.
///
/// `budget_chars` is the character budget (≈ tokens × 4). Returns empty string
/// if there are no entries. Truncates entry list when adding the next entry
/// would exceed the budget.
pub fn format_l0(index: &CapabilityIndex, budget_chars: usize) -> String {
    if index.entries.is_empty() {
        return String::new();
    }

    let header = match &index.project {
        Some(proj) => format!("## mur learning index (project: {proj})\n"),
        None => "## mur learning index\n".to_owned(),
    };
    let footer = "\nRun `mur recall <name>` to load full content of any item above.\n";

    let overhead = header.len() + footer.len();
    let entry_budget = budget_chars.saturating_sub(overhead);

    let mut body = String::new();
    for entry in &index.entries {
        let line = format!("- `{}` — {}\n", entry.name, entry.description);
        if body.len() + line.len() > entry_budget {
            break;
        }
        body.push_str(&line);
    }

    if body.is_empty() {
        return String::new();
    }

    format!("{header}{body}{footer}")
}

/// Default character budget for L0 index injection (≈ 600 tokens × 4).
pub const L0_BUDGET_CHARS: usize = 2400;

/// Save index to an explicit path (testable with tempdir).
pub fn save_to(index: &CapabilityIndex, path: &std::path::Path) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(index)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &json)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Load index from an explicit path; returns empty index if file is missing.
#[allow(dead_code)]
pub fn load_from(path: &std::path::Path) -> anyhow::Result<CapabilityIndex> {
    match std::fs::read_to_string(path) {
        Ok(content) => Ok(serde_json::from_str(&content)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(CapabilityIndex {
            entries: vec![],
            project: None,
        }),
        Err(e) => Err(e.into()),
    }
}

/// Save capability index to `~/.mur/index/capabilities.json`.
pub fn save(index: &CapabilityIndex) -> anyhow::Result<()> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home dir"))?;
    let path = home.join(".mur").join("index").join("capabilities.json");
    save_to(index, &path)
}

/// Load capability index from `~/.mur/index/capabilities.json`.
#[allow(dead_code)]
pub fn load() -> anyhow::Result<CapabilityIndex> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home dir"))?;
    let path = home.join(".mur").join("index").join("capabilities.json");
    load_from(&path)
}

pub fn build(patterns: &[Pattern], project: Option<&str>) -> CapabilityIndex {
    let mut entries: Vec<(f64, CapabilityEntry)> = patterns
        .iter()
        .filter(|p| p.lifecycle.status != LifecycleStatus::Archived && !p.lifecycle.muted)
        .filter(|p| {
            let projs = &p.applies.projects;
            if projs.is_empty() {
                return true;
            }
            match project {
                Some(proj) => projs.iter().any(|s| s == proj || s == "*"),
                None => false,
            }
        })
        .map(|p| {
            (
                p.importance,
                CapabilityEntry {
                    name: p.name.clone(),
                    description: p.description.clone(),
                },
            )
        })
        .collect();

    entries.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.1.name.cmp(&b.1.name))
    });

    CapabilityIndex {
        entries: entries.into_iter().map(|(_, e)| e).collect(),
        project: project.map(str::to_owned),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::knowledge::{DecayMeta, KnowledgeBase, Maturity};
    use mur_common::pattern::{
        Applies, Content, Evidence, Lifecycle, LifecycleStatus, Links, Pattern, Tags, Tier,
    };

    fn make_pattern(name: &str, desc: &str, importance: f64) -> Pattern {
        Pattern {
            base: KnowledgeBase {
                schema: 2,
                name: name.to_owned(),
                description: desc.to_owned(),
                content: Content::Plain(String::new()),
                tier: Tier::Project,
                importance,
                confidence: 0.8,
                tags: Tags::default(),
                applies: Applies::default(),
                evidence: Evidence::default(),
                links: Links::default(),
                lifecycle: Lifecycle::default(),
                maturity: Maturity::Stable,
                decay: DecayMeta::default(),
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                scope: mur_common::Scope::default(),
            },
            kind: None,
            origin: None,
            attachments: Vec::new(),
        }
    }

    fn archived_pattern(name: &str) -> Pattern {
        let mut p = make_pattern(name, "archived", 0.9);
        p.base.lifecycle.status = LifecycleStatus::Archived;
        p
    }

    fn muted_pattern(name: &str) -> Pattern {
        let mut p = make_pattern(name, "muted", 0.9);
        p.base.lifecycle.muted = true;
        p
    }

    fn project_pattern(name: &str, project: &str) -> Pattern {
        let mut p = make_pattern(name, "project-specific", 0.7);
        p.base.applies.projects = vec![project.to_owned()];
        p
    }

    #[test]
    fn build_excludes_archived() {
        let patterns = vec![
            make_pattern("active", "Active pattern", 0.8),
            archived_pattern("gone"),
        ];
        let idx = build(&patterns, None);
        assert_eq!(idx.entries.len(), 1);
        assert_eq!(idx.entries[0].name, "active");
    }

    #[test]
    fn build_excludes_muted() {
        let patterns = vec![
            make_pattern("visible", "Visible", 0.8),
            muted_pattern("silent"),
        ];
        let idx = build(&patterns, None);
        assert_eq!(idx.entries.len(), 1);
    }

    #[test]
    fn build_sorts_by_importance_descending() {
        let patterns = vec![
            make_pattern("low", "Low", 0.3),
            make_pattern("high", "High", 0.9),
            make_pattern("mid", "Mid", 0.6),
        ];
        let idx = build(&patterns, None);
        assert_eq!(idx.entries[0].name, "high");
        assert_eq!(idx.entries[1].name, "mid");
        assert_eq!(idx.entries[2].name, "low");
    }

    #[test]
    fn build_with_project_filters_correctly() {
        let patterns = vec![
            make_pattern("universal", "Universal (empty applies.projects)", 0.8),
            project_pattern("for-mur", "mur"),
            project_pattern("for-other", "other-project"),
        ];
        let idx = build(&patterns, Some("mur"));
        let names: Vec<_> = idx.entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"universal"));
        assert!(names.contains(&"for-mur"));
        assert!(!names.contains(&"for-other"));
    }

    #[test]
    fn build_with_no_project_includes_universal_only() {
        let patterns = vec![
            make_pattern("universal", "Universal", 0.8),
            project_pattern("specific", "mur"),
        ];
        let idx = build(&patterns, None);
        assert_eq!(idx.entries.len(), 1);
        assert_eq!(idx.entries[0].name, "universal");
    }

    #[test]
    fn build_empty_patterns_gives_empty_index() {
        let idx = build(&[], None);
        assert!(idx.entries.is_empty());
    }

    #[test]
    fn build_includes_deprecated_patterns() {
        let mut p = make_pattern(
            "deprecated-but-visible",
            "Still in index until archived",
            0.7,
        );
        p.base.lifecycle.status = LifecycleStatus::Deprecated;
        let idx = build(&[p], None);
        assert_eq!(
            idx.entries.len(),
            1,
            "Deprecated patterns should appear in the index (only Archived is excluded)"
        );
    }

    // ── Task 2: format_l0 ──────────────────────────────────────────────────────

    #[test]
    fn format_l0_produces_header_and_list() {
        let idx = CapabilityIndex {
            entries: vec![
                CapabilityEntry {
                    name: "tokio-async".into(),
                    description: "Tokio async runtime patterns".into(),
                },
                CapabilityEntry {
                    name: "clap-derive".into(),
                    description: "Clap derive API for CLI parsing".into(),
                },
            ],
            project: Some("mur".into()),
        };
        let out = format_l0(&idx, 4000);
        assert!(out.contains("## mur learning index"), "must have header");
        assert!(out.contains("project: mur"), "must show project name");
        assert!(out.contains("`tokio-async`"), "must list pattern names");
        assert!(
            out.contains("Tokio async runtime patterns"),
            "must include descriptions"
        );
        assert!(out.contains("mur recall"), "must have recall footer");
    }

    #[test]
    fn format_l0_without_project_omits_project_name() {
        let idx = CapabilityIndex {
            entries: vec![CapabilityEntry {
                name: "anyhow".into(),
                description: "Error handling".into(),
            }],
            project: None,
        };
        let out = format_l0(&idx, 4000);
        assert!(out.contains("## mur learning index"), "must have header");
        assert!(!out.contains("project:"), "must not show project when None");
    }

    #[test]
    fn format_l0_empty_index_returns_empty_string() {
        let idx = CapabilityIndex {
            entries: vec![],
            project: None,
        };
        assert_eq!(format_l0(&idx, 4000), "");
    }

    #[test]
    fn format_l0_truncates_at_budget() {
        let entries: Vec<_> = (0..20)
            .map(|i| CapabilityEntry {
                name: format!("pattern-{i:02}"),
                description: format!("Description number {i}"),
            })
            .collect();
        let idx = CapabilityIndex {
            entries,
            project: None,
        };
        let out = format_l0(&idx, 250);
        assert!(
            !out.contains("pattern-19"),
            "should truncate before last entry"
        );
        assert!(out.contains("## mur learning index"));
    }

    #[test]
    fn format_l0_respects_600_token_cap() {
        let entries: Vec<_> = (0..50)
            .map(|i| CapabilityEntry {
                name: format!("pat-{i}"),
                description: "A reasonably long description for a pattern entry here".into(),
            })
            .collect();
        let idx = CapabilityIndex {
            entries,
            project: Some("myproject".into()),
        };
        let out = format_l0(&idx, 2400);
        // budget 2400 + ~60 footer chars (header already within budget)
        assert!(
            out.len() <= 2400 + 120,
            "output must fit within token budget (with footer), got {} chars",
            out.len()
        );
    }

    // ── Task 3: save/load ──────────────────────────────────────────────────────

    #[test]
    fn save_and_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("capabilities.json");

        let idx = CapabilityIndex {
            entries: vec![
                CapabilityEntry {
                    name: "foo".into(),
                    description: "Foo pattern".into(),
                },
                CapabilityEntry {
                    name: "bar".into(),
                    description: "Bar pattern".into(),
                },
            ],
            project: Some("testproject".into()),
        };

        save_to(&idx, &path).unwrap();
        let loaded = load_from(&path).unwrap();
        assert_eq!(loaded.entries.len(), 2);
        assert_eq!(loaded.entries[0].name, "foo");
        assert_eq!(loaded.project.as_deref(), Some("testproject"));
    }

    #[test]
    fn load_from_missing_file_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.json");
        let loaded = load_from(&path).unwrap();
        assert!(loaded.entries.is_empty());
        assert!(loaded.project.is_none());
    }
}

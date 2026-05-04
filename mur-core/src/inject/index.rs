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

#[allow(dead_code)]
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
}

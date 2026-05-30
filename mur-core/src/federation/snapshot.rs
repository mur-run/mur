use anyhow::{Context, Result};
use mur_common::agent::{PatternFilter, SnapshotRef};
use mur_common::pattern::Pattern;
use sha2::{Digest, Sha256};

/// Filter `patterns` according to `filter` and return matching subset,
/// sorted by importance descending, capped at `filter.max_count`.
pub(crate) fn apply_filter(patterns: Vec<Pattern>, filter: &PatternFilter) -> Vec<Pattern> {
    let mut matched: Vec<Pattern> = patterns
        .into_iter()
        .filter(|p| {
            // tier filter: match against the lowercase Debug name of the Tier enum
            if !filter.tier.is_empty() {
                let tier_str = format!("{:?}", p.tier).to_lowercase();
                if !filter
                    .tier
                    .iter()
                    .any(|t| tier_str.contains(&t.to_lowercase()))
                {
                    return false;
                }
            }
            // maturity filter: match against the lowercase Debug name of the Maturity enum
            if !filter.maturity.is_empty() {
                let mat_str = format!("{:?}", p.maturity).to_lowercase();
                if !filter
                    .maturity
                    .iter()
                    .any(|m| mat_str.contains(&m.to_lowercase()))
                {
                    return false;
                }
            }
            // importance filter
            if p.importance < filter.importance_min {
                return false;
            }
            // applies_in filter: match against applies.projects (contexts are project-scoped)
            if !filter.applies_in.is_empty() {
                let projects: Vec<String> = p
                    .applies
                    .projects
                    .iter()
                    .map(|c| c.to_lowercase())
                    .collect();
                if !filter
                    .applies_in
                    .iter()
                    .any(|a| projects.contains(&a.to_lowercase()))
                {
                    return false;
                }
            }
            true
        })
        .collect();

    // Sort by importance descending
    matched.sort_by(|a, b| {
        b.importance
            .partial_cmp(&a.importance)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    matched.truncate(filter.max_count);
    matched
}

/// Pull a pattern snapshot for `agent_name`:
/// 1. Load all patterns from the default YamlStore.
/// 2. Apply the filter.
/// 3. Write matching patterns to `~/.mur/agents/<agent_name>/patterns_cache/<name>.yaml`.
/// 4. Write `.snapshot-ref` with the knowledge HEAD SHA.
/// 5. Return the SnapshotRef.
pub fn pull_snapshot(agent_name: &str, filter: &PatternFilter) -> Result<SnapshotRef> {
    let store = crate::store::yaml::YamlStore::default_store()?;
    let all = store.list_all()?;
    let filtered = apply_filter(all, filter);

    let cache_dir = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("no home dir"))?
        .join(".mur/agents")
        .join(agent_name)
        .join("patterns_cache");
    std::fs::create_dir_all(&cache_dir)
        .with_context(|| format!("create patterns_cache for {agent_name}"))?;

    for pattern in &filtered {
        let yaml = serde_yaml_ng::to_string(pattern)
            .with_context(|| format!("serialize pattern {}", pattern.name))?;
        let dest = cache_dir.join(format!("{}.yaml", pattern.name));
        let tmp = dest.with_extension("yaml.tmp");
        std::fs::write(&tmp, &yaml)?;
        std::fs::rename(&tmp, &dest)?;
    }

    // Get knowledge HEAD SHA (best-effort; empty string if no git repo yet).
    let knowledge_sha = get_knowledge_head_sha().unwrap_or_default();

    let snap_ref = SnapshotRef {
        knowledge_commit: knowledge_sha,
        taken_at: chrono::Utc::now().to_rfc3339(),
        filter: filter.clone(),
    };
    write_snapshot_ref(agent_name, &snap_ref, &cache_dir)?;
    Ok(snap_ref)
}

/// Read the current snapshot ref for `agent_name`. Returns None if no snapshot exists.
pub fn read_snapshot_ref(agent_name: &str) -> Result<Option<SnapshotRef>> {
    let path = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("no home dir"))?
        .join(".mur/agents")
        .join(agent_name)
        .join("patterns_cache/.snapshot-ref");
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(&path)?;
    let snap: SnapshotRef = serde_yaml_ng::from_str(&content)
        .with_context(|| format!("parse .snapshot-ref for {agent_name}"))?;
    Ok(Some(snap))
}

pub(crate) fn write_snapshot_ref(
    agent_name: &str,
    snap: &SnapshotRef,
    cache_dir: &std::path::Path,
) -> Result<()> {
    let yaml = serde_yaml_ng::to_string(snap)
        .with_context(|| format!("serialize snapshot-ref for {agent_name}"))?;
    let dest = cache_dir.join(".snapshot-ref");
    let tmp = cache_dir.join(".snapshot-ref.tmp");
    std::fs::write(&tmp, yaml)?;
    std::fs::rename(&tmp, &dest)?;
    Ok(())
}

fn get_knowledge_head_sha() -> Result<String> {
    let mur_dir = crate::store::yaml::default_mur_dir();
    let repo = git2::Repository::open(&mur_dir)
        .with_context(|| format!("open knowledge repo at {}", mur_dir.display()))?;
    let head = repo.head()?;
    let commit = head.peel_to_commit()?;
    let full_sha = commit.id().to_string();
    Ok(full_sha[..12].to_string())
}

// ── Skill snapshot ───────────────────────────────────────────────────

/// Filter for skill snapshots. Mirrors `PatternFilter` for skill queries.
#[derive(Debug, Default, Clone)]
pub struct SkillFilter {
    pub categories: Vec<String>,
    pub max_count: usize,
}

impl SkillFilter {
    pub fn new(categories: Vec<String>, max_count: usize) -> Self {
        Self {
            categories,
            max_count,
        }
    }
}

/// Return value from `pull_skill_snapshot`.
#[derive(Debug, Clone)]
pub struct SkillSnapshot {
    pub head: String,
}

/// Pull a skill snapshot for `agent_name`:
/// 1. Load all `skill.yaml` files from `~/.mur/skills/`.
/// 2. Apply the filter (categories, max_count).
/// 3. Copy matching `skill.yaml` files to `~/.mur/agents/<agent>/skills_cache/<name>/skill.yaml`.
/// 4. Write a `.snapshot-ref` file with the set of skill names.
/// 5. Return the SkillSnapshot.
pub fn pull_skill_snapshot(agent_name: &str, filter: &SkillFilter) -> Result<SkillSnapshot> {
    let mur_home = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("no home dir"))?
        .join(".mur");
    pull_skill_snapshot_from(agent_name, filter, &mur_home)
}

pub fn pull_skill_snapshot_from(
    agent_name: &str,
    filter: &SkillFilter,
    mur_home: &std::path::Path,
) -> Result<SkillSnapshot> {
    let skills_dir = mur_home.join("skills");
    let cache_dir = mur_home
        .join("agents")
        .join(agent_name)
        .join("skills_cache");
    std::fs::create_dir_all(&cache_dir)?;

    let mut matched: Vec<(String, String)> = Vec::new(); // (name, yaml_content)
    if skills_dir.exists() {
        for entry in std::fs::read_dir(&skills_dir)? {
            let path = entry?.path();
            let skill_yaml = path.join("skill.yaml");
            if !skill_yaml.is_file() {
                continue;
            }
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_string();
            let content = std::fs::read_to_string(&skill_yaml)?;
            // Category filter
            if !filter.categories.is_empty() {
                let in_cat = filter
                    .categories
                    .iter()
                    .any(|c| content.contains(c.as_str()));
                if !in_cat {
                    continue;
                }
            }
            matched.push((name, content));
        }
    }

    if filter.max_count > 0 {
        matched.truncate(filter.max_count);
    }

    let mut hasher = Sha256::new();
    for (name, content) in &matched {
        let dest_dir = cache_dir.join(name);
        std::fs::create_dir_all(&dest_dir)?;
        std::fs::write(dest_dir.join("skill.yaml"), content)?;
        hasher.update(name.as_bytes());
        hasher.update(content.as_bytes());
    }

    let head = format!("{:x}", hasher.finalize());
    std::fs::write(cache_dir.join(".snapshot-ref"), &head)?;
    Ok(SkillSnapshot { head })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::agent::PatternFilter;

    fn make_pattern(name: &str, tier: &str, importance: f64) -> mur_common::pattern::Pattern {
        let yaml = format!(
            r#"name: {name}
description: test
content:
  technical: ""
  principle: ~
tier: {tier}
importance: {importance}
confidence: 0.8
tags: {{}}
applies: {{}}
evidence: {{}}
links: []
lifecycle: {{}}
"#
        );
        serde_yaml_ng::from_str(&yaml)
            .unwrap_or_else(|e| panic!("parse test pattern '{name}': {e}"))
    }

    #[test]
    fn test_apply_filter_tier_core() {
        let patterns = vec![
            make_pattern("p1", "core", 0.9),
            make_pattern("p2", "project", 0.8),
            make_pattern("p3", "core", 0.7),
        ];
        let filter = PatternFilter {
            tier: vec!["core".into()],
            ..Default::default()
        };
        let result = apply_filter(patterns, &filter);
        assert_eq!(result.len(), 2);
        assert!(result.iter().all(|p| p.name == "p1" || p.name == "p3"));
    }

    #[test]
    fn test_apply_filter_importance_min() {
        let patterns = vec![
            make_pattern("p1", "project", 0.9),
            make_pattern("p2", "project", 0.3),
            make_pattern("p3", "project", 0.5),
        ];
        let filter = PatternFilter {
            importance_min: 0.5,
            ..Default::default()
        };
        let result = apply_filter(patterns, &filter);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_apply_filter_max_count() {
        let patterns: Vec<_> = (0..10)
            .map(|i| make_pattern(&format!("p{i}"), "project", i as f64 * 0.1))
            .collect();
        let filter = PatternFilter {
            max_count: 3,
            ..Default::default()
        };
        let result = apply_filter(patterns, &filter);
        assert_eq!(result.len(), 3);
        // Sorted by importance desc: p9 (0.9), p8 (0.8), p7 (0.7)
        assert!((result[0].importance - 0.9).abs() < f64::EPSILON);
    }

    #[test]
    fn test_snapshot_ref_roundtrip() {
        let dir = tempfile::TempDir::new().unwrap();
        let snap = SnapshotRef {
            knowledge_commit: "abc123def456".into(),
            taken_at: "2026-05-20T00:00:00Z".into(),
            filter: PatternFilter::default(),
        };
        write_snapshot_ref("test-agent", &snap, dir.path()).unwrap();
        // Read back directly
        let content = std::fs::read_to_string(dir.path().join(".snapshot-ref")).unwrap();
        let back: SnapshotRef = serde_yaml_ng::from_str(&content).unwrap();
        assert_eq!(snap, back);
    }

    #[test]
    fn pull_skill_snapshot_writes_cache_and_returns_ref() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        // Stub two skills
        for name in ["skill-a", "skill-b"] {
            let d = dir.path().join("skills").join(name);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(
                d.join("skill.yaml"),
                format!("name: {name}\nversion: 1.0.0\ncategory: context\n"),
            )
            .unwrap();
        }
        let filter = SkillFilter {
            categories: vec!["context".into()],
            max_count: 10,
        };
        let snap =
            pull_skill_snapshot_from("test-agent", &filter, dir.path()).unwrap();
        let cache_dir = dir.path().join("agents/test-agent/skills_cache");
        assert!(cache_dir.exists());
        // at least one .yaml in the cache
        let entries: Vec<_> = std::fs::read_dir(&cache_dir).unwrap().collect();
        assert!(!entries.is_empty());
        assert!(!snap.head.is_empty());
    }
}

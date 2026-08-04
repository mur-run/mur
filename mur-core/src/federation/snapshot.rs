use anyhow::{Context, Result};
use mur_common::agent::SnapshotRef;
use mur_common::skill::lifecycle::lifecycle_rank;
use mur_common::skill::stats::{LifecycleState, SkillStats};
use serde::{Deserialize, Serialize};
use std::path::Path;

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

// `read_snapshot_ref` / `write_snapshot_ref` outlive the retired Pattern pull:
// `cmd/eval.rs` still reads the Pattern-era ref for snapshot age. Once eval
// migrates to `read_skill_snapshot_ref`, both can go.
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

// ── Skill snapshot (memory federation P0) ────────────────────────────
// Spec: docs/superpowers/specs/2026-08-04-unified-memory-federation.md.
// Replaces both the retired Pattern pull above AND an earlier never-wired
// `pull_skill_snapshot` (zero callers, naive category-substring filter).

/// What one assembly wrote. Serialized as `knowledge_cache/.snapshot-ref`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillSnapshotRef {
    pub skill_count: usize,
    pub taken_at: chrono::DateTime<chrono::Utc>,
}

/// Global skills at or above `floor`, with their states. Shared by the
/// assembly and the CLI dry-run so both always agree on eligibility.
pub(crate) fn eligible_skills(
    mur_home: &Path,
    floor: LifecycleState,
) -> Result<Vec<(String, LifecycleState)>> {
    let mut out = Vec::new();
    for name in mur_common::skill::local::list_installed(mur_home)
        .map_err(|e| anyhow::anyhow!("list installed skills: {e}"))?
    {
        // Skills without stats have never been initialised — treat as Draft
        // so the curation floor keeps them local until they mature.
        let state = SkillStats::load(&SkillStats::path(mur_home, &name))?
            .map(|s| s.lifecycle_state)
            .unwrap_or(LifecycleState::Draft);
        if lifecycle_rank(state) >= lifecycle_rank(floor) {
            out.push((name, state));
        }
    }
    Ok(out)
}

/// Central-side skill snapshot: copy every global skill at or above the
/// configured lifecycle floor into
/// `agents/<agent_name>/knowledge_cache/<skill>/skill.yaml`, rebuilding the
/// cache from scratch — a skill demoted below the floor (or deleted
/// centrally) must disappear from the cache. Per-agent skills are NOT
/// copied: they already live in the agent home and win name collisions in
/// the loader. Notes join in federation P1.
pub fn assemble_skill_snapshot(mur_home: &Path, agent_name: &str) -> Result<SkillSnapshotRef> {
    let cfg = mur_common::config::Config::load_or_default(&mur_home.join("config.yaml"));
    let eligible = eligible_skills(mur_home, cfg.federation_snapshot.min_lifecycle)?;

    let cache = mur_home
        .join("agents")
        .join(agent_name)
        .join("knowledge_cache");
    if cache.exists() {
        std::fs::remove_dir_all(&cache)
            .with_context(|| format!("clear knowledge_cache for {agent_name}"))?;
    }
    std::fs::create_dir_all(&cache)?;

    let mut count = 0usize;
    for (name, _state) in &eligible {
        let src = mur_home.join("skills").join(name).join("skill.yaml");
        if !src.exists() {
            continue; // run-ledger dirs and other non-skill entries
        }
        let dst_dir = cache.join(name);
        std::fs::create_dir_all(&dst_dir)?;
        // tmp+rename so a crashed assembly never leaves a half-written yaml.
        let tmp = dst_dir.join(".skill.yaml.tmp");
        std::fs::copy(&src, &tmp)?;
        std::fs::rename(&tmp, dst_dir.join("skill.yaml"))?;
        count += 1;
    }

    let snap = SkillSnapshotRef {
        skill_count: count,
        taken_at: chrono::Utc::now(),
    };
    let yaml = serde_yaml_ng::to_string(&snap)?;
    let tmp = cache.join(".snapshot-ref.tmp");
    std::fs::write(&tmp, &yaml)?;
    std::fs::rename(&tmp, cache.join(".snapshot-ref"))?;

    // The Pattern-era cache is dead (patterns removed in workflow-engine v2
    // P1a/P1b); remove it when empty, leave it with a warning otherwise.
    let old = mur_home
        .join("agents")
        .join(agent_name)
        .join("patterns_cache");
    if old.exists() {
        match std::fs::read_dir(&old).map(|mut d| d.next().is_none()) {
            Ok(true) => {
                let _ = std::fs::remove_dir(&old);
            }
            _ => tracing::warn!(
                agent = agent_name,
                "patterns_cache is non-empty; leaving it (patterns are retired)"
            ),
        }
    }
    Ok(snap)
}

/// Read the skill-snapshot ref for `agent_name`, or None before any pull.
pub fn read_skill_snapshot_ref(
    mur_home: &Path,
    agent_name: &str,
) -> Result<Option<SkillSnapshotRef>> {
    let path = mur_home
        .join("agents")
        .join(agent_name)
        .join("knowledge_cache/.snapshot-ref");
    if !path.exists() {
        return Ok(None);
    }
    let snap = serde_yaml_ng::from_str(&std::fs::read_to_string(&path)?)
        .with_context(|| format!("parse knowledge_cache .snapshot-ref for {agent_name}"))?;
    Ok(Some(snap))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::agent::PatternFilter;

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

    // ── skill snapshot (federation P0) ──────────────────────────────────

    /// Global skill fixture: canonical manifest + stats.json at `state`.
    /// The manifest must be loader-parseable (not just copyable) because the
    /// smoke test walks the full assemble → loader path.
    fn write_skill_fixture(mur_home: &Path, name: &str, state: LifecycleState) {
        let d = mur_home.join("skills").join(name);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("skill.yaml"),
            format!(
                r#"name: {name}
version: 1.0.0
publisher: human:t
description: fixture
category: context
content:
  abstract: hi
  context: body
"#
            ),
        )
        .unwrap();
        let mut stats = SkillStats::new(name, "1.0.0", "digest", chrono::Utc::now());
        stats.lifecycle_state = state;
        std::fs::write(
            SkillStats::path(mur_home, name),
            serde_json::to_string(&stats).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn assemble_filters_below_floor() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();
        write_skill_fixture(home, "mature-skill", LifecycleState::Stable);
        write_skill_fixture(home, "raw-skill", LifecycleState::Draft);

        let snap = assemble_skill_snapshot(home, "t-agent").unwrap();

        assert_eq!(snap.skill_count, 1, "only the Stable skill qualifies");
        let cache = home.join("agents/t-agent/knowledge_cache");
        assert!(cache.join("mature-skill/skill.yaml").exists());
        assert!(!cache.join("raw-skill").exists());
        let re = read_skill_snapshot_ref(home, "t-agent").unwrap().unwrap();
        assert_eq!(re.skill_count, 1);
    }

    #[test]
    fn assemble_rebuild_drops_demoted() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();
        write_skill_fixture(home, "wobbly", LifecycleState::Stable);
        assemble_skill_snapshot(home, "t-agent").unwrap();
        assert!(
            home.join("agents/t-agent/knowledge_cache/wobbly/skill.yaml")
                .exists()
        );

        // Demote below the floor; a re-pull must drop it from the cache.
        write_skill_fixture(home, "wobbly", LifecycleState::Draft);
        let snap = assemble_skill_snapshot(home, "t-agent").unwrap();
        assert_eq!(snap.skill_count, 0);
        assert!(!home.join("agents/t-agent/knowledge_cache/wobbly").exists());
    }

    #[test]
    fn assemble_removes_empty_patterns_cache() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();
        std::fs::create_dir_all(home.join("agents/t-agent/patterns_cache")).unwrap();
        assemble_skill_snapshot(home, "t-agent").unwrap();
        assert!(
            !home.join("agents/t-agent/patterns_cache").exists(),
            "empty Pattern-era cache must be cleaned up"
        );
    }

    #[test]
    fn smoke_assemble_then_loader_sees_the_skill() {
        // P0 exit criterion (spec): one pull → cache → loadable for injection.
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();
        std::fs::create_dir_all(home.join("agents/smoke")).unwrap();
        write_skill_fixture(home, "smoke-skill", LifecycleState::Stable);

        assemble_skill_snapshot(home, "smoke").unwrap();

        let loaded = mur_common::skill::loader::load_all(home, "smoke");
        assert!(
            loaded.iter().any(|s| s.name == "smoke-skill"),
            "cached skill must be visible to the injection loader"
        );
    }
}

//! Cross-agent Jaccard + vector dedup (M7a Task 5 + Task 9).
//!
//! Reuses M5b's `dedup::tokens` / `dedup::jaccard` and M6c.1's vector scan
//! to find duplicate skills across peer agents. M7a is read-only on peer
//! state — `--apply` only writes the cross-agent JSONL report.

use std::path::Path;

use anyhow::Result;
use mur_common::skill::local::list_installed_agent;
use mur_common::skill::peers::list_peer_agents;
use mur_common::skill::stats::{LifecycleState, SkillStats};

use crate::skill_consolidate::{SkillView, dedup};
use crate::store::embedding::EmbeddingConfig;
use crate::store::vector::VectorStore;

const JACCARD_THRESHOLD: f64 = 0.85;

/// Dedup method selector for cross-agent consolidate.
#[derive(Debug, Clone)]
pub enum CrossAgentMethod {
    Jaccard,
    Vector,
    Both,
}

pub struct CrossAgentSkillView {
    pub view: SkillView,
    pub agent: String,
}

#[derive(Debug, serde::Serialize)]
pub struct CrossAgentDuplicatePair {
    pub a_agent: String,
    pub a_skill: String,
    pub b_agent: String,
    pub b_skill: String,
    pub similarity: f64,
    pub keeper_agent: String,
    pub keeper_skill: String,
    #[serde(default)]
    pub similarity_source: crate::skill_consolidate::dedup::DedupSource,
}

#[derive(Debug, Default)]
pub struct CrossAgentReport {
    pub duplicates: Vec<CrossAgentDuplicatePair>,
}

/// Synchronous entry point for Jaccard-only (backward-compatible with existing callers).
pub fn run_consolidate_cross_agent(home: &Path, apply: bool) -> Result<CrossAgentReport> {
    let views = load_all_peer_views(home)?;
    let mut report = CrossAgentReport::default();

    scan_cross_agent_duplicates(&views, &mut report);

    write_cross_agent_jsonl(home, &report, apply)?;

    Ok(report)
}

/// Async entry point supporting vector / both methods.
pub async fn run_consolidate_cross_agent_with_method(
    home: &Path,
    apply: bool,
    method: CrossAgentMethod,
    embed_config: &EmbeddingConfig,
    store: &dyn VectorStore,
) -> Result<CrossAgentReport> {
    let views = load_all_peer_views(home)?;
    let mut report = CrossAgentReport::default();

    match &method {
        CrossAgentMethod::Jaccard => {
            scan_cross_agent_duplicates(&views, &mut report);
        }
        CrossAgentMethod::Vector => {
            index_peer_skills(&views, embed_config, store).await?;
            scan_cross_agent_vector(&views, embed_config, store, &mut report).await?;
        }
        CrossAgentMethod::Both => {
            scan_cross_agent_duplicates(&views, &mut report);
            index_peer_skills(&views, embed_config, store).await?;
            scan_cross_agent_vector(&views, embed_config, store, &mut report).await?;
            dedup_combined_cross_agent(&mut report);
        }
    }

    write_cross_agent_jsonl(home, &report, apply)?;

    Ok(report)
}

fn load_all_peer_views(home: &Path) -> Result<Vec<CrossAgentSkillView>> {
    let mut out = Vec::new();
    for peer in list_peer_agents(home)? {
        let agent_home = &peer.home_path;
        for skill_name in
            list_installed_agent(home, &peer.name).map_err(|e| anyhow::anyhow!("{e}"))?
        {
            let manifest_path = agent_home
                .join("skills")
                .join(&skill_name)
                .join("skill.yaml");
            let Ok(text) = std::fs::read_to_string(&manifest_path) else {
                continue;
            };
            let Ok(m) = mur_common::skill::parser::parse_canonical(&text) else {
                continue;
            };

            let embed_text = crate::skill_index::text::embed_manifest(&m);

            let stats_path = SkillStats::path_agent(home, &peer.name, &skill_name);
            let stats = SkillStats::load(&stats_path)?
                .unwrap_or_else(|| SkillStats::new(&skill_name, "unknown", "", chrono::Utc::now()));

            let view = SkillView {
                name: skill_name.clone(),
                description: m.description,
                triggers: m.triggers.into_iter().filter_map(|t| t.pattern).collect(),
                requires: m.requires.into_iter().map(|r| r.name).collect(),
                stats,
                embed_text,
            };
            out.push(CrossAgentSkillView {
                view,
                agent: peer.name.clone(),
            });
        }
    }
    Ok(out)
}

/// Index peer skill manifests into the vector store so vector search can find them.
async fn index_peer_skills(
    views: &[CrossAgentSkillView],
    config: &EmbeddingConfig,
    store: &dyn VectorStore,
) -> anyhow::Result<()> {
    for csv in views {
        // Parse the manifest to get a proper SkillManifest for indexing.
        // We re-parse here since we only have the SkillView at this point.
        // The embed_text is already built; we regenerate it from the manifest
        // for the chunk text to match the standard format.
        let text_str = &csv.view.embed_text;
        if text_str.is_empty() {
            continue;
        }
        let vec = crate::store::embedding::embed(text_str, config).await?;

        let chunk = crate::store::vector::EmbeddedChunk {
            chunk_id: format!("skill:{}:{}", csv.view.name, csv.agent),
            source_id: crate::skill_index::SKILL_SOURCE_ID.into(),
            external_id: csv.view.name.clone(),
            ordinal: 0,
            text: text_str.clone(),
            heading_path: vec![],
            char_range: (0, 0),
            updated_at: chrono::Utc::now(),
            embedding: vec,
        };
        store.upsert(&[chunk]).await?;
    }
    Ok(())
}

/// Vector-based cross-agent duplicate scan.
///
/// Embeds each skill's text, searches the vector store for similar chunks,
/// and reports pairs where different agents have semantically similar skills.
async fn scan_cross_agent_vector(
    views: &[CrossAgentSkillView],
    config: &EmbeddingConfig,
    store: &dyn VectorStore,
    report: &mut CrossAgentReport,
) -> anyhow::Result<()> {
    use crate::skill_consolidate::dedup_vec::{COSINE_THRESHOLD, TOP_K};
    use crate::store::vector::SearchFilter;

    let filter = SearchFilter {
        source_ids: Some(vec![crate::skill_index::SKILL_SOURCE_ID.into()]),
        since: None,
    };

    // Track which pairs we've already reported to avoid duplicates from
    // symmetric vector hits.
    let mut seen: std::collections::HashSet<(String, String, String, String)> =
        std::collections::HashSet::new();

    for csv in views {
        if csv.view.embed_text.is_empty() {
            continue;
        }
        let q = crate::store::embedding::embed(&csv.view.embed_text, config).await?;
        let hits = store.search(&q, TOP_K, &filter).await?;
        for hit in hits {
            if hit.score < COSINE_THRESHOLD {
                continue;
            }
            // Find all peer views that match this external_id (skill name).
            for other in views {
                if other.view.name != hit.external_id {
                    continue;
                }
                if other.agent == csv.agent {
                    continue;
                }
                // Build canonical pair key (agent order, skill order).
                let pair = if csv.agent < other.agent
                    || (csv.agent == other.agent && csv.view.name < other.view.name)
                {
                    (
                        csv.agent.clone(),
                        csv.view.name.clone(),
                        other.agent.clone(),
                        other.view.name.clone(),
                    )
                } else {
                    (
                        other.agent.clone(),
                        other.view.name.clone(),
                        csv.agent.clone(),
                        csv.view.name.clone(),
                    )
                };
                if seen.contains(&pair) {
                    continue;
                }
                seen.insert(pair.clone());

                let (keeper_agent, keeper_skill) = select_keeper(csv, other);
                report.duplicates.push(CrossAgentDuplicatePair {
                    a_agent: pair.0,
                    a_skill: pair.1,
                    b_agent: pair.2,
                    b_skill: pair.3,
                    similarity: hit.score as f64,
                    keeper_agent,
                    keeper_skill,
                    similarity_source: crate::skill_consolidate::dedup::DedupSource::Vector,
                });
            }
        }
    }
    Ok(())
}

fn scan_cross_agent_duplicates(views: &[CrossAgentSkillView], report: &mut CrossAgentReport) {
    for i in 0..views.len() {
        for j in (i + 1)..views.len() {
            let a = &views[i];
            let b = &views[j];

            if a.agent == b.agent {
                continue;
            }

            let sim = dedup::jaccard(&dedup::tokens(&a.view), &dedup::tokens(&b.view));
            if sim >= JACCARD_THRESHOLD {
                let (keeper_agent, keeper_skill) = select_keeper(a, b);
                report.duplicates.push(CrossAgentDuplicatePair {
                    a_agent: a.agent.clone(),
                    a_skill: a.view.name.clone(),
                    b_agent: b.agent.clone(),
                    b_skill: b.view.name.clone(),
                    similarity: sim,
                    keeper_agent,
                    keeper_skill,
                    similarity_source: crate::skill_consolidate::dedup::DedupSource::Jaccard,
                });
            }
        }
    }
}

/// Collapse (a,b) pairs appearing under both Jaccard and Vector into a single entry
/// with `similarity_source = Both`. Preserves the higher similarity score.
fn dedup_combined_cross_agent(report: &mut CrossAgentReport) {
    use crate::skill_consolidate::dedup::DedupSource;
    use std::collections::HashMap;

    let mut by_pair: HashMap<(String, String, String, String), CrossAgentDuplicatePair> =
        HashMap::new();
    for p in report.duplicates.drain(..) {
        let key = (
            p.a_agent.clone(),
            p.a_skill.clone(),
            p.b_agent.clone(),
            p.b_skill.clone(),
        );
        by_pair
            .entry(key)
            .and_modify(|existing| {
                if existing.similarity_source != p.similarity_source {
                    existing.similarity_source = DedupSource::Both;
                }
                if p.similarity > existing.similarity {
                    existing.similarity = p.similarity;
                }
            })
            .or_insert(p);
    }
    report.duplicates = by_pair.into_values().collect();
    report.duplicates.sort_by(|a, b| {
        a.a_agent
            .cmp(&b.a_agent)
            .then_with(|| a.a_skill.cmp(&b.a_skill))
    });
}

fn select_keeper(a: &CrossAgentSkillView, b: &CrossAgentSkillView) -> (String, String) {
    let prefer_a = match (a.view.stats.lifecycle_state, b.view.stats.lifecycle_state) {
        (la, lb) if lifecycle_rank(la) > lifecycle_rank(lb) => true,
        (la, lb) if lifecycle_rank(la) < lifecycle_rank(lb) => false,
        _ => match (a.view.stats.success_count, b.view.stats.success_count) {
            (sa, sb) if sa > sb => true,
            (sa, sb) if sa < sb => false,
            _ => a.agent.cmp(&b.agent).is_lt(),
        },
    };
    if prefer_a {
        (a.agent.clone(), a.view.name.clone())
    } else {
        (b.agent.clone(), b.view.name.clone())
    }
}

fn lifecycle_rank(s: LifecycleState) -> u8 {
    match s {
        LifecycleState::Destroyed => 0,
        LifecycleState::Archived => 1,
        LifecycleState::Deprecated => 2,
        LifecycleState::Draft => 3,
        LifecycleState::Emerging => 4,
        LifecycleState::Stable => 5,
        LifecycleState::Canonical => 6,
    }
}

fn write_cross_agent_jsonl(home: &Path, report: &CrossAgentReport, applied: bool) -> Result<()> {
    let date = chrono::Utc::now().format("%Y-%m-%d");
    let dir = home.join("skills").join("_consolidation");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("cross-agent-{date}.jsonl"));
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    use std::io::Write;
    for d in &report.duplicates {
        let row = serde_json::json!({
            "type": "cross_agent_duplicate",
            "a_agent": d.a_agent,
            "a_skill": d.a_skill,
            "b_agent": d.b_agent,
            "b_skill": d.b_skill,
            "similarity": d.similarity,
            "similarity_source": d.similarity_source,
            "keeper_agent": d.keeper_agent,
            "keeper_skill": d.keeper_skill,
            "applied": applied,
            "applied_at": applied.then(|| chrono::Utc::now().to_rfc3339()),
        });
        writeln!(file, "{row}")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn detects_cross_agent_duplicate() {
        let dir = tempdir().unwrap();
        let home = dir.path();

        // Alice: skill "research"
        setup_agent_skill(
            home,
            "alice",
            "research",
            "web search engine information retrieval knowledge lookup",
        );
        // Bob: skill "lookup" — nearly identical tokens (differs only by one word)
        setup_agent_skill(
            home,
            "bob",
            "lookup",
            "web search engine information retrieval knowledge research",
        );
        // Carol: unrelated skill
        setup_agent_skill(
            home,
            "carol",
            "calculator",
            "Simple arithmetic calculator for basic math operations",
        );

        let views = load_all_peer_views(home).unwrap();
        assert_eq!(views.len(), 3);

        let mut report = CrossAgentReport::default();
        scan_cross_agent_duplicates(&views, &mut report);

        // alice:research and bob:lookup should be duplicates
        assert_eq!(report.duplicates.len(), 1);
        let dup = &report.duplicates[0];
        assert!(
            (dup.a_agent == "alice" && dup.b_agent == "bob")
                || (dup.a_agent == "bob" && dup.b_agent == "alice")
        );
        assert!(dup.similarity >= JACCARD_THRESHOLD);
    }

    #[test]
    fn dedup_combined_cross_agent_merges_sources() {
        let mut report = CrossAgentReport::default();
        report.duplicates.push(CrossAgentDuplicatePair {
            a_agent: "alice".into(),
            a_skill: "s1".into(),
            b_agent: "bob".into(),
            b_skill: "s2".into(),
            similarity: 0.88,
            keeper_agent: "alice".into(),
            keeper_skill: "s1".into(),
            similarity_source: crate::skill_consolidate::dedup::DedupSource::Jaccard,
        });
        report.duplicates.push(CrossAgentDuplicatePair {
            a_agent: "alice".into(),
            a_skill: "s1".into(),
            b_agent: "bob".into(),
            b_skill: "s2".into(),
            similarity: 0.94,
            keeper_agent: "alice".into(),
            keeper_skill: "s1".into(),
            similarity_source: crate::skill_consolidate::dedup::DedupSource::Vector,
        });
        dedup_combined_cross_agent(&mut report);
        assert_eq!(report.duplicates.len(), 1);
        let p = &report.duplicates[0];
        assert_eq!(
            p.similarity_source,
            crate::skill_consolidate::dedup::DedupSource::Both
        );
        assert!((p.similarity - 0.94).abs() < 0.001);
    }

    fn setup_agent_skill(home: &Path, agent: &str, skill_name: &str, description: &str) {
        let skills_dir = home
            .join("agents")
            .join(agent)
            .join("skills")
            .join(skill_name);
        std::fs::create_dir_all(&skills_dir).unwrap();

        let manifest = format!(
            "name: {skill_name}\nversion: 1.0.0\npublisher: human:test\ncategory: context\ndescription: {description}\ncontent:\n  abstract: test\n  context: test body\n"
        );
        std::fs::write(skills_dir.join("skill.yaml"), manifest).unwrap();

        let stats = SkillStats::new(skill_name, "1.0.0", "abc", chrono::Utc::now());
        let stats_path = SkillStats::path_agent(home, agent, skill_name);
        std::fs::create_dir_all(stats_path.parent().unwrap()).unwrap();
        std::fs::write(&stats_path, serde_json::to_string_pretty(&stats).unwrap()).unwrap();
    }
}

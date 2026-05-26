//! Cross-agent Jaccard consolidate (M7a Task 5).
//!
//! Reuses M5b's `dedup::tokens` / `dedup::jaccard` to find duplicate skills
//! across peer agents. M7a is read-only on peer state — `--apply` only writes
//! the cross-agent JSONL report.

use std::path::Path;

use anyhow::Result;
use mur_common::skill::local::list_installed_agent;
use mur_common::skill::peers::list_peer_agents;
use mur_common::skill::stats::{LifecycleState, SkillStats};

use crate::skill_consolidate::{SkillView, dedup};

const JACCARD_THRESHOLD: f64 = 0.85;

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
}

#[derive(Debug, Default)]
pub struct CrossAgentReport {
    pub duplicates: Vec<CrossAgentDuplicatePair>,
}

pub fn run_consolidate_cross_agent(home: &Path, apply: bool) -> Result<CrossAgentReport> {
    let views = load_all_peer_views(home)?;
    let mut report = CrossAgentReport::default();

    scan_cross_agent_duplicates(&views, &mut report);

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

            let stats_path = SkillStats::path_agent(home, &peer.name, &skill_name);
            let stats = SkillStats::load(&stats_path)?
                .unwrap_or_else(|| SkillStats::new(&skill_name, "unknown", "", chrono::Utc::now()));

            let view = SkillView {
                name: skill_name.clone(),
                description: m.description,
                triggers: m.triggers.into_iter().filter_map(|t| t.pattern).collect(),
                requires: m.requires.into_iter().map(|r| r.name).collect(),
                stats,
                embed_text: String::new(),
            };
            out.push(CrossAgentSkillView {
                view,
                agent: peer.name.clone(),
            });
        }
    }
    Ok(out)
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
                });
            }
        }
    }
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
        LifecycleState::Archived => 0,
        LifecycleState::Deprecated => 1,
        LifecycleState::Draft => 2,
        LifecycleState::Emerging => 3,
        LifecycleState::Stable => 4,
        LifecycleState::Canonical => 5,
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

//! Cross-agent skill-stats aggregation (M7a Task 3).

use std::path::Path;

use mur_common::skill::peers::list_peer_agents;
use mur_common::skill::stats::SkillStats;

#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentSkillStats {
    pub agent: String,
    pub skill: String,
    pub usage_count: u64,
    pub success_count: u64,
    pub failure_count: u64,
    pub last_used_at: Option<chrono::DateTime<chrono::Utc>>,
    pub lifecycle: String,
}

/// Loads stats for `skill_name` from every peer agent + the global skills dir.
/// Missing stats files are silently skipped.
pub fn aggregate_skill_stats(
    home: &Path,
    skill_name: &str,
) -> anyhow::Result<Vec<AgentSkillStats>> {
    let mut rows = Vec::new();

    let global_path = SkillStats::path(home, skill_name);
    if global_path.exists()
        && let Some(stats) = SkillStats::load(&global_path)?
    {
        rows.push(row_from_stats("(global)", skill_name, &stats));
    }

    for peer in list_peer_agents(home)? {
        let path = SkillStats::path_agent(home, &peer.name, skill_name);
        if !path.exists() {
            continue;
        }
        if let Some(stats) = SkillStats::load(&path)? {
            rows.push(row_from_stats(&peer.name, skill_name, &stats));
        }
    }

    Ok(rows)
}

fn row_from_stats(agent: &str, skill: &str, s: &SkillStats) -> AgentSkillStats {
    AgentSkillStats {
        agent: agent.to_string(),
        skill: skill.to_string(),
        usage_count: s.usage_count,
        success_count: s.success_count,
        failure_count: s.failure_count,
        last_used_at: s.last_used_at,
        lifecycle: format!("{:?}", s.lifecycle_state),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::skill::stats::SkillStats;
    use tempfile::tempdir;

    #[test]
    fn aggregates_across_agents() {
        let dir = tempdir().unwrap();
        let home = dir.path();

        // Alice has the skill with some usage
        let alice_skills = home
            .join("agents")
            .join("alice")
            .join("skills")
            .join("target-skill");
        std::fs::create_dir_all(&alice_skills).unwrap();
        let mut alice_stats = SkillStats::new("target-skill", "1.0.0", "abc", chrono::Utc::now());
        alice_stats.usage_count = 10;
        alice_stats.success_count = 9;
        alice_stats.failure_count = 1;
        let alice_path = SkillStats::path_agent(home, "alice", "target-skill");
        std::fs::create_dir_all(alice_path.parent().unwrap()).unwrap();
        std::fs::write(
            &alice_path,
            serde_json::to_string_pretty(&alice_stats).unwrap(),
        )
        .unwrap();

        // Bob does not have the skill
        std::fs::create_dir_all(home.join("agents").join("bob").join("skills")).unwrap();

        // Carol has the skill with different stats
        let carol_skills = home
            .join("agents")
            .join("carol")
            .join("skills")
            .join("target-skill");
        std::fs::create_dir_all(&carol_skills).unwrap();
        let mut carol_stats = SkillStats::new("target-skill", "1.0.0", "abc", chrono::Utc::now());
        carol_stats.usage_count = 5;
        carol_stats.success_count = 5;
        let carol_path = SkillStats::path_agent(home, "carol", "target-skill");
        std::fs::create_dir_all(carol_path.parent().unwrap()).unwrap();
        std::fs::write(
            &carol_path,
            serde_json::to_string_pretty(&carol_stats).unwrap(),
        )
        .unwrap();

        let rows = aggregate_skill_stats(home, "target-skill").unwrap();
        assert_eq!(rows.len(), 2);
        assert!(
            rows.iter()
                .any(|r| r.agent == "alice" && r.usage_count == 10)
        );
        assert!(
            rows.iter()
                .any(|r| r.agent == "carol" && r.usage_count == 5)
        );
    }

    #[test]
    fn empty_when_no_agent_has_skill() {
        let dir = tempdir().unwrap();
        let home = dir.path();
        std::fs::create_dir_all(home.join("agents").join("alice").join("skills")).unwrap();
        let rows = aggregate_skill_stats(home, "nonexistent").unwrap();
        assert!(rows.is_empty());
    }
}

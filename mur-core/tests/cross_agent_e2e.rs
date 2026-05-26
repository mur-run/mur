//! E2E integration test for M7a cross-agent observability.
//!
//! Builds a synthetic three-agent fixture and exercises the full
//! cross-agent flow: peers → stats aggregation → fitness → consolidate.

use chrono::{Duration, Utc};
use mur_common::skill::peers::list_peer_agents;
use mur_common::skill::stats::SkillStats;
use std::path::Path;
use tempfile::tempdir;

fn write_stats(path: &Path, stats: &SkillStats) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, serde_json::to_string_pretty(stats).unwrap()).unwrap();
}

fn write_manifest(home: &Path, agent: &str, skill_name: &str, description: &str) {
    let dir = home
        .join("agents")
        .join(agent)
        .join("skills")
        .join(skill_name);
    std::fs::create_dir_all(&dir).unwrap();
    let manifest = format!(
        "name: {skill_name}\n\
         version: 1.0.0\n\
         publisher: human:test\n\
         category: context\n\
         description: {description}\n\
         content:\n  abstract: test\n  context: test body\n"
    );
    std::fs::write(dir.join("skill.yaml"), manifest).unwrap();
}

#[test]
fn cross_agent_e2e() {
    let dir = tempdir().unwrap();
    let home = dir.path();

    // ── Fixture: three agents with varied skills ──────────────────────

    let now = Utc::now();

    // Alice: two skills, one used recently
    write_manifest(
        home,
        "alice",
        "shared-skill",
        "web search engine information knowledge retrieval find lookup",
    );
    write_manifest(home, "alice", "alice-only", "alice specific tool");
    let mut alice_shared = SkillStats::new("shared-skill", "1.0.0", "abc", now);
    alice_shared.usage_count = 20;
    alice_shared.success_count = 18;
    alice_shared.failure_count = 2;
    alice_shared.last_used_at = Some(now - Duration::hours(2));
    write_stats(
        &SkillStats::path_agent(home, "alice", "shared-skill"),
        &alice_shared,
    );
    let mut alice_only = SkillStats::new("alice-only", "1.0.0", "abc", now);
    alice_only.usage_count = 5;
    alice_only.success_count = 5;
    write_stats(
        &SkillStats::path_agent(home, "alice", "alice-only"),
        &alice_only,
    );

    // Bob: has shared-skill too, with duplicate-prone content
    write_manifest(
        home,
        "bob",
        "shared-skill",
        "web search engine information knowledge lookup find retrieval",
    );
    write_manifest(home, "bob", "bob-only", "bob specific tool");
    let mut bob_shared = SkillStats::new("shared-skill", "1.0.0", "abc", now);
    bob_shared.usage_count = 10;
    bob_shared.success_count = 8;
    bob_shared.failure_count = 2;
    bob_shared.last_used_at = Some(now - Duration::days(10));
    write_stats(
        &SkillStats::path_agent(home, "bob", "shared-skill"),
        &bob_shared,
    );

    // Carol: different skill set, no overlap
    write_manifest(
        home,
        "carol",
        "carol-tool",
        "completely different math calculator",
    );
    let mut carol_tool = SkillStats::new("carol-tool", "1.0.0", "abc", now);
    carol_tool.usage_count = 0;
    write_stats(
        &SkillStats::path_agent(home, "carol", "carol-tool"),
        &carol_tool,
    );

    // ── Test 1: Peer enumeration ──────────────────────────────────────

    let peers = list_peer_agents(home).unwrap();
    assert_eq!(peers.len(), 3, "should find alice, bob, carol");
    let names: Vec<&str> = peers.iter().map(|p| p.name.as_str()).collect();
    assert!(names.contains(&"alice"));
    assert!(names.contains(&"bob"));
    assert!(names.contains(&"carol"));

    // ── Test 2: Cross-agent stats aggregation ─────────────────────────

    let rows =
        mur_core::cross_agent::stats_agg::aggregate_skill_stats(home, "shared-skill").unwrap();
    assert_eq!(rows.len(), 2, "alice and bob both have shared-skill");
    assert!(
        rows.iter()
            .any(|r| r.agent == "alice" && r.usage_count == 20)
    );
    assert!(rows.iter().any(|r| r.agent == "bob" && r.usage_count == 10));

    let rows_none =
        mur_core::cross_agent::stats_agg::aggregate_skill_stats(home, "nonexistent").unwrap();
    assert!(rows_none.is_empty());

    // ── Test 3: Agent fitness ─────────────────────────────────────────

    let alice_fit = mur_core::cross_agent::fitness::fitness(home, "alice", now, 7, 0.1).unwrap();
    assert_eq!(alice_fit.agent, "alice");
    assert_eq!(alice_fit.sample_size, 25); // 20 + 5
    assert!((alice_fit.success_rate - 0.92).abs() < 0.01); // 23/25
    assert!(alice_fit.weight > 0.9); // recent usage → high weight

    let carol_fit = mur_core::cross_agent::fitness::fitness(home, "carol", now, 7, 0.1).unwrap();
    assert_eq!(carol_fit.sample_size, 0);
    assert_eq!(carol_fit.weight, 0.0);
    assert_eq!(carol_fit.recency_decay, 0.1); // floor

    // ── Test 4: Cross-agent consolidate ───────────────────────────────

    let report =
        mur_core::cross_agent::consolidate::run_consolidate_cross_agent(home, false).unwrap();

    // alice:shared-skill and bob:shared-skill have nearly identical
    // descriptions and the same name — Jaccard = 1.0
    assert_eq!(report.duplicates.len(), 1, "one cross-agent duplicate pair");
    let dup = &report.duplicates[0];
    assert!(
        (dup.a_agent == "alice"
            && dup.b_agent == "bob"
            && dup.a_skill == "shared-skill"
            && dup.b_skill == "shared-skill")
            || (dup.a_agent == "bob" && dup.b_agent == "alice"),
        "duplicate should be alice:shared-skill ≈ bob:shared-skill"
    );
    assert!(dup.similarity >= 0.85);
    assert_eq!(dup.keeper_skill, "shared-skill");

    // JSONL report should exist
    let date = now.format("%Y-%m-%d").to_string();
    let jsonl_path = home
        .join("skills")
        .join("_consolidation")
        .join(format!("cross-agent-{date}.jsonl"));
    assert!(
        jsonl_path.exists(),
        "cross-agent JSONL report should be written"
    );

    let content = std::fs::read_to_string(&jsonl_path).unwrap();
    assert!(content.contains("cross_agent_duplicate"));
}

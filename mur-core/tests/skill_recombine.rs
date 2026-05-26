//! M7b integration tests — cross-agent recombination scenarios.

use mur_common::skill::stats::LifecycleState;
use mur_common::skill::stats::SkillStats;
use mur_core::cross_agent::recombine::peer_ref::parse_ref;
use mur_core::cross_agent::recombine::{RecombineOptions, RecombineStrategy, run_recombine};
use std::path::Path;
use tempfile::TempDir;

fn write_skill(home: &Path, agent: &str, name: &str, yaml: &str) {
    let dir = home.join("agents").join(agent).join("skills").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("skill.yaml"), yaml).unwrap();
}

fn write_stats(
    home: &Path,
    agent: &str,
    skill: &str,
    success: u64,
    failure: u64,
    last_used: chrono::DateTime<chrono::Utc>,
) {
    let mut s = SkillStats::new(skill, "0.1.0", "", last_used);
    s.success_count = success;
    s.failure_count = failure;
    s.usage_count = success + failure;
    s.last_used_at = Some(last_used);
    let path = SkillStats::path_agent(home, agent, skill);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let json = serde_json::to_string_pretty(&s).unwrap();
    std::fs::write(&path, json).unwrap();
}

fn skill_yaml(name: &str, trigger: &str, intent: &str, desc: &str) -> String {
    format!(
        r#"name: {name}
version: 0.1.0
publisher: human:test
description: test
category: workflow
content:
  abstract: a
  procedure:
    steps:
      - description: {desc}
        intent: {intent}
triggers:
  - type: command
    pattern: "{trigger}"
priority: normal
"#
    )
}

#[tokio::test]
async fn same_agent_union_writes_offspring() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    write_skill(home, "self", "a", &skill_yaml("a", "/a", "i1", "do A"));
    write_skill(home, "self", "b", &skill_yaml("b", "/b", "i2", "do B"));

    let opts = RecombineOptions {
        a_ref: parse_ref("a").unwrap(),
        b_ref: parse_ref("b").unwrap(),
        strategy: RecombineStrategy::Union,
        output_name: Some("merged".into()),
        dry_run: false,
        current_agent: "self".into(),
    };
    let outcome = run_recombine(home, &opts).await.unwrap();
    assert_eq!(outcome.output_name, "merged");
    let out_yaml = std::fs::read_to_string(outcome.written_to.unwrap()).unwrap();
    assert!(out_yaml.contains("name: merged"));
    assert!(out_yaml.contains("/a"));
    assert!(out_yaml.contains("/b"));
}

#[tokio::test]
async fn cross_agent_intersection_picks_higher_success_step() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    let now = chrono::Utc::now();
    write_skill(
        home,
        "self",
        "find",
        &skill_yaml("find", "/find", "search", "self version"),
    );
    write_skill(
        home,
        "peer1",
        "find",
        &skill_yaml("find", "/find", "search", "peer version"),
    );
    write_stats(home, "self", "find", 1, 9, now); // 10% success
    write_stats(home, "peer1", "find", 9, 1, now); // 90% success

    let opts = RecombineOptions {
        a_ref: parse_ref("find").unwrap(),
        b_ref: parse_ref("agent://peer1/find").unwrap(),
        strategy: RecombineStrategy::Intersection,
        output_name: Some("find-merged".into()),
        dry_run: false,
        current_agent: "self".into(),
    };
    let outcome = run_recombine(home, &opts).await.unwrap();
    let out_yaml = std::fs::read_to_string(outcome.written_to.unwrap()).unwrap();
    assert!(
        out_yaml.contains("peer version"),
        "should pick higher success_rate peer version"
    );
}

#[tokio::test]
async fn dry_run_writes_nothing_to_disk() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    write_skill(home, "self", "a", &skill_yaml("a", "/a", "i1", "A"));
    write_skill(home, "self", "b", &skill_yaml("b", "/b", "i2", "B"));

    let opts = RecombineOptions {
        a_ref: parse_ref("a").unwrap(),
        b_ref: parse_ref("b").unwrap(),
        strategy: RecombineStrategy::Union,
        output_name: Some("x".into()),
        dry_run: true,
        current_agent: "self".into(),
    };
    let outcome = run_recombine(home, &opts).await.unwrap();
    assert!(outcome.written_to.is_none());
    assert!(!home.join("agents/self/skills/x/skill.yaml").exists());
}

#[tokio::test]
async fn offspring_lands_at_draft_lifecycle() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    write_skill(home, "self", "a", &skill_yaml("a", "/a", "i1", "A"));
    write_skill(home, "self", "b", &skill_yaml("b", "/b", "i2", "B"));

    let opts = RecombineOptions {
        a_ref: parse_ref("a").unwrap(),
        b_ref: parse_ref("b").unwrap(),
        strategy: RecombineStrategy::Union,
        output_name: Some("c".into()),
        dry_run: false,
        current_agent: "self".into(),
    };
    run_recombine(home, &opts).await.unwrap();
    let stats = SkillStats::load(&SkillStats::path_agent(home, "self", "c"))
        .unwrap()
        .unwrap();
    assert!(matches!(stats.lifecycle_state, LifecycleState::Draft));
}

#[tokio::test]
async fn evolution_log_contains_recombined_event() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    write_skill(home, "self", "a", &skill_yaml("a", "/a", "i1", "A"));
    write_skill(home, "self", "b", &skill_yaml("b", "/b", "i2", "B"));

    let opts = RecombineOptions {
        a_ref: parse_ref("a").unwrap(),
        b_ref: parse_ref("b").unwrap(),
        strategy: RecombineStrategy::Union,
        output_name: Some("d".into()),
        dry_run: false,
        current_agent: "self".into(),
    };
    let outcome = run_recombine(home, &opts).await.unwrap();
    let entry = &outcome.manifest.evolution_log[0];
    assert_eq!(entry.source, "agent:recombiner");
    assert!(entry.changes.contains("strategy=union"));
    assert!(entry.changes.contains("output=d"));
}

#[tokio::test]
async fn name_collision_returns_error() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    write_skill(home, "self", "a", &skill_yaml("a", "/a", "i1", "A"));
    write_skill(home, "self", "b", &skill_yaml("b", "/b", "i2", "B"));
    write_skill(
        home,
        "self",
        "exists",
        &skill_yaml("exists", "/x", "ix", "X"),
    );

    let opts = RecombineOptions {
        a_ref: parse_ref("a").unwrap(),
        b_ref: parse_ref("b").unwrap(),
        strategy: RecombineStrategy::Union,
        output_name: Some("exists".into()),
        dry_run: false,
        current_agent: "self".into(),
    };
    let err = run_recombine(home, &opts).await.unwrap_err();
    assert!(err.to_string().contains("already exists"));
}

#[tokio::test]
async fn llm_strategy_without_model_returns_error() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    write_skill(home, "self", "a", &skill_yaml("a", "/a", "i1", "A"));
    write_skill(home, "self", "b", &skill_yaml("b", "/b", "i2", "B"));

    let opts = RecombineOptions {
        a_ref: parse_ref("a").unwrap(),
        b_ref: parse_ref("b").unwrap(),
        strategy: RecombineStrategy::Llm,
        output_name: Some("llm-out".into()),
        dry_run: false,
        current_agent: "self".into(),
    };
    let err = run_recombine(home, &opts).await.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("LLM") || msg.contains("model") || msg.contains("mur model add"),
        "unexpected error: {msg}"
    );
}

//! E2E: build canonical mapping from synthetic agents, then verify idempotency.
use std::fs;

use mur_core::cross_agent::intent::canonical::build_canonical;
use tempfile::tempdir;

fn write_skill_manifest(home: &std::path::Path, agent: &str, skill: &str, intents: &[&str]) {
    let skills_dir = home.join("agents").join(agent).join("skills").join(skill);
    fs::create_dir_all(&skills_dir).unwrap();
    let mut steps = String::new();
    for intent in intents {
        steps.push_str(&format!(
            "    - description: \"do {intent}\"\n      intent: \"{intent}\"\n"
        ));
    }
    let yaml = format!(
        "name: {skill}\nversion: \"1.0.0\"\npublisher: human:test\ndescription: test\ncategory: context\ncontent:\n  abstract: a\n  procedure:\n    mode: procedure\n    steps:\n{steps}"
    );
    fs::write(skills_dir.join("skill.yaml"), yaml).unwrap();
}

fn write_agent_profile(home: &std::path::Path, agent: &str) {
    let dir = home.join("agents").join(agent);
    fs::create_dir_all(&dir).unwrap();
    let profile = format!("name: {agent}\ndisplay_name: {agent}\n");
    fs::write(dir.join("profile.yaml"), profile).unwrap();
}

#[test]
fn builds_canonical_from_agent_intents() {
    let d = tempdir().unwrap();
    let home = d.path();

    write_agent_profile(home, "alice");
    write_agent_profile(home, "bob");

    write_skill_manifest(home, "alice", "research", &["Web Search", "web_search"]);
    write_skill_manifest(home, "bob", "lookup", &["web-search", "Web Search"]);

    let ic = build_canonical(home, "test").unwrap();

    // All four intents normalise to "web_search" — one cluster.
    assert_eq!(ic.canonical.len(), 1);
    assert_eq!(ic.canonical[0].canonical, "Web Search");
    assert_eq!(ic.canonical[0].count, 4);
    assert!(ic.canonical[0].aliases.contains(&"Web Search".to_string()));
    assert!(ic.canonical[0].aliases.contains(&"web_search".to_string()));
    assert!(ic.canonical[0].aliases.contains(&"web-search".to_string()));
}

#[test]
fn multiple_clusters_sorted_by_count() {
    let d = tempdir().unwrap();
    let home = d.path();

    write_agent_profile(home, "alice");
    write_skill_manifest(
        home,
        "alice",
        "s1",
        &["Web Search", "web_search", "Web Search"],
    );
    write_skill_manifest(home, "alice", "s2", &["Run Tests", "run_tests"]);

    let ic = build_canonical(home, "test").unwrap();

    assert_eq!(ic.canonical.len(), 2);
    // Most frequent first.
    assert_eq!(ic.canonical[0].canonical, "Web Search");
    assert_eq!(ic.canonical[0].count, 3);
    assert_eq!(ic.canonical[1].canonical, "Run Tests");
    assert_eq!(ic.canonical[1].count, 2);
}

#[test]
fn empty_host_yields_empty_canonical() {
    let d = tempdir().unwrap();
    let home = d.path();
    // No agents at all.
    let ic = build_canonical(home, "test").unwrap();
    assert!(ic.canonical.is_empty());
}

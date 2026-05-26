//! Credit synthesis from manifest evolution_log when the ledger is empty.
use std::fs;

use mur_core::cross_agent::credit::aggregate::build_credit_view;
use tempfile::tempdir;

#[test]
fn synthesises_mutator_entries_from_evolution_log() {
    let d = tempdir().unwrap();
    let home = d.path();

    // Create an agent with a skill whose manifest has an evolution_log.
    let skill_dir = home
        .join("agents")
        .join("alice")
        .join("skills")
        .join("demo");
    fs::create_dir_all(&skill_dir).unwrap();

    let manifest = r#"
name: demo
version: "1.2.0"
publisher: human:alice
description: test
category: context
content:
  abstract: a
  context: b
evolution_log:
  - version: "1.0.0"
    timestamp: "2026-05-20T10:00:00Z"
    source: "human:alice"
    generation: 0
    changes: "initial creation"
  - version: "1.1.0"
    timestamp: "2026-05-22T10:00:00Z"
    source: "human:alice"
    generation: 1
    changes: "added search intent"
  - version: "1.2.0"
    timestamp: "2026-05-24T10:00:00Z"
    source: "human:alice"
    generation: 2
    changes: "improved error handling"
"#;
    fs::write(skill_dir.join("skill.yaml"), manifest).unwrap();

    let view = build_credit_view(home, "alice", "demo").unwrap();

    // generation 0 is skipped; generations 1 and 2 become Mutator entries.
    let mutators: Vec<_> = view
        .entries
        .iter()
        .filter(|e| matches!(e.kind, mur_common::skill::credit::CreditKind::Mutator))
        .collect();
    assert_eq!(mutators.len(), 2);
    // They should have from_version set correctly.
    let versions: Vec<&str> = mutators.iter().map(|e| e.skill_version.as_str()).collect();
    assert!(versions.contains(&"1.1.0"));
    assert!(versions.contains(&"1.2.0"));
}

#[test]
fn no_evolution_log_yields_no_synthetic_entries() {
    let d = tempdir().unwrap();
    let home = d.path();

    let skill_dir = home
        .join("agents")
        .join("alice")
        .join("skills")
        .join("simple");
    fs::create_dir_all(&skill_dir).unwrap();
    fs::write(
        skill_dir.join("skill.yaml"),
        "name: simple\nversion: \"1.0.0\"\npublisher: human:alice\ndescription: test\ncategory: context\ncontent:\n  abstract: a\n  context: b\n",
    ).unwrap();

    let view = build_credit_view(home, "alice", "simple").unwrap();
    assert!(view.entries.is_empty());
}

//! Credit aggregation: entries from peers + invoker all appear in the view.
use std::fs;

use chrono::Utc;
use mur_common::skill::credit::{CreditEntry, CreditKind};
use mur_core::cross_agent::credit::aggregate::build_credit_view;
use mur_core::cross_agent::credit::ledger::{append, ledger_path_for_agent};
use tempfile::tempdir;

fn entry(skill: &str, kind: CreditKind, agent: &str) -> CreditEntry {
    CreditEntry {
        ts: Utc::now(),
        skill: skill.into(),
        skill_version: "1.0.0".into(),
        kind,
        agent: agent.into(),
        evidence: None,
        source: format!("human:{agent}"),
    }
}

#[test]
fn aggregates_entries_from_peers() {
    let d = tempdir().unwrap();
    let home = d.path();

    // Alice authored the skill.
    fs::create_dir_all(ledger_path_for_agent(home, "alice").parent().unwrap()).unwrap();
    append(home, "alice", &entry("research", CreditKind::Author, "alice")).unwrap();

    // Bob propagated it.
    fs::create_dir_all(ledger_path_for_agent(home, "bob").parent().unwrap()).unwrap();
    append(home, "bob", &entry("research", CreditKind::Propagator, "bob")).unwrap();

    // Charlie (invoker) evolved it.
    fs::create_dir_all(ledger_path_for_agent(home, "charlie").parent().unwrap()).unwrap();
    append(home, "charlie", &entry("research", CreditKind::Mutator, "charlie")).unwrap();

    let view = build_credit_view(home, "charlie", "research").unwrap();

    assert_eq!(view.skill, "research");
    assert_eq!(view.entries.len(), 3);
    // Sorted by ts, all three agents should appear.
    let agents: Vec<&str> = view.entries.iter().map(|e| e.agent.as_str()).collect();
    assert!(agents.contains(&"alice"));
    assert!(agents.contains(&"bob"));
    assert!(agents.contains(&"charlie"));
}

#[test]
fn empty_view_when_nobody_has_skill() {
    let d = tempdir().unwrap();
    let home = d.path();
    fs::create_dir_all(home.join("agents").join("alice")).unwrap();

    let view = build_credit_view(home, "alice", "nonexistent").unwrap();
    assert!(view.entries.is_empty());
}

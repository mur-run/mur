//! Verifies that a propagate sweep does NOT mutate peer agent files.
use std::fs;
use std::path::Path;

use mur_core::cross_agent::propagate::{PropagateOptions, run_propagate};
use tempfile::tempdir;

fn fingerprint(dir: &Path) -> Vec<(String, u64)> {
    let mut out = Vec::new();
    for entry in walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if entry.file_type().is_file() {
            let md = entry.metadata().unwrap();
            out.push((entry.path().display().to_string(), md.len()));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[test]
fn propagate_does_not_modify_peers() {
    let d = tempdir().unwrap();
    let home = d.path();

    // Build peer "alice" with a skill and stats that would pass all gates.
    let alice_skills = home
        .join("agents")
        .join("alice")
        .join("skills")
        .join("research");
    fs::create_dir_all(&alice_skills).unwrap();
    fs::write(
        alice_skills.join("skill.yaml"),
        "name: research\nversion: \"1.0.0\"\npublisher: human:test\ndescription: test\ncategory: context\ncontent:\n  abstract: a\n  context: b\n",
    )
    .unwrap();

    // Write SkillStats for alice's "research" skill with high success count.
    let stats_dir = home
        .join("agents")
        .join("alice")
        .join("skills")
        .join("research");
    fs::create_dir_all(&stats_dir).unwrap();
    let stats_json = serde_json::json!({
        "schema_version": 1,
        "skill_name": "research",
        "skill_version": "1.0.0",
        "manifest_digest": "abc123",
        "lifecycle_state": "stable",
        "lifecycle_changed_at": "2026-05-26T00:00:00Z",
        "pinned": false,
        "pinned_reason": "",
        "usage_count": 100,
        "success_count": 95,
        "failure_count": 5,
        "last_used_at": "2026-05-26T00:00:00Z",
        "last_success_at": "2026-05-26T00:00:00Z",
        "first_successful_use_at": "2026-05-20T00:00:00Z",
        "anchor_confidence": 0.9,
        "rebuilt_from_trace_through": null,
        "resolution_misses": 0
    });
    fs::write(
        stats_dir.join("stats.json"),
        serde_json::to_string(&stats_json).unwrap(),
    )
    .unwrap();

    // Create invoker "bob" with no skills.
    fs::create_dir_all(home.join("agents").join("bob").join("skills")).unwrap();

    // Snapshot peer dirs before.
    let before = fingerprint(&home.join("agents").join("alice"));

    let mut opts = PropagateOptions::default();
    opts.gates.min_samples = 1;
    opts.gates.min_fitness = 0.0;
    opts.gates.min_source_weight = 0.0;
    let _ = run_propagate(home, "bob", &opts).unwrap();

    let after = fingerprint(&home.join("agents").join("alice"));
    assert_eq!(
        before, after,
        "peer state mutated by propagate sweep — M7a invariant violated"
    );
}

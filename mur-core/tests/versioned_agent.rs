//! Integration tests for `VersionedAgentStore` (E1 W2).
//!
//! Validates ADR-0001 FIN-1/2/3/4 for the execution layer.

use mur_core::store::versioned::agent::VersionedAgentStore;
use std::time::Instant;
use tempfile::TempDir;

fn temp_store() -> (TempDir, VersionedAgentStore) {
    let dir = TempDir::new().unwrap();
    let store = VersionedAgentStore::init(dir.path()).unwrap();
    (dir, store)
}

fn profile(content: &str) -> String {
    format!("name: test\ndescription: {content}\n")
}

// ── Basic CRUD ────────────────────────────────────────────────────────────────

#[test]
fn save_and_read_profile_roundtrip() {
    let (_dir, mut store) = temp_store();

    let rev = store
        .save_profile("agent-a", &profile("v1"), "init")
        .unwrap();
    assert_eq!(rev.version, 1);
    assert_eq!(rev.name, "agent-a");
    assert!(!rev.sha.is_empty());

    let loaded = store.read_profile("agent-a").unwrap().unwrap();
    assert!(loaded.contains("v1"));
}

#[test]
fn read_missing_agent_returns_none() {
    let (_dir, store) = temp_store();
    assert!(store.read_profile("nonexistent").unwrap().is_none());
}

// ── FIN-2: O(1) version derivation ────────────────────────────────────────────

#[test]
fn version_increments_on_each_save() {
    let (_dir, mut store) = temp_store();

    let r1 = store
        .save_profile("agent-b", &profile("v1"), "init")
        .unwrap();
    assert_eq!(r1.version, 1);

    let r2 = store
        .save_profile("agent-b", &profile("v2"), "update")
        .unwrap();
    assert_eq!(r2.version, 2);

    let r3 = store
        .save_profile("agent-b", &profile("v3"), "refine")
        .unwrap();
    assert_eq!(r3.version, 3);

    assert_eq!(store.current_version("agent-b"), 3);
}

#[test]
fn no_op_save_does_not_increment_version() {
    let (_dir, mut store) = temp_store();
    let content = profile("unchanged");

    let r1 = store.save_profile("agent-c", &content, "init").unwrap();
    let r2 = store.save_profile("agent-c", &content, "same").unwrap();

    assert_eq!(r1.version, 1);
    assert_eq!(r2.version, 1, "no-op must not bump version");
}

// ── FIN-3: history from index ─────────────────────────────────────────────────

#[test]
fn history_populated_after_saves() {
    let (_dir, mut store) = temp_store();

    store
        .save_profile("agent-d", &profile("v1"), "init")
        .unwrap();
    store
        .save_profile("agent-d", &profile("v2"), "update skills")
        .unwrap();

    let hist = store.history("agent-d").unwrap();
    assert_eq!(hist.len(), 2);
    assert_eq!(hist[0].version, 1);
    assert_eq!(hist[0].reason, "init");
    assert_eq!(hist[1].version, 2);
    assert_eq!(hist[1].reason, "update skills");
}

#[test]
fn history_empty_for_unknown_agent() {
    let (_dir, store) = temp_store();
    assert!(store.history("phantom").unwrap().is_empty());
}

#[test]
fn history_fast_on_many_revisions() {
    let (_dir, mut store) = temp_store();

    for i in 1..=50u32 {
        store
            .save_profile(
                "perf-agent",
                &profile(&format!("v{i}")),
                &format!("rev {i}"),
            )
            .unwrap();
    }

    let t = Instant::now();
    let hist = store.history("perf-agent").unwrap();
    let elapsed = t.elapsed();

    assert_eq!(hist.len(), 50);
    assert!(
        elapsed.as_millis() < 100,
        "history() took {}ms (FIN-3 target < 100ms)",
        elapsed.as_millis()
    );
}

// ── Skill operations ──────────────────────────────────────────────────────────

#[test]
fn save_and_read_skill() {
    let (_dir, mut store) = temp_store();

    store
        .save_profile("agent-e", &profile("v1"), "init")
        .unwrap();
    store
        .save_skill(
            "agent-e",
            "rust-basics",
            "# Rust basics\nUse Result.",
            "add skill",
        )
        .unwrap();

    let content = store.read_skill("agent-e", "rust-basics").unwrap().unwrap();
    assert!(content.contains("Rust basics"));
}

// ── Rollback ─────────────────────────────────────────────────────────────────

#[test]
fn rollback_restores_profile_as_new_version() {
    let (_dir, mut store) = temp_store();

    store
        .save_profile("agent-f", &profile("original"), "init")
        .unwrap(); // v1
    store
        .save_profile("agent-f", &profile("modified"), "update")
        .unwrap(); // v2

    let rev = store.rollback_profile("agent-f", 1).unwrap(); // v3 = rollback to v1
    assert_eq!(rev.version, 3);

    let loaded = store.read_profile("agent-f").unwrap().unwrap();
    assert!(
        !loaded.contains("modified"),
        "rollback should revert to v1 content"
    );
}

#[test]
fn rollback_missing_version_errors() {
    let (_dir, mut store) = temp_store();
    store
        .save_profile("agent-g", &profile("v1"), "init")
        .unwrap();

    let err = store.rollback_profile("agent-g", 99).unwrap_err();
    assert!(err.to_string().contains("no archived"));
}

// ── detect_external_change & rebuild_index ────────────────────────────────────

#[test]
fn detect_external_change_false_after_save() {
    let (_dir, mut store) = temp_store();
    store
        .save_profile("agent-h", &profile("v1"), "init")
        .unwrap();
    assert!(!store.detect_external_change().unwrap());
}

#[test]
fn rebuild_index_restores_history() {
    let (_dir, mut store) = temp_store();
    store
        .save_profile("agent-i", &profile("v1"), "init")
        .unwrap();
    store
        .save_profile("agent-i", &profile("v2"), "update")
        .unwrap();

    store.rebuild_index().unwrap();

    let hist = store.history("agent-i").unwrap();
    assert_eq!(hist.len(), 2);
    assert_eq!(hist[0].reason, "init");
    assert_eq!(hist[1].reason, "update");
}

// ── Split-brain repair ────────────────────────────────────────────────────────

#[test]
fn repair_recovers_missing_git_dir() {
    let dir = TempDir::new().unwrap();
    let agents_dir = dir.path();

    // Create agent dir with profile, but no .git
    let agent_dir = agents_dir.join("orphan-agent");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(agent_dir.join("profile.yaml"), profile("orphaned")).unwrap();

    let report = VersionedAgentStore::repair(agents_dir).unwrap();
    assert!(report.recovered);
    assert_eq!(report.agents_recommitted, 1); // one agent dir recovered

    // After repair, should be openable
    let store = VersionedAgentStore::open(agents_dir).unwrap();
    // Profile should be readable (it was committed in the repair commit)
    assert!(store.read_profile("orphan-agent").unwrap().is_some());
}

#[test]
fn repair_noop_when_git_healthy() {
    let (_dir, store) = temp_store();
    let report = VersionedAgentStore::repair(store.root()).unwrap();
    assert!(!report.recovered);
}

// ── FIN-4: gitignore correctness ──────────────────────────────────────────────

#[test]
fn gitignore_ignores_runtime_files() {
    let (_dir, store) = temp_store();
    let repo = git2::Repository::open(store.root()).unwrap();

    for path in [
        "my-agent/telemetry/events.jsonl",
        "my-agent/crashlogs/last.txt",
        "my-agent/running.lock",
        "my-agent/.extract_digest",
        "my-agent/.apply-staging/tmp",
    ] {
        assert!(
            repo.is_path_ignored(path).unwrap_or(false),
            "'{path}' should be gitignored (FIN-4)"
        );
    }
}

#[test]
fn gitignore_tracks_profile_and_skills() {
    let (_dir, store) = temp_store();
    let repo = git2::Repository::open(store.root()).unwrap();

    for path in [
        "my-agent/profile.yaml",
        "my-agent/skills/rust.md",
        "my-agent/sys_prompt.md",
        "my-agent/identity.pub",
    ] {
        assert!(
            !repo.is_path_ignored(path).unwrap_or(true),
            "'{path}' should NOT be gitignored"
        );
    }
}

//! Integration tests for `VersionedYamlStore` (E1 W1).
//!
//! Validates ADR-0001 FIN-1 through FIN-4 + basic CRUD.

use mur_common::{
    knowledge::KnowledgeBase,
    pattern::{Content, Pattern},
};
use mur_core::store::versioned::VersionedYamlStore;
use std::time::Instant;
use tempfile::TempDir;

fn temp_store() -> (TempDir, VersionedYamlStore) {
    let dir = TempDir::new().unwrap();
    let store = VersionedYamlStore::init(dir.path()).unwrap();
    (dir, store)
}

fn minimal_pattern(name: &str, content: &str) -> Pattern {
    Pattern {
        base: KnowledgeBase {
            name: name.to_string(),
            content: Content::Plain(content.to_string()),
            ..Default::default()
        },
        kind: None,
        origin: None,
        attachments: vec![],
    }
}

// ── Basic CRUD ────────────────────────────────────────────────────────────────

#[test]
fn save_and_read_roundtrip() {
    let (_dir, mut store) = temp_store();
    let p = minimal_pattern("alpha", "alpha content v1");

    let rev = store.save_pattern(&p, "init").unwrap();
    assert_eq!(rev.version, 1);
    assert_eq!(rev.name, "alpha");
    assert!(!rev.sha.is_empty());

    let loaded = store.read_pattern("alpha").unwrap().unwrap();
    assert_eq!(loaded.name, "alpha");
}

#[test]
fn read_missing_returns_none() {
    let (_dir, store) = temp_store();
    assert!(store.read_pattern("nonexistent").unwrap().is_none());
}

// ── FIN-2: O(1) version derivation ────────────────────────────────────────────

#[test]
fn version_increments_on_each_save() {
    let (_dir, mut store) = temp_store();
    let mut p = minimal_pattern("beta", "v1");

    let r1 = store.save_pattern(&p, "init").unwrap();
    assert_eq!(r1.version, 1);

    p.base.description = "changed".to_string();
    let r2 = store.save_pattern(&p, "update").unwrap();
    assert_eq!(r2.version, 2);

    p.base.description = "changed again".to_string();
    let r3 = store.save_pattern(&p, "refine").unwrap();
    assert_eq!(r3.version, 3);

    assert_eq!(store.current_version("beta"), 3);
}

#[test]
fn no_op_save_does_not_increment_version() {
    let (_dir, mut store) = temp_store();
    let p = minimal_pattern("gamma", "unchanged");

    let r1 = store.save_pattern(&p, "init").unwrap();
    let r2 = store.save_pattern(&p, "same content").unwrap();

    assert_eq!(r1.version, 1);
    assert_eq!(r2.version, 1, "no-op must not bump version");
}

// ── FIN-3: history reads from index (not git log) ─────────────────────────────

#[test]
fn history_populated_after_saves() {
    let (_dir, mut store) = temp_store();
    let mut p = minimal_pattern("delta", "v1");
    store.save_pattern(&p, "init").unwrap();

    p.base.description = "d2".to_string();
    store.save_pattern(&p, "refine").unwrap();

    let hist = store.history("delta").unwrap();
    assert_eq!(hist.len(), 2);
    assert_eq!(hist[0].version, 1);
    assert_eq!(hist[0].reason, "init");
    assert_eq!(hist[1].version, 2);
    assert_eq!(hist[1].reason, "refine");
}

#[test]
fn history_empty_for_unknown_pattern() {
    let (_dir, store) = temp_store();
    assert!(store.history("unknown").unwrap().is_empty());
}

#[test]
fn history_fast_on_many_revisions() {
    let (_dir, mut store) = temp_store();
    let mut p = minimal_pattern("perf-target", "v0");

    for i in 1..=50u32 {
        p.base.description = format!("v{i}");
        store.save_pattern(&p, &format!("rev {i}")).unwrap();
    }

    let t = Instant::now();
    let hist = store.history("perf-target").unwrap();
    let elapsed = t.elapsed();

    assert_eq!(hist.len(), 50);
    assert!(
        elapsed.as_millis() < 100,
        "history() took {}ms, expected < 100ms (FIN-3)",
        elapsed.as_millis()
    );
}

// ── Rollback ─────────────────────────────────────────────────────────────────

#[test]
fn rollback_restores_content_as_new_version() {
    let (_dir, mut store) = temp_store();
    let mut p = minimal_pattern("epsilon", "original");

    store.save_pattern(&p, "init").unwrap(); // v1 = "original"
    p.base.description = "modified".to_string();
    store.save_pattern(&p, "update").unwrap(); // v2 = "modified"

    let rev = store.rollback_pattern("epsilon", 1).unwrap(); // v3 = rollback to v1
    assert_eq!(rev.version, 3);

    let loaded = store.read_pattern("epsilon").unwrap().unwrap();
    // The rolled-back pattern should match v1's description
    assert_ne!(loaded.description, "modified");
}

#[test]
fn rollback_missing_version_errors() {
    let (_dir, mut store) = temp_store();
    let p = minimal_pattern("zeta", "content");
    store.save_pattern(&p, "init").unwrap();

    let err = store.rollback_pattern("zeta", 99).unwrap_err();
    assert!(err.to_string().contains("no archived"));
}

// ── detect_external_change & rebuild_index ────────────────────────────────────

#[test]
fn detect_external_change_false_after_save() {
    let (_dir, mut store) = temp_store();
    let p = minimal_pattern("eta", "c");
    store.save_pattern(&p, "init").unwrap();
    // Index was written by save_pattern — head should match
    assert!(!store.detect_external_change().unwrap());
}

#[test]
fn rebuild_index_restores_history() {
    let (_dir, mut store) = temp_store();
    let mut p = minimal_pattern("theta", "v1");
    store.save_pattern(&p, "init").unwrap();
    p.base.description = "v2".to_string();
    store.save_pattern(&p, "update").unwrap();

    // Wipe the in-memory index and rebuild from git
    store.rebuild_index().unwrap();

    let hist = store.history("theta").unwrap();
    assert_eq!(hist.len(), 2, "rebuild must recover 2 versions");
    assert_eq!(hist[0].reason, "init");
    assert_eq!(hist[1].reason, "update");
}

// ── FIN-4: gitignore correctness ──────────────────────────────────────────────

#[test]
fn gitignore_ignores_runtime_dirs() {
    let (_dir, store) = temp_store();
    let root = store.root();

    let repo = git2::Repository::open(root).unwrap();

    // These dirs must be ignored per knowledge.gitignore (bare patterns, FIN-4)
    for path in [
        "agents/some-agent/profile.yaml",
        "session/active.json",
        "cache/vectors.db",
        "secrets/api.key",
    ] {
        assert!(
            repo.is_path_ignored(path).unwrap_or(false),
            "path '{path}' should be gitignored (FIN-4)"
        );
    }
}

#[test]
fn gitignore_tracks_patterns_and_archive() {
    let (_dir, store) = temp_store();
    let root = store.root();
    let repo = git2::Repository::open(root).unwrap();

    for path in [
        "patterns/my-pattern.yaml",
        "archive/patterns/my-pattern/v1.yaml",
        "workflows/my-workflow.yaml",
    ] {
        assert!(
            !repo.is_path_ignored(path).unwrap_or(true),
            "path '{path}' should NOT be gitignored"
        );
    }
}

// ── Schema-3 version field ────────────────────────────────────────────────────

#[test]
fn schema3_version_written_to_yaml_on_save() {
    let (dir, mut store) = temp_store();
    let p = minimal_pattern("schema3-test", "hello");

    let rev = store.save_pattern(&p, "init").unwrap();
    assert_eq!(rev.version, 1);

    // The on-disk YAML must contain `version: 1`.
    let yaml = std::fs::read_to_string(dir.path().join("patterns/schema3-test.yaml")).unwrap();
    assert!(
        yaml.contains("version: 1"),
        "expected 'version: 1' in:\n{yaml}"
    );
    assert!(
        !yaml.contains("revision:"),
        "revision should not appear until set explicitly"
    );

    // No-op: saving same content again must not bump version even though
    // the in-memory pattern has version=0 (skip_serializing_if default).
    let rev2 = store.save_pattern(&p, "same content").unwrap();
    assert_eq!(rev2.version, 1, "no-op must not bump version");

    // Real change: new content → version=2.
    let p2 = minimal_pattern("schema3-test", "world");
    let rev3 = store.save_pattern(&p2, "changed").unwrap();
    assert_eq!(rev3.version, 2);

    let yaml2 = std::fs::read_to_string(dir.path().join("patterns/schema3-test.yaml")).unwrap();
    assert!(
        yaml2.contains("version: 2"),
        "expected 'version: 2' in:\n{yaml2}"
    );
}

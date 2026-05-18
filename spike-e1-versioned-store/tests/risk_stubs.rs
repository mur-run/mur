//! Day-2 risk validation — one test per risk from
//! plans/2026-05-18-continual-learning-versioned-evolution.md spike plan.
//!
//! R1 stays #[ignore]'d (redundant with 01_smoke; CI runs smoke on 3 OS).
//! R4 + R8 (Day 2 wave 1) are live.
//! R2 / R3 / R5 / R6 / R7 remain as #[ignore]'d stubs.

use spike_e1_versioned_store::SpikeStore;
use std::time::Instant;
use tempfile::tempdir;

/// Risk #1 — git2 on macOS/Linux/Windows
///
/// PASS: 01_smoke + this test go green on all three matrix runners.
/// KILL: Windows-only failure → shell out to `git` instead of git2 in prod.
///
/// This test body is identical to 01_smoke ops 1+2; kept as a semantic
/// marker so CI invocation `cargo test r1_` is unambiguous about intent.
#[test]
#[ignore = "redundant with 01_smoke; semantic marker for risk #1 CI matrix"]
fn r1_git2_three_platforms_smoke() {
    let tmp = tempdir().unwrap();
    let mut store = SpikeStore::init(tmp.path()).unwrap();
    store.save_pattern("p1", "x", "ci-smoke").unwrap();
    let h = store.history("p1").unwrap();
    assert_eq!(h.len(), 1);
}

/// Risk #2 — history() performance on 1000-pattern repo
///
/// PASS: history("p500") < 100ms after 1000 patterns × 3 revisions.
/// KILL: > 1s → cache becomes load-bearing.
#[test]
#[ignore = "risk #2 — slow, opt-in"]
fn r2_history_perf_on_1k_patterns() {
    let tmp = tempdir().unwrap();
    let mut store = SpikeStore::init(tmp.path()).unwrap();

    for i in 0..1000 {
        let name = format!("p{i:04}");
        for v in 0..3 {
            store
                .save_pattern(&name, &format!("v{v}"), &format!("seed {v}"))
                .unwrap();
        }
    }

    let start = Instant::now();
    let h = store.history("p0500").unwrap();
    let elapsed = start.elapsed();
    assert_eq!(h.len(), 3);
    println!("r2: history p0500 took {elapsed:?}");
    assert!(elapsed.as_millis() < 100, "history too slow: {elapsed:?}");
}

/// Risk #3 — concurrent writers race
///
/// PASS: 2 threads × 20 writes on the SAME pattern → final history.len() == 40,
///       no panics, no corrupt index.
/// KILL: any lost commit / index corruption → need explicit file locking.
#[test]
#[ignore = "risk #3 — fill in day 2 wave 2"]
fn r3_concurrent_writers_no_lost_commits() {
    todo!("spawn 2 threads writing pattern 'shared' 20 times each, assert history.len() == 40");
}

/// Risk #4 — external `git reset --hard` recovery
///
/// PASS:
///   1. After external reset HEAD~3, `detect_external_change()` returns true
///   2. `rebuild_index()` makes it false again
///   3. Surviving patterns are readable; reset-out patterns return None
///   4. `save_pattern` works after recovery (no broken HEAD)
///
/// KILL: store enters unrecoverable state, or detect returns false (silent
///       corruption).
///
/// Uses git2 directly to perform the reset (no shell dependency in test).
#[test]
fn r4_external_reset_recovery() {
    let tmp = tempdir().unwrap();
    let mut store = SpikeStore::init(tmp.path()).unwrap();

    // Layer history: init(C0) + 5 saves(C1..C5). HEAD = C5.
    for i in 1..=5 {
        store
            .save_pattern(
                &format!("p{i}"),
                &format!("content-{i}"),
                &format!("save {i}"),
            )
            .unwrap();
    }
    store.rebuild_index().unwrap();
    let pre_reset_head = store.knowledge_head().unwrap();
    assert!(!pre_reset_head.is_empty());
    assert!(!store.detect_external_change().unwrap());

    // External reset --hard HEAD~3 on knowledge repo (drop C3..C5)
    // After reset, HEAD = C2 → p1, p2 survive; p3..p5 gone from working tree.
    {
        let repo = git2::Repository::open(tmp.path()).unwrap();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        let target = head
            .parent(0)
            .unwrap()
            .parent(0)
            .unwrap()
            .parent(0)
            .unwrap();
        repo.reset(target.as_object(), git2::ResetType::Hard, None)
            .unwrap();
    }

    // Re-open (since git operations went around the SpikeStore handle)
    let mut store = SpikeStore::open(tmp.path()).unwrap();

    // 1) drift detected
    assert!(
        store.detect_external_change().unwrap(),
        "should detect HEAD drift after external reset"
    );

    // 2) rebuild clears
    store.rebuild_index().unwrap();
    assert!(!store.detect_external_change().unwrap());

    // 3) surviving vs gone
    assert!(store.read_pattern("p1").unwrap().is_some(), "p1 should survive reset");
    assert!(store.read_pattern("p2").unwrap().is_some(), "p2 should survive reset");
    assert!(
        store.read_pattern("p3").unwrap().is_none(),
        "p3 should be gone (reset HEAD~3 from C5 → C2)"
    );
    assert!(store.read_pattern("p4").unwrap().is_none());
    assert!(store.read_pattern("p5").unwrap().is_none());

    // 4) new save works post-recovery
    let rev = store
        .save_pattern("p6", "post-recovery", "after reset")
        .unwrap();
    assert_eq!(rev.version, 1, "p6 is new, should be v1");
}

/// Risk #5 — telemetry growth: agents/.git stays small under high-freq writes
///
/// Validates the .gitignore-based separation in spec §4.2.1: telemetry
/// jsonl files in `agents/<name>/telemetry/` must NOT be tracked, and
/// 24h of @1Hz appends must leave `agents/.git` essentially untouched.
///
/// PASS: agents/.git size delta < 5MB after 86400 telemetry appends.
/// KILL: > 50MB → telemetry must live OUTSIDE the git tree entirely
///       (spec §4.2.1 dir structure needs rewrite).
#[test]
#[ignore = "risk #5 — slow (writes ~10MB), opt-in"]
fn r5_telemetry_growth_under_gitignore() {
    use std::io::Write;

    let tmp = tempdir().unwrap();
    let mut store = SpikeStore::init(tmp.path()).unwrap();

    // Seed: a real agent so agents/.git has a committed baseline
    store
        .save_agent_profile("agent-a", "name: agent-a\nmodel: m1\n", "seed")
        .unwrap();

    let agents_git = tmp.path().join("agents/.git");
    let initial_size = dir_size(&agents_git);

    // Simulate 24h @ 1Hz telemetry append (86400 lines, ~10MB raw)
    let tele_dir = tmp.path().join("agents/agent-a/telemetry");
    std::fs::create_dir_all(&tele_dir).unwrap();
    let tele_file = tele_dir.join("2026-05-18.jsonl");
    {
        let f = std::fs::File::create(&tele_file).unwrap();
        let mut bw = std::io::BufWriter::with_capacity(64 * 1024, f);
        for i in 0..86_400u32 {
            writeln!(
                bw,
                r#"{{"ts":"2026-05-18T{:02}:{:02}:{:02}Z","event":"tick","seq":{}}}"#,
                (i / 3600) % 24,
                (i / 60) % 60,
                i % 60,
                i
            )
            .unwrap();
        }
    }

    let raw_telemetry_mb = std::fs::metadata(&tele_file).unwrap().len() / 1_000_000;
    println!("r5: telemetry file = {} MB raw", raw_telemetry_mb);

    // gitignore working? telemetry path must NOT appear in any status entry.
    let agents_repo = git2::Repository::open(tmp.path().join("agents")).unwrap();
    let statuses = agents_repo.statuses(None).unwrap();
    let mut leaked = Vec::new();
    for entry in statuses.iter() {
        if let Some(path) = entry.path() {
            if path.contains("telemetry/") {
                leaked.push(path.to_string());
            }
        }
    }
    assert!(
        leaked.is_empty(),
        "telemetry path leaked into git status (gitignore broken): {leaked:?}"
    );

    // Trigger a legit commit AFTER the telemetry burst to exercise the
    // add-all path with the heavy telemetry on disk. This is where a broken
    // gitignore would explode .git size.
    store
        .save_agent_profile("agent-a", "name: agent-a\nmodel: m1\nupdated: true\n", "post-tele")
        .unwrap();

    let final_size = dir_size(&agents_git);
    let delta = final_size.saturating_sub(initial_size);
    let delta_mb = delta as f64 / 1_000_000.0;

    println!(
        "r5: agents/.git initial={}B, final={}B, delta={:.3}MB",
        initial_size, final_size, delta_mb
    );

    // PASS: < 5MB delta
    assert!(
        delta < 5_000_000,
        "agents/.git grew by {} bytes ({:.3}MB) — telemetry suspected to leak through .gitignore",
        delta,
        delta_mb
    );
}

fn dir_size(p: &std::path::Path) -> u64 {
    walkdir::WalkDir::new(p)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file())
        .map(|e| e.metadata().map(|m| m.len()).unwrap_or(0))
        .sum()
}

/// Risk #6 — atomic commit safety under SIGKILL
///
/// PASS: kill subprocess mid-save N times; on restart every pattern is
///       either fully present or fully absent.
/// KILL: torn writes → need write-ahead log.
#[test]
#[ignore = "risk #6 — fill in day 2 wave 2"]
fn r6_no_torn_writes_under_sigkill() {
    todo!("subprocess pattern: spawn child doing save_pattern in loop, kill at random offsets");
}

/// Risk #7 — migration of real ~/.mur to schema=3 + git init
///
/// PASS: cp real ~/.mur → tmp; run migrate(); diff content trees == empty.
/// KILL: any data loss / broken YAML / missing pattern.
#[test]
#[ignore = "risk #7 — fill in day 2 wave 2"]
fn r7_migration_from_real_mur() {
    todo!("cp -r ~/.mur tmp; run hypothetical migrate(); diff content trees");
}

/// Risk #8 — split-brain recovery (one repo nuked)
///
/// PASS:
///   1. After `rm -rf agents/.git`, `SpikeStore::open()` fails (strict)
///   2. Knowledge repo unaffected — openable via raw git2
///   3. `SpikeStore::repair_agents()` succeeds, re-commits existing agent dirs
///   4. `SpikeStore::open()` works again; both layers usable
///   5. Knowledge layer history preserved; agents layer history reset
///      (single recovery commit) — documented expected behaviour
///
/// KILL: knowledge repo also corrupted (cross-repo coupling unsafe), or
///       repair loses agent files, or post-repair store can't save new state.
#[test]
fn r8_split_brain_recovery() {
    let tmp = tempdir().unwrap();
    let mut store = SpikeStore::init(tmp.path()).unwrap();

    // Populate both layers
    store
        .save_pattern("knowledge-a", "content-a", "seed")
        .unwrap();
    store
        .save_pattern("knowledge-b", "content-b", "seed")
        .unwrap();
    store
        .save_agent_profile("agent-x", "name: agent-x\nmodel: m1\n", "seed")
        .unwrap();
    store
        .save_agent_profile("agent-y", "name: agent-y\nmodel: m2\n", "seed")
        .unwrap();

    let pre_knowledge_head = store.knowledge_head().unwrap();
    let pre_agents_head = store.agents_head().unwrap();
    assert!(!pre_knowledge_head.is_empty());
    assert!(!pre_agents_head.is_empty());
    drop(store);

    // ── Disaster: nuke agents/.git entirely ─────────────────────────────
    std::fs::remove_dir_all(tmp.path().join("agents/.git")).unwrap();

    // 1) Strict open fails
    assert!(
        SpikeStore::open(tmp.path()).is_err(),
        "open should fail with missing agents/.git"
    );

    // 2) Knowledge repo independently openable (cross-repo independence)
    assert!(
        git2::Repository::open(tmp.path()).is_ok(),
        "knowledge .git should be untouched by agents disaster"
    );

    // 3) Repair succeeds
    let report = SpikeStore::repair_agents(tmp.path()).unwrap();
    assert!(report.recovered, "should report recovery happened");
    assert_eq!(
        report.agents_recommitted, 2,
        "should detect 2 agent dirs to re-commit"
    );

    // 4) Open now works; both layers usable
    let mut store = SpikeStore::open(tmp.path()).unwrap();
    assert_eq!(
        store.knowledge_head().unwrap(),
        pre_knowledge_head,
        "knowledge HEAD unchanged"
    );
    assert_ne!(
        store.agents_head().unwrap(),
        pre_agents_head,
        "agents HEAD reset to new recovery commit"
    );

    // 5a) Knowledge layer fully intact (content + history)
    assert!(store.read_pattern("knowledge-a").unwrap().is_some());
    assert!(store.read_pattern("knowledge-b").unwrap().is_some());
    assert_eq!(store.history("knowledge-a").unwrap().len(), 1);

    // 5b) Agents layer: content rescued, history reset (expected, not a kill)
    assert_eq!(
        store.read_agent_profile("agent-x").unwrap().as_deref(),
        Some("name: agent-x\nmodel: m1\n")
    );
    assert_eq!(
        store.read_agent_profile("agent-y").unwrap().as_deref(),
        Some("name: agent-y\nmodel: m2\n")
    );

    // 6) Forward writes work after repair
    store
        .save_pattern("knowledge-c", "post-repair", "new")
        .unwrap();
    store
        .save_agent_profile("agent-z", "name: agent-z\n", "post-repair")
        .unwrap();
    assert!(store.read_pattern("knowledge-c").unwrap().is_some());
    assert!(store.read_agent_profile("agent-z").unwrap().is_some());
}


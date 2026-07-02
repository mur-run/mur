//! Consolidation tests: dedup + contradiction + orphan (M5b).

use chrono::{Duration, Utc};
use mur_common::config::Config;
use mur_common::skill::stats::{LifecycleState, SkillStats};
use mur_core::skill_consolidate::{
    ConsolidateMethod, ConsolidateOptions, ConsolidateReport, SkillView, contradiction, dedup,
    orphan,
};
use mur_core::store::embedding::EmbeddingConfig;
use mur_core::store::vector::factory::get_vector_store;
use std::fs;
use tempfile::TempDir;

// Test fixture tuple: (name, description, triggers, requires, usage, success,
// last_used_days_ago, lifecycle_state, pinned) - alias avoids clippy::type_complexity
// on the long-hand tuple-slice type used purely for test-table readability.
type SkillConfigRow<'a> = (
    &'a str,
    &'a str,
    &'a [&'a str],
    &'a [&'a str],
    u64,
    u64,
    i64,
    LifecycleState,
    bool,
);

// Test helper mirrors the fixture tuple's 10 fields 1:1 for readability at call sites;
// splitting it would obscure the table-driven test data below. clippy::too_many_arguments
// allowed for test ergonomics.
#[allow(clippy::too_many_arguments)]
fn make_skill_view(
    name: &str,
    description: &str,
    triggers: Vec<String>,
    requires: Vec<String>,
    usage: u64,
    success: u64,
    last_used_days_ago: i64,
    state: LifecycleState,
    pinned: bool,
    now: chrono::DateTime<Utc>,
) -> SkillView {
    let mut stats = SkillStats::new(name, "1.0.0", "abc123", now);
    stats.lifecycle_state = state;
    stats.usage_count = usage;
    stats.success_count = success;
    if usage > 0 {
        stats.last_used_at = Some(now - Duration::days(last_used_days_ago));
    }
    stats.pinned = pinned;

    SkillView {
        name: name.to_string(),
        description: description.to_string(),
        triggers,
        requires,
        stats,
        embed_text: String::new(),
    }
}

/// Setup minimal store for Jaccard-only consolidation tests.
/// The store is not used by the Jaccard path, but the async API signature requires it.
async fn jaccard_test_store(
    dir: &std::path::Path,
) -> (
    EmbeddingConfig,
    std::sync::Arc<dyn mur_core::store::vector::VectorStore>,
) {
    let mut cfg = Config::default();
    cfg.embedding.dimensions = 1;
    let embed_config = EmbeddingConfig::from_config(&cfg);
    let store = get_vector_store(&cfg, dir).await.unwrap();
    (embed_config, store)
}

#[test]
fn jaccard_dedup_near_identical_skills() {
    let now = Utc::now();
    let skills = vec![
        make_skill_view(
            "research pricing",
            "Research pricing data using web search tools",
            vec!["research prices".to_string(), "check pricing".to_string()],
            vec!["web-search".to_string()],
            50,
            45,
            1,
            LifecycleState::Stable,
            false,
            now,
        ),
        make_skill_view(
            "research pricing v2",
            "Research pricing data using web search tools",
            vec!["research prices".to_string(), "check pricing".to_string()],
            vec!["web-search".to_string()],
            10,
            8,
            5,
            LifecycleState::Draft,
            false,
            now,
        ),
    ];

    let mut report = ConsolidateReport {
        method: ConsolidateMethod::Jaccard,
        duplicates: Vec::new(),
        contradictions: Vec::new(),
        orphans: Vec::new(),
    };
    dedup::scan(&skills, &mut report);

    assert_eq!(
        report.duplicates.len(),
        1,
        "near-identical skills should produce 1 duplicate pair, got {report:?}"
    );
    let pair = &report.duplicates[0];
    assert!(
        pair.similarity > 0.5,
        "jaccard should be >0.5 for overlapping token sets, got {}",
        pair.similarity
    );
    assert_eq!(
        pair.keeper, "research pricing",
        "higher-lifecycle skill should be keeper"
    );
}

#[test]
fn jaccard_dissimilar_skills_no_match() {
    let now = Utc::now();
    let skills = vec![
        make_skill_view(
            "web-search",
            "Search the web using a search engine",
            vec!["web search".to_string()],
            vec![],
            10,
            9,
            1,
            LifecycleState::Stable,
            false,
            now,
        ),
        make_skill_view(
            "git-commit",
            "Create well-formed git commits with conventional commit messages",
            vec!["git commit".to_string(), "commit".to_string()],
            vec![],
            10,
            9,
            1,
            LifecycleState::Stable,
            false,
            now,
        ),
    ];

    let mut report = ConsolidateReport {
        method: ConsolidateMethod::Jaccard,
        duplicates: Vec::new(),
        contradictions: Vec::new(),
        orphans: Vec::new(),
    };
    dedup::scan(&skills, &mut report);

    assert!(
        report.duplicates.is_empty(),
        "dissimilar skills should not match, got {report:?}"
    );
}

#[test]
fn inactive_skills_skipped_by_dedup() {
    let now = Utc::now();
    let skills = vec![
        make_skill_view(
            "a-researcher",
            "Research topics via web search",
            vec!["research".to_string()],
            vec!["web-search".to_string()],
            50,
            45,
            1,
            LifecycleState::Stable,
            false,
            now,
        ),
        make_skill_view(
            "b-researcher",
            "Research topics via web search",
            vec!["research".to_string()],
            vec!["web-search".to_string()],
            10,
            8,
            200,
            LifecycleState::Archived,
            false,
            now,
        ),
    ];

    let mut report = ConsolidateReport {
        method: ConsolidateMethod::Jaccard,
        duplicates: Vec::new(),
        contradictions: Vec::new(),
        orphans: Vec::new(),
    };
    dedup::scan(&skills, &mut report);

    assert!(
        report.duplicates.is_empty(),
        "archived skills should be skipped, got {report:?}"
    );
}

#[test]
fn contradiction_on_shared_exact_trigger() {
    let now = Utc::now();
    let skills = vec![
        make_skill_view(
            "git-push",
            "Push commits to remote",
            vec!["git push".to_string()],
            vec!["git".to_string()],
            10,
            9,
            1,
            LifecycleState::Stable,
            false,
            now,
        ),
        make_skill_view(
            "git-force-push",
            "Force push to remote with safety checks",
            vec!["git push".to_string()],
            vec!["git".to_string()],
            8,
            7,
            2,
            LifecycleState::Stable,
            false,
            now,
        ),
    ];

    let mut report = ConsolidateReport {
        method: ConsolidateMethod::Jaccard,
        duplicates: Vec::new(),
        contradictions: Vec::new(),
        orphans: Vec::new(),
    };
    contradiction::scan(&skills, &mut report);

    assert_eq!(
        report.contradictions.len(),
        1,
        "shared exact trigger should produce contradiction"
    );
    assert_eq!(report.contradictions[0].trigger, "git push");
}

#[test]
fn glob_triggers_skipped_by_contradiction() {
    let now = Utc::now();
    let skills = vec![
        make_skill_view(
            "research-any",
            "Research anything",
            vec!["research-*".to_string()],
            vec![],
            10,
            9,
            1,
            LifecycleState::Stable,
            false,
            now,
        ),
        make_skill_view(
            "research-specific",
            "Research specific topics",
            vec!["research-*".to_string()],
            vec![],
            8,
            7,
            2,
            LifecycleState::Stable,
            false,
            now,
        ),
    ];

    let mut report = ConsolidateReport {
        method: ConsolidateMethod::Jaccard,
        duplicates: Vec::new(),
        contradictions: Vec::new(),
        orphans: Vec::new(),
    };
    contradiction::scan(&skills, &mut report);

    assert!(
        report.contradictions.is_empty(),
        "glob triggers should be skipped, got {report:?}"
    );
}

#[test]
fn orphan_detection() {
    let now = Utc::now();
    let skills = vec![
        make_skill_view(
            "active-skill",
            "Used recently",
            vec!["test".to_string()],
            vec![],
            10,
            9,
            1,
            LifecycleState::Stable,
            false,
            now,
        ),
        make_skill_view(
            "stale-skill",
            "Not used for a long time",
            vec!["stale".to_string()],
            vec![],
            5,
            4,
            200,
            LifecycleState::Stable,
            false,
            now,
        ),
    ];

    let mut report = ConsolidateReport {
        method: ConsolidateMethod::Jaccard,
        duplicates: Vec::new(),
        contradictions: Vec::new(),
        orphans: Vec::new(),
    };
    orphan::scan(&skills, &mut report, now).unwrap();

    assert_eq!(
        report.orphans.len(),
        1,
        "stale skill should be flagged as orphan"
    );
    assert_eq!(report.orphans[0].name, "stale-skill");
    assert_eq!(report.orphans[0].usage_count, 5);
}

#[test]
fn pinned_skills_not_orphans() {
    let now = Utc::now();
    let skills = vec![make_skill_view(
        "pinned-stale",
        "Pinned and stale",
        vec!["pinned".to_string()],
        vec![],
        5,
        4,
        200,
        LifecycleState::Stable,
        true,
        now,
    )];

    let mut report = ConsolidateReport {
        method: ConsolidateMethod::Jaccard,
        duplicates: Vec::new(),
        contradictions: Vec::new(),
        orphans: Vec::new(),
    };
    orphan::scan(&skills, &mut report, now).unwrap();

    assert!(
        report.orphans.is_empty(),
        "pinned skills should not be flagged as orphans"
    );
}

#[test]
fn consolidate_all_passes_integration() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let now = Utc::now();

    // Create skill directories with stats.json files
    let skills_dir = home.join("skills");
    fs::create_dir_all(&skills_dir).unwrap();

    let skill_configs: &[SkillConfigRow] = &[
        (
            "pricing-research",
            "Research pricing data using web search for competitive market analysis",
            &["research prices", "check pricing"],
            &["web-search"],
            50u64,
            45u64,
            1i64,
            LifecycleState::Stable,
            false,
        ),
        (
            "pricing-research-v2",
            "Research pricing data using web search for competitive market analysis",
            &["research prices", "check pricing"],
            &["web-search"],
            10u64,
            8u64,
            5i64,
            LifecycleState::Draft,
            false,
        ),
        (
            "gamma",
            "Git push to remote repository",
            &["git push"],
            &["git"],
            10u64,
            9u64,
            1i64,
            LifecycleState::Stable,
            false,
        ),
        (
            "delta",
            "Git push to remote repository with force",
            &["git push"],
            &["git"],
            8u64,
            7u64,
            2i64,
            LifecycleState::Stable,
            false,
        ),
        (
            "epsilon",
            "Old unused helper tool",
            &["old"],
            &[],
            5u64,
            4u64,
            200i64,
            LifecycleState::Stable,
            false,
        ),
    ];

    for (name, desc, triggers, requires, usage, success, last, state, pinned) in skill_configs {
        let skill_dir = skills_dir.join(name);
        fs::create_dir_all(&skill_dir).unwrap();

        // Write a minimal skill.yaml so load_all_with_stats can parse it
        let yaml = format!(
            "name: {name}\nversion: 1.0.0\npublisher: test\ndescription: {desc}\ncategory: context\ncontent:\n  abstract: \"test\"\n  procedure:\n    steps:\n      - description: \"test step\"\ntriggers:\n{}\nrequires:\n{}\n",
            triggers
                .iter()
                .map(|t| format!("  - type: keyword\n    pattern: \"{t}\""))
                .collect::<Vec<_>>()
                .join("\n"),
            requires
                .iter()
                .map(|r| format!("  - name: {r}\n    version: \"*\""))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        fs::write(skill_dir.join("skill.yaml"), &yaml).unwrap();

        let mut stats = SkillStats::new(name, "1.0.0", "abc123", now);
        stats.lifecycle_state = *state;
        stats.usage_count = *usage;
        stats.success_count = *success;
        stats.last_used_at = Some(now - Duration::days(*last));
        stats.pinned = *pinned;
        let stats_path = SkillStats::path(home, name);
        fs::create_dir_all(stats_path.parent().unwrap()).unwrap();
        fs::write(&stats_path, serde_json::to_string_pretty(&stats).unwrap()).unwrap();
    }

    let store_dir = TempDir::new().unwrap();
    let (embed_config, store) = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(jaccard_test_store(store_dir.path()));

    let report = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(mur_core::skill_consolidate::run_consolidate(
            home,
            &embed_config,
            store.as_ref(),
            &ConsolidateOptions {
                dry_run: false,
                apply: false,
                method: ConsolidateMethod::Jaccard,
                llm_adjudicate: false,
            },
        ))
        .unwrap();

    // Should find at least 1 duplicate (pricing-research ≈ pricing-research-v2)
    assert!(
        !report.duplicates.is_empty(),
        "should detect pricing-research duplicate, got {report:?}"
    );
    // Should find at least 1 contradiction (gamma vs delta on "git push")
    assert!(
        !report.contradictions.is_empty(),
        "should detect gamma-delta contradiction"
    );
    // Should find epsilon as orphan (200d old)
    assert!(
        report.orphans.iter().any(|o| o.name == "epsilon"),
        "should detect epsilon as orphan"
    );
}

#[test]
fn consolidate_apply_archives_orphans() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let now = Utc::now();

    let skills_dir = home.join("skills");
    fs::create_dir_all(&skills_dir).unwrap();

    let name = "old-skill";
    let skill_dir = skills_dir.join(name);
    fs::create_dir_all(&skill_dir).unwrap();

    let yaml = "name: old-skill\nversion: 1.0.0\npublisher: test\ndescription: old\ncategory: context\ncontent:\n  abstract: \"test\"\n  procedure:\n    steps:\n      - description: \"test step\"\n";
    fs::write(skill_dir.join("skill.yaml"), yaml).unwrap();

    let mut stats = SkillStats::new(name, "1.0.0", "abc123", now);
    stats.lifecycle_state = LifecycleState::Stable;
    stats.usage_count = 5;
    stats.success_count = 4;
    stats.last_used_at = Some(now - Duration::days(200));
    let stats_path = SkillStats::path(home, name);
    fs::create_dir_all(stats_path.parent().unwrap()).unwrap();
    fs::write(&stats_path, serde_json::to_string_pretty(&stats).unwrap()).unwrap();

    // Dry run first
    let store_dir = TempDir::new().unwrap();
    let (embed_config, store) = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(jaccard_test_store(store_dir.path()));

    let report_dry = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(mur_core::skill_consolidate::run_consolidate(
            home,
            &embed_config,
            store.as_ref(),
            &ConsolidateOptions {
                dry_run: true,
                apply: false,
                method: ConsolidateMethod::Jaccard,
                llm_adjudicate: false,
            },
        ))
        .unwrap();
    assert!(report_dry.orphans.iter().any(|o| o.name == name));

    // Verify not archived yet (dry_run)
    let after_dry = SkillStats::load(&stats_path).unwrap().unwrap();
    assert_eq!(after_dry.lifecycle_state, LifecycleState::Stable);

    // Apply
    let report_apply = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(mur_core::skill_consolidate::run_consolidate(
            home,
            &embed_config,
            store.as_ref(),
            &ConsolidateOptions {
                dry_run: false,
                apply: true,
                method: ConsolidateMethod::Jaccard,
                llm_adjudicate: false,
            },
        ))
        .unwrap();
    assert!(report_apply.orphans.iter().any(|o| o.name == name));

    // Verify archived
    let after_apply = SkillStats::load(&stats_path).unwrap().unwrap();
    assert_eq!(after_apply.lifecycle_state, LifecycleState::Archived);
}

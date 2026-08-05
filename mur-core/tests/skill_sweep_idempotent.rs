//! Idempotency and anchor-reset tests for `mur skill sweep` (M5b).

use chrono::{DateTime, Duration, Utc};
use mur_common::skill::lifecycle::{calculate_decay, half_life_days};
use mur_common::skill::stats::{LifecycleState, SkillStats};
use mur_core::skill_lifecycle::sweep::{SweepOptions, run_sweep};
use std::fs;
use tempfile::TempDir;

// Positional fixture builder: every argument is a dimension a sweep test
// varies. Bundling them into a struct would only move the noise.
#[allow(clippy::too_many_arguments)]
fn make_stats(
    name: &str,
    state: LifecycleState,
    usage: u64,
    success: u64,
    first_ok_days_ago: i64,
    last_ok_days_ago: i64,
    anchor: f64,
    pinned: bool,
    now: DateTime<Utc>,
) -> SkillStats {
    let mut stats = SkillStats::new(name, "1.0.0", "abc123", now);
    stats.lifecycle_state = state;
    stats.lifecycle_changed_at = now - Duration::hours(48);
    stats.usage_count = usage;
    stats.success_count = success;
    stats.failure_count = usage.saturating_sub(success);
    stats.last_used_at = Some(now - Duration::days(last_ok_days_ago));
    stats.last_success_at = Some(now - Duration::days(last_ok_days_ago));
    stats.first_successful_use_at = Some(now - Duration::days(first_ok_days_ago));
    stats.anchor_confidence = anchor;
    stats.pinned = pinned;
    stats
}

fn write_stats(home: &std::path::Path, stats: &SkillStats) {
    let path = SkillStats::path(home, &stats.skill_name);
    let parent = path.parent().unwrap();
    fs::create_dir_all(parent).unwrap();
    fs::write(&path, serde_json::to_string_pretty(stats).unwrap()).unwrap();
}

#[test]
fn sweep_idempotent() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let now = Utc::now();

    // Create 5 skills in various states
    let skill_names = &["alpha", "beta", "gamma", "delta", "epsilon"];
    let states = &[
        LifecycleState::Draft,    // 3 successes → would promote to Emerging
        LifecycleState::Emerging, // 12 successes, 80% rate, 14d age → would hit Stable
        LifecycleState::Stable, // 40 successes, 90% rate, 40d age → would hit Canonical (if pinned)
        LifecycleState::Stable, // low success rate → would deprecate
        LifecycleState::Archived, // already archived → no change
    ];

    let configs = [
        (3, 3, 5, 1, 1.0, false),      // Draft → Emerging: 3 successes
        (12, 10, 14, 1, 1.0, false),   // Emerging → Stable: 10 succ >= 10, 83% >= 60%, 14d >= 7d
        (40, 35, 40, 1, 1.0, true), // Stable → Canonical: pinned, 35 succ >= 30, 88% >= 80%, 40d >= 30d
        (10, 2, 30, 10, 0.5, false), // Stable → Deprecated: 20% < 30%, usage >= 5
        (0, 0, 365, 365, 0.05, false), // Archived: stays
    ];

    for (i, name) in skill_names.iter().enumerate() {
        let (usage, succ, first, last, anchor, pinned) = configs[i];
        let stats = make_stats(
            name, states[i], usage, succ, first, last, anchor, pinned, now,
        );
        write_stats(home, &stats);
    }

    // First sweep
    let report1 = run_sweep(
        home,
        SweepOptions {
            filter: None,
            dry_run: false,
            now,
            require_human_curation_before_stable: true,
            ..SweepOptions::default()
        },
    )
    .unwrap();

    assert!(
        report1.examined >= 4,
        "expected at least 4 skills examined, got {report1:?}"
    );
    assert!(
        !report1.transitions.is_empty(),
        "first sweep should produce transitions"
    );

    // Second sweep with same now — must be idempotent
    let report2 = run_sweep(
        home,
        SweepOptions {
            filter: None,
            dry_run: false,
            now,
            require_human_curation_before_stable: true,
            ..SweepOptions::default()
        },
    )
    .unwrap();

    assert_eq!(
        report2.transitions.len(),
        0,
        "second sweep with same 'now' must produce zero transitions, got: {report2:?}"
    );

    // Verify persisted states match what the first sweep wrote
    for name in skill_names {
        let path = SkillStats::path(home, name);
        let loaded = SkillStats::load(&path).unwrap().unwrap();
        let proposed = mur_common::skill::lifecycle::next_state(
            &loaded,
            now,
            &mur_common::skill::lifecycle::LifecycleThresholds::default(),
        );
        // After persisting, next_state should agree with the current state
        // (no further transitions needed, since we just applied them)
        if proposed != loaded.lifecycle_state
            && mur_common::skill::lifecycle::transition_allowed(
                loaded.lifecycle_state,
                proposed,
                &loaded,
                now,
            )
        {
            panic!(
                "{name}: persisted {state:?} but next_state wants {proposed:?}",
                state = loaded.lifecycle_state
            );
        }
    }
}

#[test]
fn anchor_reset_on_promotion() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let now = Utc::now();

    // Draft with 1.0 anchor, last success 14d ago → one half-life → decayed = 0.5
    let name = "anchor-test";
    let mut stats = make_stats(name, LifecycleState::Draft, 3, 3, 14, 14, 1.0, false, now);
    // Explicitly set lifecycle_changed_at far enough back to allow transition
    stats.lifecycle_changed_at = now - Duration::hours(48);
    write_stats(home, &stats);

    // Run sweep — Draft → Emerging promotion (3 successes >= 3)
    run_sweep(
        home,
        SweepOptions {
            filter: None,
            dry_run: false,
            now,
            require_human_curation_before_stable: true,
            ..SweepOptions::default()
        },
    )
    .unwrap();

    let loaded = SkillStats::load(&SkillStats::path(home, name))
        .unwrap()
        .unwrap();

    assert_eq!(
        loaded.lifecycle_state,
        LifecycleState::Emerging,
        "should promote to Emerging"
    );

    // Anchor should be the decayed value (~0.5 at one half-life), not 1.0.
    // on_promotion uses the OLD (Draft) half-life of 14 days.
    let expected_decay = calculate_decay(
        1.0,
        Some(now - Duration::days(14)),
        half_life_days(LifecycleState::Draft),
        now,
    );
    assert!(
        loaded.anchor_confidence < 0.99,
        "anchor should be decayed ({expected_decay:.3}), not 1.0, got {}",
        loaded.anchor_confidence
    );
    assert!(
        (loaded.anchor_confidence - expected_decay).abs() < 0.01,
        "anchor confidence {} should equal expected decay {expected_decay}",
        loaded.anchor_confidence
    );
}

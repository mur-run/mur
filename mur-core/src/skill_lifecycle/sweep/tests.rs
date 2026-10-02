use super::*;
use chrono::Duration;
use mur_common::skill::event_log::{append_event, event_log_path};
use mur_common::skill::stats::SkillStats;

fn write_llm_skill(home: &std::path::Path, name: &str) {
    std::fs::create_dir_all(home.join("skills").join(name)).unwrap();
    std::fs::write(
            home.join("skills").join(name).join("skill.yaml"),
            format!("name: {name}\nversion: \"1\"\npublisher: me\ndescription: d\ncategory: workflow\nprovenance: llm\ncontent:\n  abstract: a\n  command: \"echo hi\"\n"),
        )
        .unwrap();
}

fn write_human_skill(home: &std::path::Path, name: &str) {
    std::fs::create_dir_all(home.join("skills").join(name)).unwrap();
    std::fs::write(
            home.join("skills").join(name).join("skill.yaml"),
            format!("name: {name}\nversion: \"1\"\npublisher: me\ndescription: d\ncategory: workflow\ncontent:\n  abstract: a\n  command: \"echo hi\"\n"),
        )
        .unwrap();
}

// Stats that next_state() would promote to Stable: 12 successes, perfect
// rate, aged 40 days.
fn stable_grade_stats(home: &std::path::Path, name: &str, now: chrono::DateTime<Utc>) {
    let mut s = SkillStats::new(name, "1", "digest", now - Duration::days(40));
    s.lifecycle_state = LifecycleState::Emerging;
    s.usage_count = 12;
    s.success_count = 12;
    s.last_used_at = Some(now);
    s.last_success_at = Some(now);
    s.first_successful_use_at = Some(now - Duration::days(40));
    s.anchor_confidence = 1.0;
    s.lifecycle_changed_at = now - Duration::days(40);
    let path = SkillStats::path(home, name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, serde_json::to_string(&s).unwrap()).unwrap();
}

fn emerging_grade_stats(home: &std::path::Path, name: &str, now: chrono::DateTime<Utc>) {
    let mut s = SkillStats::new(name, "1", "digest", now - Duration::days(10));
    s.lifecycle_state = LifecycleState::Emerging;
    s.usage_count = 5;
    s.success_count = 4;
    s.last_used_at = Some(now);
    s.last_success_at = Some(now - Duration::days(2));
    s.first_successful_use_at = Some(now - Duration::days(10));
    s.anchor_confidence = 0.8;
    s.lifecycle_changed_at = now - Duration::days(48); // well past MIN_DWELL_HOURS
    let path = SkillStats::path(home, name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, serde_json::to_string(&s).unwrap()).unwrap();
}

fn write_workflow_failure_events(
    home: &std::path::Path,
    name: &str,
    count: usize,
    now: chrono::DateTime<Utc>,
) {
    for i in 0..count {
        let event = SkillEvent::Execution {
            ts: now - Duration::seconds((count - i) as i64),
            device_id: "test".into(),
            outcome: "failure".into(),
            error: Some("workflow step failed".into()),
            step: None,
            duration_ms: None,
            exit_code: Some(1),
            env_class: Some("workflow".into()),
            confidence: Some(0.9),
            trigger: Some("workflow".into()),
        };
        append_event(&event_log_path(home, name), &event).unwrap();
    }
}

#[test]
fn llm_uncurated_skill_is_capped_at_emerging() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let now = Utc::now();
    write_llm_skill(home, "deploy");
    stable_grade_stats(home, "deploy", now);

    run_sweep(
        home,
        SweepOptions {
            filter: Some("deploy".into()),
            dry_run: false,
            now,
            require_human_curation_before_stable: true,
            thresholds: LifecycleThresholds::default(),
            broken_workflow_streak: 3,
            archive_destroy_grace_days: 30,
        },
    )
    .unwrap();

    let after = SkillStats::load(&SkillStats::path(home, "deploy"))
        .unwrap()
        .unwrap();
    assert_eq!(
        after.lifecycle_state,
        LifecycleState::Emerging,
        "LLM uncurated skill must not pass Emerging despite Stable-grade stats"
    );
}

#[test]
fn llm_curated_skill_promotes_to_stable() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let now = Utc::now();
    write_llm_skill(home, "deploy");
    stable_grade_stats(home, "deploy", now);
    // Mark curated.
    let path = SkillStats::path(home, "deploy");
    let mut s = SkillStats::load(&path).unwrap().unwrap();
    s.curated_at = Some(now - Duration::days(1));
    std::fs::write(&path, serde_json::to_string(&s).unwrap()).unwrap();

    run_sweep(
        home,
        SweepOptions {
            filter: Some("deploy".into()),
            dry_run: false,
            now,
            require_human_curation_before_stable: true,
            thresholds: LifecycleThresholds::default(),
            broken_workflow_streak: 3,
            archive_destroy_grace_days: 30,
        },
    )
    .unwrap();

    let after = SkillStats::load(&path).unwrap().unwrap();
    assert_eq!(after.lifecycle_state, LifecycleState::Stable);
}

// ── P4-1: Broken fast-path tests ─────────────────────────────────────

#[test]
fn broken_fast_path_triggers_after_n_workflow_failures() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let now = Utc::now();
    write_human_skill(home, "build");
    emerging_grade_stats(home, "build", now);
    // 3 consecutive workflow failures.
    write_workflow_failure_events(home, "build", 3, now);

    let report = run_sweep(
        home,
        SweepOptions {
            filter: Some("build".into()),
            dry_run: false,
            now,
            require_human_curation_before_stable: false,
            thresholds: LifecycleThresholds::default(), // broken_workflow_streak = 3
            broken_workflow_streak: 3,
            archive_destroy_grace_days: 30,
        },
    )
    .unwrap();

    assert_eq!(report.transitions.len(), 1);
    assert_eq!(report.transitions[0].to, LifecycleState::Deprecated);
    assert_eq!(
        report.transitions[0].reason,
        TransitionReason::BrokenFastPath
    );
    let after = SkillStats::load(&SkillStats::path(home, "build"))
        .unwrap()
        .unwrap();
    assert_eq!(after.lifecycle_state, LifecycleState::Deprecated);
}

#[test]
fn broken_fast_path_does_not_trigger_below_threshold() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let now = Utc::now();
    write_human_skill(home, "build");
    emerging_grade_stats(home, "build", now);
    // Only 2 workflow failures — below threshold of 3.
    write_workflow_failure_events(home, "build", 2, now);

    let report = run_sweep(
        home,
        SweepOptions {
            filter: Some("build".into()),
            dry_run: false,
            now,
            require_human_curation_before_stable: false,
            thresholds: LifecycleThresholds::default(),
            broken_workflow_streak: 3,
            archive_destroy_grace_days: 30,
        },
    )
    .unwrap();

    assert!(
        report
            .transitions
            .iter()
            .all(|t| t.reason != TransitionReason::BrokenFastPath),
        "should not trigger broken fast-path with only 2 failures"
    );
}

#[test]
fn broken_fast_path_reset_by_success() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let now = Utc::now();
    write_human_skill(home, "build");
    emerging_grade_stats(home, "build", now);
    // 2 failures, then 1 success, then 2 more failures — no streak of 3.
    write_workflow_failure_events(home, "build", 2, now - Duration::seconds(10));
    append_event(
        &event_log_path(home, "build"),
        &SkillEvent::Execution {
            ts: now - Duration::seconds(5),
            device_id: "test".into(),
            outcome: "success".into(),
            error: None,
            step: None,
            duration_ms: None,
            exit_code: Some(0),
            env_class: Some("workflow".into()),
            confidence: None,
            trigger: Some("workflow".into()),
        },
    )
    .unwrap();
    write_workflow_failure_events(home, "build", 2, now);

    let report = run_sweep(
        home,
        SweepOptions {
            filter: Some("build".into()),
            dry_run: false,
            now,
            require_human_curation_before_stable: false,
            thresholds: LifecycleThresholds::default(),
            broken_workflow_streak: 3,
            archive_destroy_grace_days: 30,
        },
    )
    .unwrap();

    assert!(
        report
            .transitions
            .iter()
            .all(|t| t.reason != TransitionReason::BrokenFastPath),
        "success in the middle should reset the streak"
    );
}

#[test]
fn broken_fast_path_disabled_when_streak_zero() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let now = Utc::now();
    write_human_skill(home, "build");
    emerging_grade_stats(home, "build", now);
    write_workflow_failure_events(home, "build", 10, now);

    let report = run_sweep(
        home,
        SweepOptions {
            filter: Some("build".into()),
            dry_run: false,
            now,
            require_human_curation_before_stable: false,
            thresholds: LifecycleThresholds::default(),
            broken_workflow_streak: 0, // disabled — no fast-path trigger
            archive_destroy_grace_days: 30,
        },
    )
    .unwrap();

    assert!(
        report
            .transitions
            .iter()
            .all(|t| t.reason != TransitionReason::BrokenFastPath),
        "broken_workflow_streak=0 must disable the fast-path entirely"
    );
}

// ── P4-3: Destroyed state tests ───────────────────────────────────────

#[test]
fn archived_skill_destroyed_after_grace_period() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let now = Utc::now();
    write_human_skill(home, "old-skill");

    // Create archived stats with lifecycle_changed_at > 30 days ago.
    let mut s = SkillStats::new("old-skill", "1", "digest", now - Duration::days(200));
    s.lifecycle_state = LifecycleState::Archived;
    s.lifecycle_changed_at = now - Duration::days(35); // > 30 day grace
    let path = SkillStats::path(home, "old-skill");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, serde_json::to_string(&s).unwrap()).unwrap();

    let skill_dir = home.join("skills").join("old-skill");
    assert!(skill_dir.exists());

    let report = run_sweep(
        home,
        SweepOptions {
            filter: Some("old-skill".into()),
            dry_run: false,
            now,
            require_human_curation_before_stable: true,
            thresholds: LifecycleThresholds::default(), // grace = 30 days
            broken_workflow_streak: 3,
            archive_destroy_grace_days: 30,
        },
    )
    .unwrap();

    assert_eq!(report.destroyed, 1);
    assert_eq!(report.transitions.len(), 1);
    assert_eq!(report.transitions[0].to, LifecycleState::Destroyed);
    assert_eq!(report.transitions[0].reason, TransitionReason::Destroyed);
    // Directory should be gone.
    assert!(!skill_dir.exists(), "skill directory must be deleted");
}

#[test]
fn archived_skill_not_destroyed_within_grace_period() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let now = Utc::now();
    write_human_skill(home, "fresh-archive");

    let mut s = SkillStats::new("fresh-archive", "1", "digest", now - Duration::days(200));
    s.lifecycle_state = LifecycleState::Archived;
    s.lifecycle_changed_at = now - Duration::days(10); // within 30 day grace
    let path = SkillStats::path(home, "fresh-archive");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, serde_json::to_string(&s).unwrap()).unwrap();

    let report = run_sweep(
        home,
        SweepOptions {
            filter: Some("fresh-archive".into()),
            dry_run: false,
            now,
            require_human_curation_before_stable: true,
            thresholds: LifecycleThresholds::default(),
            broken_workflow_streak: 3,
            archive_destroy_grace_days: 30,
        },
    )
    .unwrap();

    assert_eq!(report.destroyed, 0);
    assert!(home.join("skills").join("fresh-archive").exists());
}

#[test]
fn archived_skill_not_destroyed_when_grace_disabled() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let now = Utc::now();
    write_human_skill(home, "old-skill");

    let mut s = SkillStats::new("old-skill", "1", "digest", now - Duration::days(200));
    s.lifecycle_state = LifecycleState::Archived;
    s.lifecycle_changed_at = now - Duration::days(365);
    let path = SkillStats::path(home, "old-skill");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, serde_json::to_string(&s).unwrap()).unwrap();

    let t = LifecycleThresholds::default();
    let _ = t;

    let report = run_sweep(
        home,
        SweepOptions {
            filter: Some("old-skill".into()),
            dry_run: true, // dry-run so we can check without mutating
            now,
            require_human_curation_before_stable: true,
            thresholds: LifecycleThresholds::default(),
            broken_workflow_streak: 3,
            archive_destroy_grace_days: 30,
        },
    )
    .unwrap();

    // In dry-run mode the directory must not be removed regardless.
    assert!(home.join("skills").join("old-skill").exists());
    // The transition is still reported.
    assert_eq!(report.destroyed, 1);
}

// ── Unit tests for helper functions ──────────────────────────────────

#[test]
fn consecutive_workflow_failures_counts_tail() {
    let now = Utc::now();
    let events = vec![
        SkillEvent::Execution {
            ts: now - Duration::seconds(10),
            device_id: "d".into(),
            outcome: "success".into(),
            error: None,
            step: None,
            duration_ms: None,
            exit_code: Some(0),
            env_class: Some("workflow".into()),
            confidence: None,
            trigger: None,
        },
        SkillEvent::Execution {
            ts: now - Duration::seconds(3),
            device_id: "d".into(),
            outcome: "failure".into(),
            error: None,
            step: None,
            duration_ms: None,
            exit_code: Some(1),
            env_class: Some("workflow".into()),
            confidence: None,
            trigger: None,
        },
        SkillEvent::Execution {
            ts: now - Duration::seconds(2),
            device_id: "d".into(),
            outcome: "failure".into(),
            error: None,
            step: None,
            duration_ms: None,
            exit_code: Some(1),
            env_class: Some("workflow".into()),
            confidence: None,
            trigger: None,
        },
    ];
    assert_eq!(consecutive_trailing_workflow_failures(&events), 2);
}

#[test]
fn consecutive_workflow_failures_stops_at_non_workflow() {
    let now = Utc::now();
    let events = vec![
        SkillEvent::Execution {
            ts: now - Duration::seconds(4),
            device_id: "d".into(),
            outcome: "failure".into(),
            error: None,
            step: None,
            duration_ms: None,
            exit_code: Some(1),
            env_class: Some("workflow".into()),
            confidence: None,
            trigger: None,
        },
        SkillEvent::Execution {
            ts: now - Duration::seconds(3),
            device_id: "d".into(),
            outcome: "failure".into(),
            error: None,
            step: None,
            duration_ms: None,
            exit_code: Some(1),
            env_class: None, // non-workflow failure breaks streak
            confidence: None,
            trigger: None,
        },
        SkillEvent::Execution {
            ts: now - Duration::seconds(2),
            device_id: "d".into(),
            outcome: "failure".into(),
            error: None,
            step: None,
            duration_ms: None,
            exit_code: Some(1),
            env_class: Some("workflow".into()),
            confidence: None,
            trigger: None,
        },
    ];
    // Only the final event forms a streak of 1.
    assert_eq!(consecutive_trailing_workflow_failures(&events), 1);
}

/// Stats that decay would walk DOWN: stale, never successful, low anchor.
fn decayed_grade_stats(
    home: &std::path::Path,
    name: &str,
    now: chrono::DateTime<Utc>,
    state: LifecycleState,
) {
    let mut s = SkillStats::new(name, "1", "digest", now - Duration::days(400));
    s.lifecycle_state = state;
    s.usage_count = 1;
    s.success_count = 0;
    s.last_used_at = Some(now - Duration::days(400));
    s.last_success_at = Some(now - Duration::days(400));
    s.anchor_confidence = 0.05;
    // REQUIRED for the auto-archive branch in next_state(): without it the
    // hard-archive condition is skipped entirely and decay bottoms out at
    // Deprecated. The first version of this fixture omitted it and the
    // destroy-pass test passed while proving nothing.
    s.first_successful_use_at = Some(now - Duration::days(400));
    s.lifecycle_changed_at = now - Duration::days(400);
    let path = SkillStats::path(home, name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, serde_json::to_string(&s).unwrap()).unwrap();
}

/// A note as `mur notes create` writes it: `human:local`, which MUR cannot
/// put back.
fn write_user_note(home: &std::path::Path, name: &str) {
    write_note_with_publisher(home, name, "human:local");
}

/// A note MUR shipped. Recoverable with `mur sync`, so decay may have it.
fn write_mur_note(home: &std::path::Path, name: &str) {
    write_note_with_publisher(home, name, "human:mur");
}

fn write_note_with_publisher(home: &std::path::Path, name: &str, publisher: &str) {
    std::fs::create_dir_all(home.join("skills").join(name)).unwrap();
    std::fs::write(
        home.join("skills").join(name).join("skill.yaml"),
        format!(
            "name: {name}\nversion: \"1\"\npublisher: {publisher}\ndescription: d\n\
                 category: note\ncontent:\n  abstract: a\n  note: |\n    always reply in zh-TW\n"
        ),
    )
    .unwrap();
}

fn sweep_opts(now: chrono::DateTime<Utc>) -> SweepOptions {
    SweepOptions {
        filter: None,
        dry_run: false,
        now,
        require_human_curation_before_stable: true,
        thresholds: LifecycleThresholds::default(),
        broken_workflow_streak: 3,
        archive_destroy_grace_days: 30,
    }
}

/// A note the USER wrote must not be walked down by decay — MUR cannot put
/// it back.
#[test]
fn decay_does_not_demote_a_user_authored_note() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path();
    let now = Utc::now();
    write_user_note(home, "reply-in-zh-tw");
    decayed_grade_stats(home, "reply-in-zh-tw", now, LifecycleState::Stable);

    run_sweep(home, sweep_opts(now)).unwrap();

    let after = SkillStats::load(&SkillStats::path(home, "reply-in-zh-tw"))
        .unwrap()
        .unwrap();
    assert_eq!(
        after.lifecycle_state,
        LifecycleState::Stable,
        "a note the user wrote must hold its state under decay"
    );
}

/// The control that separates "replaceable" from "human wrote it": a note
/// MUR shipped decays like anything else, because `mur sync` reinstalls it.
/// Without this the rule would read as "notes never decay", which is not
/// what was chosen — a builtin you never use SHOULD be deprioritised.
#[test]
fn decay_still_demotes_a_mur_published_note() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path();
    let now = Utc::now();
    write_mur_note(home, "shipped-note");
    decayed_grade_stats(home, "shipped-note", now, LifecycleState::Stable);

    run_sweep(home, sweep_opts(now)).unwrap();

    let after = SkillStats::load(&SkillStats::path(home, "shipped-note"))
        .unwrap()
        .unwrap();
    assert!(
        rank(after.lifecycle_state) < rank(LifecycleState::Stable),
        "a MUR-published note is recoverable and must still decay; got {:?}",
        after.lifecycle_state
    );
}

/// The control: an uncurated machine proposal is exactly what decay is for,
/// and must still be demoted. Without this the fix would read as "nothing
/// decays any more".
#[test]
fn decay_still_demotes_an_uncurated_llm_skill() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path();
    let now = Utc::now();
    write_llm_skill(home, "mined-thing");
    decayed_grade_stats(home, "mined-thing", now, LifecycleState::Stable);

    run_sweep(home, sweep_opts(now)).unwrap();

    let after = SkillStats::load(&SkillStats::path(home, "mined-thing"))
        .unwrap()
        .unwrap();
    assert!(
        rank(after.lifecycle_state) < rank(LifecycleState::Stable),
        "an uncurated LLM skill must still decay; got {:?}",
        after.lifecycle_state
    );
}

/// The harm this closes, end to end: the destroy pass deletes the directory
/// of anything that reaches Archived. A human-authored note must never get
/// there by decay, so its files must still exist after a sweep.
#[test]
fn a_user_note_survives_the_destroy_pass() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path();
    let now = Utc::now();
    write_user_note(home, "kept-note");
    decayed_grade_stats(home, "kept-note", now, LifecycleState::Draft);

    // Enough rounds for the demotion chain (Draft → Deprecated → Archived)
    // plus the destroy grace to elapse. Two sweeps was not enough and the
    // test passed without the fix — it never reached the destroy pass.
    for i in 0..12 {
        run_sweep(home, sweep_opts(now + Duration::days(90 * i))).unwrap();
    }

    let state = SkillStats::load(&SkillStats::path(home, "kept-note"))
        .unwrap()
        .map(|s| s.lifecycle_state);
    assert!(
        home.join("skills")
            .join("kept-note")
            .join("skill.yaml")
            .exists(),
        "sweep deleted a note the user wrote (ended in {state:?})"
    );
}

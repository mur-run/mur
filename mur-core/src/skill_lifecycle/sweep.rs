//! Idempotent lifecycle reconcile pass. Called by `mur skill sweep` and
//! by the idle-trigger handler (Task 5). Per-skill atomic — each
//! `merge_in_place` is its own lock window.

use anyhow::Result;
use chrono::{DateTime, Utc};
use mur_common::skill::event_log::{SkillEvent, event_log_path, read_events};
use mur_common::skill::lifecycle::{
    LifecycleThresholds, calculate_decay, cap_for_provenance, half_life_days, half_life_factor_for,
    next_state, on_promotion, transition_allowed,
};
use mur_common::skill::stats::{LifecycleState, SkillStats};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum TransitionReason {
    Promotion,
    Demotion,
    AutoArchive,
    Deprecation,
    /// N consecutive workflow-env failures triggered the broken fast-path.
    BrokenFastPath,
    /// Archived grace period expired; skill files deleted from disk.
    Destroyed,
}

impl std::fmt::Display for TransitionReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransitionReason::Promotion => write!(f, "promotion"),
            TransitionReason::Demotion => write!(f, "demotion"),
            TransitionReason::AutoArchive => write!(f, "auto_archive"),
            TransitionReason::Deprecation => write!(f, "deprecation"),
            TransitionReason::BrokenFastPath => write!(f, "broken_fast_path"),
            TransitionReason::Destroyed => write!(f, "destroyed"),
        }
    }
}

pub struct SweepOptions {
    pub filter: Option<String>,
    pub dry_run: bool,
    pub now: DateTime<Utc>,
    /// A1 curation gate. When true, LLM-authored uncurated skills are capped
    /// at `Emerging`. Set by the CLI from `config.skills`.
    pub require_human_curation_before_stable: bool,
    /// Lifecycle scoring thresholds. Derived from `config.skill.lifecycle`
    /// at the sweep call-site. Defaults to compile-time constants.
    pub thresholds: LifecycleThresholds,
    /// P4-1: Number of consecutive trailing `Execution` events with
    /// `env_class == "workflow"` that immediately forces a `Deprecated`
    /// transition. 0 = disabled.
    pub broken_workflow_streak: u32,
    /// P4-3: Days a skill must remain in `Archived` state before this sweep
    /// deletes its directory. 0 = disabled.
    pub archive_destroy_grace_days: i64,
}

impl Default for SweepOptions {
    fn default() -> Self {
        Self {
            filter: None,
            dry_run: true,
            now: Utc::now(),
            require_human_curation_before_stable: true,
            thresholds: LifecycleThresholds::default(),
            broken_workflow_streak: 3,
            archive_destroy_grace_days: 30,
        }
    }
}

#[derive(Debug, Default)]
pub struct SweepReport {
    pub examined: usize,
    pub transitions: Vec<Transition>,
    pub decayed: usize,
    pub archived: usize,
    pub destroyed: usize,
}

#[derive(Debug)]
pub struct Transition {
    pub skill_name: String,
    pub from: LifecycleState,
    pub to: LifecycleState,
    pub reason: TransitionReason,
}

fn rank(s: LifecycleState) -> u8 {
    match s {
        LifecycleState::Destroyed => 0,
        LifecycleState::Archived => 1,
        LifecycleState::Deprecated => 2,
        LifecycleState::Draft => 3,
        LifecycleState::Emerging => 4,
        LifecycleState::Stable => 5,
        LifecycleState::Canonical => 6,
    }
}

fn classify_reason(proposed: LifecycleState) -> TransitionReason {
    match proposed {
        LifecycleState::Archived => TransitionReason::AutoArchive,
        LifecycleState::Deprecated => TransitionReason::Deprecation,
        LifecycleState::Destroyed => TransitionReason::Destroyed,
        _ => TransitionReason::Promotion,
    }
}

/// Count the number of trailing consecutive `Execution` events where
/// `env_class == "workflow"` and outcome is not "success".
/// A success event or a non-workflow-failure event resets the streak.
/// Non-Execution events (Retrieval, Dismissed, etc.) are ignored.
fn consecutive_trailing_workflow_failures(events: &[SkillEvent]) -> u32 {
    let mut count = 0u32;
    for event in events.iter().rev() {
        if let SkillEvent::Execution {
            env_class, outcome, ..
        } = event
        {
            if outcome == "success" {
                break; // any success resets the streak
            }
            if env_class.as_deref() == Some("workflow") {
                count += 1;
            } else {
                break; // non-workflow failure resets the streak
            }
        }
    }
    count
}

pub fn run_sweep(home: &Path, opts: SweepOptions) -> Result<SweepReport> {
    let installed =
        mur_common::skill::local::list_installed(home).map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut report = SweepReport::default();

    for name in installed {
        if !matches_filter(&name, opts.filter.as_deref()) {
            continue;
        }
        report.examined += 1;

        let stats_path = SkillStats::path(home, &name);
        let current = match SkillStats::load(&stats_path)? {
            Some(s) => s,
            None => continue, // no stats yet — nothing to sweep
        };

        // ── Destroy pass (P4-3) ───────────────────────────────────────────
        // Archived skills that have exceeded the grace period are hard-deleted.
        // This runs before the normal transition check so a Destroyed skill
        // never goes through the promotion/demotion logic.
        if current.lifecycle_state == LifecycleState::Archived
            && opts.archive_destroy_grace_days > 0
        {
            let grace = chrono::Duration::days(opts.archive_destroy_grace_days);
            if opts.now - current.lifecycle_changed_at > grace {
                report.transitions.push(Transition {
                    skill_name: name.clone(),
                    from: LifecycleState::Archived,
                    to: LifecycleState::Destroyed,
                    reason: TransitionReason::Destroyed,
                });
                report.destroyed += 1;

                if !opts.dry_run {
                    let skill_dir = home.join("skills").join(&name);
                    if skill_dir.exists() {
                        std::fs::remove_dir_all(&skill_dir).map_err(|e| {
                            anyhow::anyhow!("destroy {name}: remove_dir_all failed: {e}")
                        })?;
                    }
                    tracing::info_span!("mur.skill.state_changed",
                        skill = %name,
                        from = ?LifecycleState::Archived,
                        to = ?LifecycleState::Destroyed,
                        reason = "destroyed",
                    )
                    .in_scope(|| tracing::info!("skill directory deleted"));
                }
                continue; // skip normal transition for this skill
            }
        }

        // ── Broken fast-path (P4-1) ───────────────────────────────────────
        // If N consecutive Execution events have env_class == "workflow" and
        // the skill is not already Deprecated/Archived/Destroyed, immediately
        // force it to Deprecated without waiting for the slow scoring path.
        let forced_deprecated = if opts.broken_workflow_streak > 0
            && !matches!(
                current.lifecycle_state,
                LifecycleState::Deprecated | LifecycleState::Archived | LifecycleState::Destroyed
            ) {
            let events = read_events(&event_log_path(home, &name)).unwrap_or_default();
            let streak = consecutive_trailing_workflow_failures(&events);
            streak >= opts.broken_workflow_streak
        } else {
            false
        };

        // Loaded once per skill: kind-aware decay below and the provenance
        // gate both need it. A missing manifest degrades the same way in both
        // places (factor 1.0 / provenance Human).
        let manifest = mur_common::skill::local::load_installed(home, &name).ok();

        // ── Normal scoring path ───────────────────────────────────────────
        let proposed = if forced_deprecated {
            LifecycleState::Deprecated
        } else {
            // Provenance gate (A1): an LLM-authored, uncurated skill cannot rise
            // above Emerging. Reuses the manifest loaded above; a missing
            // manifest defaults to Human (no cap), matching `#[serde(default)]`.
            let provenance = manifest.as_ref().map(|m| m.provenance).unwrap_or_default();
            let curated = current.curated_at.is_some();
            let capped = cap_for_provenance(
                next_state(&current, opts.now, &opts.thresholds),
                provenance,
                curated,
                opts.require_human_curation_before_stable,
            );
            // Decay ends at Archived, Archived ends at `remove_dir_all`, and
            // that is only survivable for content MUR can reinstall. A missing
            // manifest reads as MUR-owned: there is no file with content to
            // lose, and orphaned stats should still be cleanable.
            let publisher = manifest
                .as_ref()
                .map(|m| m.publisher.as_str())
                .unwrap_or("human:mur");
            if rank(capped) < rank(current.lifecycle_state)
                && !mur_common::skill::lifecycle::decay_may_demote(publisher, provenance, curated)
            {
                current.lifecycle_state
            } else {
                capped
            }
        };

        let decayed_value = calculate_decay(
            current.anchor_confidence,
            current.last_success_at,
            // Per-kind curves (federation P1): rule notes halve their
            // half-life, fact notes double it; plain skills are unchanged.
            half_life_days(current.lifecycle_state)
                * half_life_factor_for(manifest.as_ref(), &opts.thresholds),
            opts.now,
        );
        report.decayed += 1;

        let reason = if forced_deprecated {
            TransitionReason::BrokenFastPath
        } else {
            classify_reason(proposed)
        };

        let should_transition = proposed != current.lifecycle_state
            && (forced_deprecated
                || transition_allowed(current.lifecycle_state, proposed, &current, opts.now));

        if should_transition {
            report.transitions.push(Transition {
                skill_name: name.clone(),
                from: current.lifecycle_state,
                to: proposed,
                reason,
            });

            if proposed == LifecycleState::Archived {
                report.archived += 1;
            }

            if !opts.dry_run {
                let decayed = decayed_value;
                SkillStats::merge_in_place(
                    &stats_path,
                    || current.clone(),
                    |s| {
                        let was = s.lifecycle_state;
                        if rank(proposed) > rank(was) {
                            on_promotion(s, opts.now);
                        }
                        s.lifecycle_state = proposed;
                        s.lifecycle_changed_at = opts.now;
                        let _ = decayed;
                        Ok(())
                    },
                )?;

                tracing::info_span!("mur.skill.state_changed",
                    skill = %name,
                    from = ?current.lifecycle_state,
                    to = ?proposed,
                    reason = %reason,
                )
                .in_scope(|| tracing::info!("transition persisted"));
            }
        }
    }

    Ok(report)
}

fn matches_filter(name: &str, filter: Option<&str>) -> bool {
    match filter {
        None => true,
        Some(f) => {
            if f.contains('*') || f.contains('?') {
                let pat = crate::skill_stats::reindex::glob_pattern(f);
                pat.matches(name)
            } else {
                name == f
            }
        }
    }
}

#[cfg(test)]
mod tests {
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
}

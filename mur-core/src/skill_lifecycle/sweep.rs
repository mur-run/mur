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
mod tests;

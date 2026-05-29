//! Idempotent lifecycle reconcile pass. Called by `mur skill sweep` and
//! by the idle-trigger handler (Task 5). Per-skill atomic — each
//! `merge_in_place` is its own lock window.

use anyhow::Result;
use chrono::{DateTime, Utc};
use mur_common::skill::lifecycle::{
    calculate_decay, cap_for_provenance, half_life_days, next_state, on_promotion,
    transition_allowed,
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
}

impl std::fmt::Display for TransitionReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransitionReason::Promotion => write!(f, "promotion"),
            TransitionReason::Demotion => write!(f, "demotion"),
            TransitionReason::AutoArchive => write!(f, "auto_archive"),
            TransitionReason::Deprecation => write!(f, "deprecation"),
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
}

impl Default for SweepOptions {
    fn default() -> Self {
        Self {
            filter: None,
            dry_run: true,
            now: Utc::now(),
            require_human_curation_before_stable: true,
        }
    }
}

#[derive(Debug, Default)]
pub struct SweepReport {
    pub examined: usize,
    pub transitions: Vec<Transition>,
    pub decayed: usize,
    pub archived: usize,
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
        LifecycleState::Archived => 0,
        LifecycleState::Deprecated => 1,
        LifecycleState::Draft => 2,
        LifecycleState::Emerging => 3,
        LifecycleState::Stable => 4,
        LifecycleState::Canonical => 5,
    }
}

fn classify_reason(
    _current: &SkillStats,
    proposed: LifecycleState,
    _now: DateTime<Utc>,
) -> TransitionReason {
    match proposed {
        LifecycleState::Archived => TransitionReason::AutoArchive,
        LifecycleState::Deprecated => TransitionReason::Deprecation,
        _ => TransitionReason::Promotion,
    }
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

        // Provenance gate (A1): an LLM-authored, uncurated skill cannot rise
        // above Emerging. Load the manifest for its provenance; a missing
        // manifest defaults to Human (no cap), matching `#[serde(default)]`.
        let provenance = mur_common::skill::local::load_installed(home, &name)
            .map(|m| m.provenance)
            .unwrap_or_default();
        let proposed = cap_for_provenance(
            next_state(&current, opts.now),
            provenance,
            current.curated_at.is_some(),
            opts.require_human_curation_before_stable,
        );
        let decayed_value = calculate_decay(
            current.anchor_confidence,
            current.last_success_at,
            half_life_days(current.lifecycle_state),
            opts.now,
        );
        report.decayed += 1;

        if proposed != current.lifecycle_state
            && transition_allowed(current.lifecycle_state, proposed, &current, opts.now)
        {
            let reason = classify_reason(&current, proposed, opts.now);

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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use mur_common::skill::stats::SkillStats;

    fn write_llm_skill(home: &std::path::Path, name: &str) {
        std::fs::create_dir_all(home.join("skills").join(name)).unwrap();
        std::fs::write(
            home.join("skills").join(name).join("skill.yaml"),
            format!("name: {name}\nversion: \"1\"\npublisher: me\ndescription: d\ncategory: workflow\nprovenance: llm\ncontent:\n  abstract: a\n  command: \"echo hi\"\n"),
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
            },
        )
        .unwrap();

        let after = SkillStats::load(&path).unwrap().unwrap();
        assert_eq!(after.lifecycle_state, LifecycleState::Stable);
    }
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

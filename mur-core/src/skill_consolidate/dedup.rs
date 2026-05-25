//! Jaccard-based deduplication pass (M5b).

use std::collections::HashSet;

use crate::skill_consolidate::{ConsolidateReport, SkillView};

const JACCARD_THRESHOLD: f64 = 0.85;

#[derive(Debug, Clone, serde::Serialize)]
pub struct DuplicatePair {
    pub a: String,
    pub b: String,
    pub similarity: f64,
    pub keeper: String,
    pub kept_reason: KeeperReason,
}

#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KeeperReason {
    HigherLifecycle,
    HigherSuccessCount,
    Alphabetical,
}

pub fn scan(skills: &[SkillView], report: &mut ConsolidateReport) {
    for i in 0..skills.len() {
        for j in (i + 1)..skills.len() {
            let a = &skills[i];
            let b = &skills[j];

            // Skip already-Archived or Deprecated skills
            if is_inactive(&a.stats) || is_inactive(&b.stats) {
                continue;
            }

            let sim = jaccard(&tokens(a), &tokens(b));
            if sim >= JACCARD_THRESHOLD {
                let (keeper, loser, reason) = select_keeper(a, b);
                report.duplicates.push(DuplicatePair {
                    a: a.name.clone(),
                    b: b.name.clone(),
                    similarity: sim,
                    keeper,
                    kept_reason: reason,
                });
                // Silence unused variable
                let _ = loser;
            }
        }
    }
}

fn tokens(view: &SkillView) -> HashSet<String> {
    let mut set = HashSet::new();
    for word in view
        .name
        .split_whitespace()
        .chain(view.description.split_whitespace())
    {
        set.insert(word.to_lowercase());
    }
    for t in &view.triggers {
        for word in t.split_whitespace() {
            set.insert(word.to_lowercase());
        }
    }
    for r in &view.requires {
        set.insert(r.to_lowercase());
    }
    set
}

fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let inter = a.intersection(b).count() as f64;
    let union = a.union(b).count() as f64;
    if union == 0.0 {
        return 1.0;
    }
    inter / union
}

fn is_inactive(stats: &mur_common::skill::stats::SkillStats) -> bool {
    use mur_common::skill::stats::LifecycleState;
    matches!(
        stats.lifecycle_state,
        LifecycleState::Archived | LifecycleState::Deprecated
    )
}

fn select_keeper(a: &SkillView, b: &SkillView) -> (String, String, KeeperReason) {
    use mur_common::skill::stats::LifecycleState;

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

    let ra = rank(a.stats.lifecycle_state);
    let rb = rank(b.stats.lifecycle_state);
    if ra > rb {
        return (
            a.name.clone(),
            b.name.clone(),
            KeeperReason::HigherLifecycle,
        );
    }
    if rb > ra {
        return (
            b.name.clone(),
            a.name.clone(),
            KeeperReason::HigherLifecycle,
        );
    }

    if a.stats.success_count > b.stats.success_count {
        return (
            a.name.clone(),
            b.name.clone(),
            KeeperReason::HigherSuccessCount,
        );
    }
    if b.stats.success_count > a.stats.success_count {
        return (
            b.name.clone(),
            a.name.clone(),
            KeeperReason::HigherSuccessCount,
        );
    }

    // Alphabetical tiebreaker
    if a.name < b.name {
        (a.name.clone(), b.name.clone(), KeeperReason::Alphabetical)
    } else {
        (b.name.clone(), a.name.clone(), KeeperReason::Alphabetical)
    }
}

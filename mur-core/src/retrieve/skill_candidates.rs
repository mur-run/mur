//! Loaded skills (manifest + stats) and their `Retrievable` impl, so the
//! generic scorer in `super::scoring` can rank skills.

use std::path::Path;

use anyhow::Result;
use chrono::{DateTime, Utc};
use mur_common::pattern::Tier;
use mur_common::skill::manifest::SkillManifest;
use mur_common::skill::stats::{LifecycleState, SkillStats};
use mur_common::skill::types::Priority;

use super::scoring::{Retrievable, ScopeContext};

/// A skill loaded together with its runtime stats. The retrieval pipeline
/// scores `Vec<LoadedSkill>` through the generic `score_and_rank_inner`.
#[derive(Debug, Clone)]
pub struct LoadedSkill {
    pub manifest: SkillManifest,
    pub stats: SkillStats,
}

fn priority_to_tier(p: Priority) -> Tier {
    match p {
        Priority::Critical => Tier::Core,
        Priority::High | Priority::Normal => Tier::Project,
        Priority::Low => Tier::Session,
    }
}

fn priority_to_importance(p: Priority) -> f64 {
    match p {
        Priority::Critical => 1.0,
        Priority::High => 0.8,
        Priority::Normal => 0.5,
        Priority::Low => 0.3,
    }
}

impl Retrievable for LoadedSkill {
    fn name(&self) -> &str {
        &self.manifest.name
    }

    fn description(&self) -> &str {
        &self.manifest.description
    }

    fn text(&self) -> std::borrow::Cow<'_, str> {
        // Pre-Note skills: abstract + description is the keyword/embed surface.
        // Extended once ContentMode::Note (separate plan) lands.
        std::borrow::Cow::Owned(format!(
            "{}\n{}",
            self.manifest.content.r#abstract, self.manifest.description
        ))
    }

    fn tag_terms(&self) -> Vec<&str> {
        self.manifest.tags.iter().map(String::as_str).collect()
    }

    fn importance(&self) -> f64 {
        priority_to_importance(self.manifest.priority)
    }

    fn effectiveness(&self) -> f64 {
        if self.stats.usage_count == 0 {
            0.0
        } else {
            self.stats.success_count as f64 / self.stats.usage_count as f64
        }
    }

    fn tier(&self) -> Tier {
        priority_to_tier(self.manifest.priority)
    }

    fn created_at(&self) -> DateTime<Utc> {
        // Manifest has no created_at; use the earliest stats anchor we have.
        self.stats
            .first_successful_use_at
            .unwrap_or(self.stats.lifecycle_changed_at)
    }

    fn last_activity(&self) -> Option<DateTime<Utc>> {
        self.stats.last_success_at
    }

    fn decay_half_life_days(&self) -> f64 {
        self.tier().decay_half_life_days() as f64
    }

    fn is_active(&self) -> bool {
        !matches!(
            self.stats.lifecycle_state,
            LifecycleState::Deprecated | LifecycleState::Archived
        )
    }

    // adjust_score uses the trait default (identity) — skills have no
    // Pattern-specific scope/lang/kind boosts.
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use mur_common::skill::manifest::Content;
    use mur_common::skill::types::Category;

    fn fake_loaded(name: &str, priority: Priority) -> LoadedSkill {
        let manifest = SkillManifest {
            name: name.into(),
            version: "1.0.0".into(),
            publisher: "human:test".into(),
            description: format!("desc for {name}"),
            category: Category::Context,
            hosts: vec![],
            content: Content {
                r#abstract: format!("abstract about {name}"),
                context: Some(format!("body of {name}")),
                procedure: None,
                command: None,
            },
            requires: vec![],
            tags: vec!["alpha".into(), "beta".into()],
            triggers: vec![],
            priority,
            evolution_log: vec![],
            transfer_chain: vec![],
            mcp_requirements: vec![],
        };
        let mut stats = SkillStats::new(name, "1.0.0", "", Utc::now() - Duration::days(2));
        stats.usage_count = 4;
        stats.success_count = 3;
        stats.last_success_at = Some(Utc::now() - Duration::days(1));
        stats.first_successful_use_at = Some(Utc::now() - Duration::days(2));
        LoadedSkill { manifest, stats }
    }

    #[test]
    fn retrievable_accessors_reflect_manifest_and_stats() {
        let s = fake_loaded("alpha-skill", Priority::High);
        assert_eq!(s.name(), "alpha-skill");
        assert_eq!(s.description(), "desc for alpha-skill");
        assert_eq!(&*s.text(), "abstract about alpha-skill\ndesc for alpha-skill");
        assert_eq!(s.tag_terms(), vec!["alpha", "beta"]);
        assert_eq!(s.importance(), 0.8);
        assert_eq!(s.tier(), Tier::Project);
        assert!((s.effectiveness() - 0.75).abs() < 1e-9);
        assert!(s.is_active());
        assert_eq!(s.decay_half_life_days(), Tier::Project.decay_half_life_days() as f64);
        assert_eq!(s.last_activity(), s.stats.last_success_at);
    }

    #[test]
    fn priority_critical_maps_to_core_tier_and_importance_one() {
        let s = fake_loaded("k", Priority::Critical);
        assert_eq!(s.tier(), Tier::Core);
        assert_eq!(s.importance(), 1.0);
    }

    #[test]
    fn priority_low_maps_to_session_tier_and_importance_zero_three() {
        let s = fake_loaded("k", Priority::Low);
        assert_eq!(s.tier(), Tier::Session);
        assert!((s.importance() - 0.3).abs() < 1e-9);
    }

    #[test]
    fn effectiveness_is_zero_when_usage_count_is_zero() {
        let mut s = fake_loaded("k", Priority::Normal);
        s.stats.usage_count = 0;
        s.stats.success_count = 0;
        assert_eq!(s.effectiveness(), 0.0);
    }

    #[test]
    fn deprecated_skill_is_not_active() {
        let mut s = fake_loaded("k", Priority::Normal);
        s.stats.lifecycle_state = LifecycleState::Deprecated;
        assert!(!s.is_active());
    }

    #[test]
    fn archived_skill_is_not_active() {
        let mut s = fake_loaded("k", Priority::Normal);
        s.stats.lifecycle_state = LifecycleState::Archived;
        assert!(!s.is_active());
    }

    #[test]
    fn adjust_score_is_identity_for_skills() {
        let s = fake_loaded("k", Priority::Normal);
        let scope = ScopeContext::default();
        assert_eq!(s.adjust_score(0.42, &["q"], Some(&scope), Some("rust")), 0.42);
    }
}

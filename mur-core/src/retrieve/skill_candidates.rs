//! Loaded skills (manifest + stats) and their `Retrievable` impl, so the
//! generic scorer in `super::scoring` can rank skills.

use std::path::Path;

use anyhow::Result;
use chrono::{DateTime, Utc};
use mur_common::pattern::Tier;
use mur_common::skill::manifest::SkillManifest;
use mur_common::skill::stats::{LifecycleState, SkillStats};
use mur_common::skill::types::Priority;

use super::scoring::Retrievable;
use crate::inject::hook::{InjectedItem, KindGroup};

/// A skill loaded together with its runtime stats. The retrieval pipeline
/// scores `Vec<LoadedSkill>` through the generic `score_and_rank_inner`.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct LoadedSkill {
    pub manifest: SkillManifest,
    pub stats: SkillStats,
}

impl LoadedSkill {
    /// Convert this skill into an `InjectedItem` for the injection formatting pipeline.
    pub fn to_injected_item(&self) -> InjectedItem {
        let content_text = self
            .manifest
            .content
            .context
            .as_deref()
            .or(self.manifest.content.note.as_deref())
            .or(self.manifest.content.command.as_deref())
            .unwrap_or(&self.manifest.content.r#abstract)
            .to_string();
        let kind_group = match self.manifest.category {
            mur_common::skill::types::Category::Context
            | mur_common::skill::types::Category::Meta
            | mur_common::skill::types::Category::Note => KindGroup::Knowledge,
            mur_common::skill::types::Category::Workflow => KindGroup::Procedures,
            mur_common::skill::types::Category::Command => KindGroup::Procedures,
            // `KindGroup` is a *formatting* taxonomy (it only selects the injected
            // section header + entry formatter), not a domain taxonomy. A media
            // skill injects as procedural guidance ("to analyze a video, use
            // video_analyze when…"), so it belongs under Procedures. We deliberately
            // do NOT add a `KindGroup::Media`: that would push a domain label into a
            // formatting enum (the same altitude mistake as a media-specific Category).
            mur_common::skill::types::Category::Media => KindGroup::Procedures,
        };
        InjectedItem {
            name: self.manifest.name.clone(),
            description: self.manifest.description.clone(),
            content_text,
            kind_group,
        }
    }
}

/// Active scope context for injection filtering. Set by the runtime (a fleet
/// member's turn / project tooling) via env; absent → that scope's skills are
/// NOT injected (fail-closed). Until the runtime sets these, only `User`/
/// `Enterprise` skills inject — making `scope_visible` live without leaking
/// fleet/project-tagged skills into unrelated sessions.
#[derive(Debug, Default, Clone)]
pub struct ActiveScope {
    pub fleet: Option<String>,
    pub project: Option<String>,
    pub team: Option<String>,
}

impl ActiveScope {
    /// Detect the active scope. `MUR_ACTIVE_FLEET` / `MUR_ACTIVE_PROJECT` /
    /// `MUR_ACTIVE_TEAM` env override; otherwise `project` defaults to the
    /// current working dir's git repo root (so a `scope: Project` skill learned
    /// in repo X injects anywhere in X, and nowhere else). `fleet` and `team`
    /// have no cwd-derivable default — they stay env-only until the fleet
    /// runtime supplies them.
    pub fn detect() -> Self {
        let nonempty = |k: &str| std::env::var(k).ok().filter(|s| !s.trim().is_empty());
        Self {
            fleet: nonempty("MUR_ACTIVE_FLEET"),
            // Shared detection (env override → cwd repo root) with the runtime injector.
            project: mur_common::project::active_project_id(),
            team: nonempty("MUR_ACTIVE_TEAM"),
        }
    }
}

/// Drop candidate skills not visible in the active scope. `User`/`Enterprise`
/// always pass; `Fleet`/`Project`/`Team` pass only when their selector matches
/// `ctx`, so an unmatched skill is excluded, never leaked (fail-closed).
pub fn filter_by_scope(candidates: &mut Vec<LoadedSkill>, ctx: &ActiveScope) {
    candidates.retain(|c| {
        mur_common::skill::manifest::scope_visible(
            c.manifest.scope,
            c.manifest.fleet.as_deref(),
            c.manifest.project.as_deref(),
            c.manifest.team.as_deref(),
            ctx.fleet.as_deref(),
            ctx.project.as_deref(),
            ctx.team.as_deref(),
        )
    });
}

/// Scan `skills_dir` (typically `<mur_home>/skills/`) for skill directories
/// and return a `LoadedSkill` for each parseable `skill.yaml`.
///
/// Malformed or missing manifests are skipped with a `tracing::warn` so a
/// single bad skill never poisons the corpus. Stats are loaded via
/// `SkillStats::path(mur_home, name)`; if absent, a fresh `SkillStats` is
/// constructed so the skill still scores (with `usage_count = 0`).
#[allow(dead_code)]
pub fn load_skill_candidates(skills_dir: &Path, mur_home: &Path) -> Result<Vec<LoadedSkill>> {
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(skills_dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e.into()),
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let yaml_path = path.join("skill.yaml");
        if !yaml_path.is_file() {
            continue;
        }
        let yaml = match std::fs::read_to_string(&yaml_path) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(path = %yaml_path.display(), error = %e, "read skill.yaml failed");
                continue;
            }
        };
        let manifest = match mur_common::skill::parser::parse_canonical(&yaml) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(path = %yaml_path.display(), error = %e, "parse skill.yaml failed");
                continue;
            }
        };

        let stats_path = SkillStats::path(mur_home, &manifest.name);
        let stats = match SkillStats::load(&stats_path) {
            Ok(Some(s)) => s,
            Ok(None) => SkillStats::new(&manifest.name, &manifest.version, "", Utc::now()),
            Err(e) => {
                tracing::warn!(path = %stats_path.display(), error = %e, "load skill stats failed; using fresh");
                SkillStats::new(&manifest.name, &manifest.version, "", Utc::now())
            }
        };

        out.push(LoadedSkill { manifest, stats });
    }

    Ok(out)
}

#[allow(dead_code)]
fn priority_to_tier(p: Priority) -> Tier {
    match p {
        Priority::Critical => Tier::Core,
        Priority::High | Priority::Normal => Tier::Project,
        Priority::Low => Tier::Session,
    }
}

#[allow(dead_code)]
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
        // abstract + description is the base keyword/embed surface; note-mode
        // skills append their markdown body so they rank on their content.
        let mut s = format!(
            "{}\n{}",
            self.manifest.content.r#abstract, self.manifest.description
        );
        if let Some(note) = &self.manifest.content.note {
            s.push('\n');
            s.push_str(note);
        }
        std::borrow::Cow::Owned(s)
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
        // Per-kind curves (federation P1): rule notes decay fast, fact notes
        // linger. Uses the compile-time default factors — the config override
        // applies to the lifecycle sweep; retrieval uses the same defaults so
        // the two decay systems agree unless deliberately tuned apart.
        let factor = mur_common::skill::lifecycle::note_kind(&self.manifest)
            .map(|k| k.default_half_life_factor())
            .unwrap_or(1.0);
        self.tier().decay_half_life_days() as f64 * factor
    }

    fn is_note(&self) -> bool {
        self.manifest.category == mur_common::skill::Category::Note
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
    use crate::retrieve::scoring::ScoringHints;
    use chrono::Duration;
    use mur_common::skill::manifest::{Content, Visibility};
    use mur_common::skill::types::Category;

    fn fake_loaded(name: &str, priority: Priority) -> LoadedSkill {
        let manifest = SkillManifest {
            name: name.into(),
            version: "1.0.0".into(),
            publisher: "human:test".into(),
            description: format!("desc for {name}"),
            category: Category::Context,
            hosts: vec![],
            scope: Default::default(),
            visibility: Visibility::default(),
            origin: None,
            origin_version: None,
            origin_hash: None,
            fleet: None,
            project: None,
            team: None,
            governance: None,
            content: Content {
                r#abstract: format!("abstract about {name}"),
                context: Some(format!("body of {name}")),
                procedure: None,
                command: None,
                note: None,
            },
            requires: vec![],
            tags: vec!["alpha".into(), "beta".into()],
            triggers: vec![],
            priority,
            evolution_log: vec![],
            transfer_chain: vec![],
            mcp_requirements: vec![],
            provenance: Default::default(),
            updated_at: Utc::now(),
            requires_programs: vec![],
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
        assert_eq!(
            &*s.text(),
            "abstract about alpha-skill\ndesc for alpha-skill"
        );
        assert_eq!(s.tag_terms(), vec!["alpha", "beta"]);
        assert_eq!(s.importance(), 0.8);
        assert_eq!(s.tier(), Tier::Project);
        assert!((s.effectiveness() - 0.75).abs() < 1e-9);
        assert!(s.is_active());
        assert_eq!(
            s.decay_half_life_days(),
            Tier::Project.decay_half_life_days() as f64
        );
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
        let scope = ScoringHints::default();
        assert_eq!(
            s.adjust_score(0.42, &["q"], Some(&scope), Some("rust")),
            0.42
        );
    }

    #[test]
    fn load_skill_candidates_reads_two_well_formed_skills() {
        use std::fs;
        use tempfile::tempdir;

        let tmp = tempdir().unwrap();
        let skills_dir = tmp.path().join("skills");
        let mur_home = tmp.path();

        // Write two well-formed skill directories.
        for name in ["alpha", "beta"] {
            let dir = skills_dir.join(name);
            fs::create_dir_all(&dir).unwrap();
            let yaml = format!(
                "name: {name}\nversion: 1.0.0\npublisher: human:test\n\
                 description: desc for {name}\ncategory: context\n\
                 content:\n  abstract: a\n  context: c\n"
            );
            fs::write(dir.join("skill.yaml"), yaml).unwrap();
        }

        let loaded = load_skill_candidates(&skills_dir, mur_home).unwrap();
        let names: Vec<_> = loaded.iter().map(|s| s.name().to_string()).collect();
        assert_eq!(names.len(), 2);
        assert!(names.contains(&"alpha".to_string()));
        assert!(names.contains(&"beta".to_string()));
    }

    #[test]
    fn filter_by_scope_fail_closed() {
        use std::fs;
        use tempfile::tempdir;
        let tmp = tempdir().unwrap();
        let skills_dir = tmp.path().join("skills");
        let mur_home = tmp.path();
        let write = |name: &str, extra: &str| {
            let dir = skills_dir.join(name);
            fs::create_dir_all(&dir).unwrap();
            let yaml = format!(
                "name: {name}\nversion: 1.0.0\npublisher: human:test\n\
                 description: d\ncategory: context\n{extra}\
                 content:\n  abstract: a\n  context: c\n"
            );
            fs::write(dir.join("skill.yaml"), yaml).unwrap();
        };
        write("u", ""); // scope defaults to user
        write("f", "scope: fleet\nfleet: devteam\n");
        write("p", "scope: project\nproject: /repo\n");

        let names = |ctx: &ActiveScope| {
            let mut v = load_skill_candidates(&skills_dir, mur_home).unwrap();
            filter_by_scope(&mut v, ctx);
            let mut n: Vec<String> = v.iter().map(|s| s.name().to_string()).collect();
            n.sort();
            n
        };
        // no context → only user skill (fleet/project fail-closed)
        assert_eq!(names(&ActiveScope::default()), vec!["u".to_string()]);
        // matching fleet context → user + that fleet skill
        let ctx = ActiveScope {
            fleet: Some("devteam".into()),
            project: None,
            team: None,
        };
        assert_eq!(names(&ctx), vec!["f".to_string(), "u".to_string()]);
        // matching project context
        let ctx = ActiveScope {
            fleet: None,
            project: Some("/repo".into()),
            team: None,
        };
        assert_eq!(names(&ctx), vec!["p".to_string(), "u".to_string()]);
        // wrong fleet → fail-closed (only user)
        let ctx = ActiveScope {
            fleet: Some("other".into()),
            project: None,
            team: None,
        };
        assert_eq!(names(&ctx), vec!["u".to_string()]);
    }

    #[test]
    fn load_skill_candidates_skips_directories_without_skill_yaml() {
        use std::fs;
        use tempfile::tempdir;

        let tmp = tempdir().unwrap();
        let skills_dir = tmp.path().join("skills");
        fs::create_dir_all(skills_dir.join("not-a-skill")).unwrap();

        let loaded = load_skill_candidates(&skills_dir, tmp.path()).unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn load_skill_candidates_skips_malformed_yaml_with_warning() {
        use std::fs;
        use tempfile::tempdir;

        let tmp = tempdir().unwrap();
        let skills_dir = tmp.path().join("skills");
        fs::create_dir_all(skills_dir.join("broken")).unwrap();
        fs::write(
            skills_dir.join("broken").join("skill.yaml"),
            "{ not valid yaml",
        )
        .unwrap();

        // Loader must not propagate the parse error; return Ok(empty).
        let loaded = load_skill_candidates(&skills_dir, tmp.path()).unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn load_skill_candidates_returns_empty_if_skills_dir_missing() {
        use tempfile::tempdir;
        let tmp = tempdir().unwrap();
        let skills_dir = tmp.path().join("does-not-exist");
        let loaded = load_skill_candidates(&skills_dir, tmp.path()).unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn end_to_end_ranks_loaded_skills_via_generic_scorer() {
        use crate::retrieve::scoring::score_and_rank_generic;
        use std::fs;
        use tempfile::tempdir;

        let tmp = tempdir().unwrap();
        let skills_dir = tmp.path().join("skills");
        let mur_home = tmp.path();

        // alpha: matches query "deploy" in abstract and description.
        fs::create_dir_all(skills_dir.join("deploy-fly")).unwrap();
        fs::write(
            skills_dir.join("deploy-fly").join("skill.yaml"),
            "name: deploy-fly\nversion: 1.0.0\npublisher: human:test\n\
             description: deploy to Fly.io\ncategory: context\n\
             priority: high\ntags: [deploy, fly]\n\
             content:\n  abstract: how to deploy a Rust app to Fly.io\n  context: details\n",
        )
        .unwrap();

        // beta: unrelated keyword content.
        fs::create_dir_all(skills_dir.join("brew-update")).unwrap();
        fs::write(
            skills_dir.join("brew-update").join("skill.yaml"),
            "name: brew-update\nversion: 1.0.0\npublisher: human:test\n\
             description: keep homebrew current\ncategory: context\n\
             priority: normal\ntags: [brew, mac]\n\
             content:\n  abstract: run brew update weekly\n  context: details\n",
        )
        .unwrap();

        let candidates = load_skill_candidates(&skills_dir, mur_home).unwrap();
        assert_eq!(candidates.len(), 2);

        let ranked = score_and_rank_generic("deploy fly rust", candidates);
        assert!(
            !ranked.is_empty(),
            "deploy query should rank at least the deploy-fly skill"
        );
        assert_eq!(ranked[0].item.name(), "deploy-fly");
        // If brew-update made it past the score floor, it must rank below deploy-fly.
        if ranked.len() > 1 {
            assert!(ranked[0].score > ranked[1].score);
        }
    }

    #[test]
    fn text_includes_note_body_when_present() {
        use mur_common::skill::manifest::Content;
        use mur_common::skill::types::Category;

        let mut s = fake_loaded("note-skill", Priority::Normal);
        s.manifest.category = Category::Note;
        s.manifest.content = Content {
            r#abstract: "abstract line".into(),
            context: None,
            procedure: None,
            command: None,
            note: Some("the note body about rust errors".into()),
        };
        let text = s.text();
        assert!(text.contains("abstract line"));
        assert!(text.contains("the note body about rust errors"));
    }

    #[test]
    fn active_scope_detect_reads_mur_active_team() {
        unsafe {
            std::env::set_var("MUR_ACTIVE_TEAM", "org-xyz");
            std::env::remove_var("MUR_ACTIVE_FLEET");
        }
        let scope = ActiveScope::detect();
        assert_eq!(scope.team.as_deref(), Some("org-xyz"));
        unsafe {
            std::env::remove_var("MUR_ACTIVE_TEAM");
        }
    }

    #[test]
    fn filter_by_scope_excludes_team_skill_when_no_active_team() {
        use mur_common::skill::manifest::{SkillScope, Visibility};
        use std::fs;
        use tempfile::tempdir;

        let tmp = tempdir().unwrap();
        let skills_dir = tmp.path().join("skills");
        fs::create_dir_all(skills_dir.join("t")).unwrap();

        let manifest = SkillManifest {
            name: "t".into(),
            version: "1.0.0".into(),
            publisher: "human:test".into(),
            description: "test team skill".into(),
            category: Category::Context,
            hosts: vec![],
            scope: SkillScope::Team,
            visibility: Visibility::default(),
            origin: None,
            origin_version: None,
            origin_hash: None,
            team: Some("org-x".into()),
            fleet: None,
            project: None,
            governance: None,
            content: Content {
                r#abstract: "".into(),
                context: None,
                procedure: None,
                command: None,
                note: None,
            },
            requires: vec![],
            tags: vec![],
            triggers: vec![],
            priority: Priority::Normal,
            evolution_log: vec![],
            transfer_chain: vec![],
            mcp_requirements: vec![],
            provenance: Default::default(),
            updated_at: Utc::now(),
            requires_programs: vec![],
        };

        let yaml = serde_yaml::to_string(&manifest).unwrap();
        fs::write(skills_dir.join("t").join("skill.yaml"), yaml).unwrap();

        let mut candidates = vec![LoadedSkill {
            manifest,
            stats: SkillStats::new("t", "1.0.0", "", Utc::now()),
        }];

        let ctx = ActiveScope {
            fleet: None,
            project: None,
            team: None,
        };
        filter_by_scope(&mut candidates, &ctx);
        assert!(
            candidates.is_empty(),
            "team skill must not inject without active team"
        );
    }

    #[test]
    fn filter_by_scope_includes_team_skill_when_team_matches() {
        use mur_common::skill::manifest::{SkillScope, Visibility};
        use std::fs;
        use tempfile::tempdir;

        let tmp = tempdir().unwrap();
        let skills_dir = tmp.path().join("skills");
        fs::create_dir_all(skills_dir.join("t")).unwrap();

        let manifest = SkillManifest {
            name: "t".into(),
            version: "1.0.0".into(),
            publisher: "human:test".into(),
            description: "test team skill".into(),
            category: Category::Context,
            hosts: vec![],
            scope: SkillScope::Team,
            visibility: Visibility::default(),
            origin: None,
            origin_version: None,
            origin_hash: None,
            team: Some("org-x".into()),
            fleet: None,
            project: None,
            governance: None,
            content: Content {
                r#abstract: "".into(),
                context: None,
                procedure: None,
                command: None,
                note: None,
            },
            requires: vec![],
            tags: vec![],
            triggers: vec![],
            priority: Priority::Normal,
            evolution_log: vec![],
            transfer_chain: vec![],
            mcp_requirements: vec![],
            provenance: Default::default(),
            updated_at: Utc::now(),
            requires_programs: vec![],
        };

        let yaml = serde_yaml::to_string(&manifest).unwrap();
        fs::write(skills_dir.join("t").join("skill.yaml"), yaml).unwrap();

        let mut candidates = vec![LoadedSkill {
            manifest,
            stats: SkillStats::new("t", "1.0.0", "", Utc::now()),
        }];

        let ctx = ActiveScope {
            fleet: None,
            project: None,
            team: Some("org-x".into()),
        };
        filter_by_scope(&mut candidates, &ctx);
        assert!(
            !candidates.is_empty(),
            "team skill must inject when team matches"
        );
        assert_eq!(candidates[0].name(), "t");
    }
}

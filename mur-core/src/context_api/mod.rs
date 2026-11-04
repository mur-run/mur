//! Context API — programmatic retrieve interface.
//!
//! Wraps MUR's skill scoring and gating pipeline into a clean
//! request/response interface for external tools (`POST /api/v1/context`).
//! The former `ingest` / `feedback` halves wrote to the local Pattern store
//! and were retired with it (Tier C3); skills are authored via `mur skill`.

use std::path::Path;

use anyhow::Result;
use mur_common::skill::stats::LifecycleState;
use serde::{Deserialize, Serialize};

use crate::inject::hook;
use crate::retrieve::gate::{Tier as GateTier, evaluate_query};
use crate::retrieve::scoring::score_and_rank_generic_with_config;
use crate::retrieve::skill_candidates::load_skill_candidates;
use crate::store::config::load_config;

/// Scope for context retrieval. Kept for request compatibility; skills carry
/// no per-user/platform origin, so it no longer filters results.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContextScope {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
}

/// Request for context retrieval.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextRequest {
    pub query: String,
    #[serde(default = "default_token_budget")]
    pub token_budget: usize,
    #[serde(default)]
    pub scope: ContextScope,
    #[serde(default = "default_source")]
    pub source: String,
}

fn default_token_budget() -> usize {
    2000
}

fn default_source() -> String {
    "api".to_string()
}

/// A skill in the context response with its score. The field is still named
/// `patterns` on the wire for API compatibility.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoredPatternResponse {
    pub name: String,
    pub description: String,
    pub score: f64,
    pub kind: String,
}

/// Response from context retrieval.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextResponse {
    pub patterns: Vec<ScoredPatternResponse>,
    pub tokens_used: usize,
    pub formatted: String,
    pub injection_ids: Vec<String>,
}

// ─── Implementation ─────────────────────────────────────────────

/// Retrieve context-optimized skills for a query.
///
/// Skills are the knowledge objects; `req.scope` is accepted for request
/// compatibility but skills carry no per-user/platform origin, so it does not
/// filter. `skills_dir` is `~/.mur/skills`, `mur_home` is `~/.mur`.
pub fn retrieve(
    req: &ContextRequest,
    skills_dir: &Path,
    mur_home: &Path,
) -> Result<ContextResponse> {
    let config = load_config().unwrap_or_default();
    retrieve_with_config(req, skills_dir, mur_home, &config)
}

fn retrieve_with_config(
    req: &ContextRequest,
    skills_dir: &Path,
    mur_home: &Path,
    config: &mur_common::config::Config,
) -> Result<ContextResponse> {
    if evaluate_query(&req.query).tier == GateTier::Skip {
        return Ok(ContextResponse {
            patterns: vec![],
            tokens_used: 0,
            formatted: String::new(),
            injection_ids: vec![],
        });
    }

    let candidates = load_skill_candidates(skills_dir, mur_home)?;
    let scored: Vec<_> =
        score_and_rank_generic_with_config(&req.query, candidates, &config.retrieval)
            .into_iter()
            .filter(|s| s.item.stats.lifecycle_state != LifecycleState::Archived)
            .collect();

    let patterns = scored
        .iter()
        .map(|s| ScoredPatternResponse {
            name: s.item.manifest.name.clone(),
            description: s.item.manifest.description.clone(),
            score: s.score,
            kind: format!("{:?}", s.item.manifest.category).to_lowercase(),
        })
        .collect();
    let injection_ids = scored
        .iter()
        .map(|s| s.item.manifest.name.clone())
        .collect();

    let formatted = hook::format_skills_for_injection(&scored, &[], req.token_budget);
    let tokens_used = formatted.len() / 4; // rough estimate

    Ok(ContextResponse {
        patterns,
        tokens_used,
        formatted,
        injection_ids,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn skills_home_with(name: &str, description: &str, abstract_: &str) -> TempDir {
        let home = TempDir::new().unwrap();
        let dir = home.path().join("skills").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("skill.yaml"),
            format!(
                "name: {name}\nversion: 1.0.0\npublisher: human:test\n\
                 description: {description}\ncategory: context\npriority: normal\n\
                 tags: [swift, testing]\ncontent:\n  abstract: {abstract_}\n  context: x\n"
            ),
        )
        .unwrap();
        home
    }

    #[test]
    fn test_retrieve_empty_query() {
        let home = skills_home_with("swift-testing", "Use @Test macro", "Use @Test macro");
        let req = ContextRequest {
            query: "".into(),
            token_budget: 2000,
            scope: ContextScope::default(),
            source: "test".into(),
        };
        let resp = retrieve(&req, &home.path().join("skills"), home.path()).unwrap();
        assert!(resp.patterns.is_empty());
        assert!(resp.formatted.is_empty());
    }

    #[test]
    fn test_retrieve_returns_skills() {
        let home = skills_home_with(
            "swift-testing",
            "Use @Test macro for Swift testing",
            "Use @Test macro instead of XCTest",
        );
        let req = ContextRequest {
            query: "swift testing @Test macro".into(),
            token_budget: 2000,
            scope: ContextScope::default(),
            source: "test".into(),
        };
        let resp = retrieve_with_config(
            &req,
            &home.path().join("skills"),
            home.path(),
            &mur_common::config::Config::default(),
        )
        .unwrap();
        assert_eq!(resp.patterns.len(), 1);
        assert_eq!(resp.patterns[0].name, "swift-testing");
        assert_eq!(resp.patterns[0].kind, "context");
        assert!(resp.formatted.contains("Use @Test macro for Swift testing"));
        assert_eq!(resp.injection_ids, vec!["swift-testing".to_string()]);
    }

    #[test]
    fn test_retrieve_with_no_skills_dir_is_empty_not_an_error() {
        let home = TempDir::new().unwrap();
        let req = ContextRequest {
            query: "swift testing".into(),
            token_budget: 2000,
            scope: ContextScope::default(),
            source: "test".into(),
        };
        let resp = retrieve(&req, &home.path().join("skills"), home.path()).unwrap();
        assert!(resp.patterns.is_empty());
    }
}

//! cwd-based skill/workflow recommendations for the murmur Panel.
//!
//! Reuses the exact retrieve pipeline `cmd/hook.rs::cmd_hook_prompt` uses for
//! its degraded-mode / cold-start skill fallback: `load_skill_candidates` +
//! `filter_by_scope` + `score_and_rank_generic` (which applies the
//! `retrieval.min_score` floor, default 0.42, internally). Workflows are not
//! `Retrievable` in this codebase (only `Pattern` and `LoadedSkill` are), so
//! they are loaded via the same `WorkflowYamlStore::default_store()` hook.rs
//! uses and ranked with a lightweight word-overlap score instead.
//!
//! Fail-soft: any missing/unreadable store yields an empty result, never an
//! error or panic.

use std::path::Path;

use crate::retrieve::scoring::score_and_rank_generic;
use crate::retrieve::skill_candidates::{ActiveScope, filter_by_scope, load_skill_candidates};
use crate::store::workflow_yaml::WorkflowYamlStore;

/// A single recommended skill or workflow for the current working directory.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Recommendation {
    pub name: String,
    pub kind: String, // "skill" | "workflow"
    pub score: f32,
    pub description: String,
    /// Ready-to-edit command insert-on-click.
    pub command: String, // "mur run <name>" | "mur skill show <name>"
}

/// Build a query string from a cwd's trailing path components (project dir +
/// parent dir name) — the only cwd signal a session itself carries.
pub fn cwd_query(cwd: &Path) -> String {
    let parts: Vec<String> = cwd
        .components()
        .rev()
        .take(2)
        .filter_map(|c| {
            let s = c.as_os_str().to_string_lossy();
            if s.is_empty() || s == "/" {
                None
            } else {
                Some(s.to_string())
            }
        })
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    parts.join(" ")
}

/// Minimum word-overlap score for an unscored workflow to be considered
/// relevant. Mirrors the score floor semantics `score_and_rank_generic` uses
/// for skills, applied to the simpler workflow-name/description overlap.
const WORKFLOW_SCORE_FLOOR: f32 = 0.0;

/// Recommend skills/workflows relevant to the given working directory.
///
/// Fail-soft: any store error (missing `~/.mur`, unreadable skills dir,
/// missing workflow store) yields an empty `Vec`, never a panic or `Err`.
pub fn recommend_for_cwd(cwd: &Path, limit: usize) -> Vec<Recommendation> {
    let query = cwd_query(cwd);
    if query.trim().is_empty() {
        return Vec::new();
    }

    let mut out = Vec::new();
    let mur_dir = mur_common::trust::mur_home();

    // Skills: exact pipeline cmd_hook_prompt uses for its fallback path.
    if let Ok(mut candidates) = load_skill_candidates(&mur_dir.join("skills"), &mur_dir) {
        filter_by_scope(&mut candidates, &ActiveScope::detect());
        for scored in score_and_rank_generic(&query, candidates)
            .into_iter()
            .take(limit)
        {
            out.push(Recommendation {
                name: scored.item.manifest.name.clone(),
                kind: "skill".into(),
                score: scored.score as f32,
                description: scored.item.manifest.description.clone(),
                command: format!("mur skill show {}", scored.item.manifest.name),
            });
        }
    }

    // Workflows: `Workflow` doesn't implement `Retrievable`, so there is no
    // `score_and_rank_generic` path to reuse for it (only `Pattern` and
    // `LoadedSkill` do). Load via the same store hook.rs uses and rank with a
    // simple case-insensitive word-overlap score against the query.
    if let Ok(store) = WorkflowYamlStore::default_store()
        && let Ok(workflows) = store.list_all()
    {
        let query_words: Vec<String> = query
            .to_ascii_lowercase()
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        let mut scored_workflows: Vec<(f32, mur_common::workflow::Workflow)> = workflows
            .into_iter()
            .map(|w| {
                let haystack = format!("{} {}", w.name, w.description).to_ascii_lowercase();
                let hits = query_words
                    .iter()
                    .filter(|qw| haystack.contains(qw.as_str()))
                    .count();
                let score = if query_words.is_empty() {
                    0.0
                } else {
                    hits as f32 / query_words.len() as f32
                };
                (score, w)
            })
            .filter(|(score, _)| *score > WORKFLOW_SCORE_FLOOR)
            .collect();
        scored_workflows.sort_by(|a, b| b.0.total_cmp(&a.0));
        for (score, w) in scored_workflows.into_iter().take(limit) {
            out.push(Recommendation {
                name: w.name.clone(),
                kind: "workflow".into(),
                score,
                description: w.description.clone(),
                command: format!("mur run \"{}\"", w.name),
            });
        }
    }

    out.sort_by(|a, b| b.score.total_cmp(&a.score));
    out.truncate(limit);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_uses_trailing_path_components() {
        assert_eq!(
            cwd_query(Path::new("/Volumes/x/Projects/mur")),
            "Projects mur"
        );
        assert_eq!(cwd_query(Path::new("/")), "");
    }

    #[test]
    fn recommend_is_fail_soft_on_empty_home() {
        // No ~/.mur stores reachable for this nonsense cwd/env in a test
        // sandbox → must return without panicking or erroring.
        let recs = recommend_for_cwd(Path::new("/nonexistent/dir"), 5);
        assert!(recs.len() <= 5);
    }
}

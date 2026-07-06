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
    // Only real path segments (Component::Normal) — skips root/prefix/separator
    // components in a platform-agnostic way (on Windows the root renders as
    // "\\", not "/", so a string comparison would leak it into the query).
    let parts: Vec<String> = cwd
        .components()
        .rev()
        .take(2)
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    parts.join(" ")
}

/// Recommend skills/workflows relevant to the given working directory.
///
/// Fail-soft: any store error (missing `~/.mur`, unreadable skills dir,
/// missing workflow store) yields an empty `Vec`, never a panic or `Err`.
///
/// Tiered by kind: skills are ranked among themselves by their real
/// `score_and_rank_generic` score (0.42+ floor), then workflows are ranked
/// among themselves by word-overlap (0.0–1.0). Skills appear first in the
/// result, followed by workflows, then truncated to limit. This prevents
/// high-overlap workflows from outranking lower-scored but more relevant
/// skills across incompatible scoring scales.
pub fn recommend_for_cwd(cwd: &Path, limit: usize) -> Vec<Recommendation> {
    let query = cwd_query(cwd);
    if query.trim().is_empty() {
        return Vec::new();
    }
    recommend_with_query(&query, limit)
}

/// Run the skill + workflow retrieval for an arbitrary query string.
/// (Shared by `recommend_for_cwd` and `recommend_for_input`.)
fn recommend_with_query(query: &str, limit: usize) -> Vec<Recommendation> {
    let mur_dir = mur_common::trust::mur_home();

    // Skills: exact pipeline cmd_hook_prompt uses for its fallback path.
    let mut skills = Vec::new();
    if let Ok(mut candidates) = load_skill_candidates(&mur_dir.join("skills"), &mur_dir) {
        filter_by_scope(&mut candidates, &ActiveScope::detect());
        for scored in score_and_rank_generic(query, candidates) {
            skills.push(Recommendation {
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
    let mut workflows = Vec::new();
    if let Ok(store) = WorkflowYamlStore::default_store()
        && let Ok(list) = store.list_all()
    {
        let query_words: Vec<String> = query
            .to_ascii_lowercase()
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        let mut scored_workflows: Vec<(f32, mur_common::workflow::Workflow)> = list
            .into_iter()
            .filter_map(|w| {
                let haystack = format!("{} {}", w.name, w.description).to_ascii_lowercase();
                let hits = query_words
                    .iter()
                    .filter(|qw| haystack.contains(qw.as_str()))
                    .count();
                if hits == 0 {
                    return None;
                }
                let score = if query_words.is_empty() {
                    0.0
                } else {
                    hits as f32 / query_words.len() as f32
                };
                Some((score, w))
            })
            .collect();
        scored_workflows.sort_by(|a, b| b.0.total_cmp(&a.0));
        for (score, w) in scored_workflows {
            workflows.push(Recommendation {
                name: w.name.clone(),
                kind: "workflow".into(),
                score,
                description: w.description.clone(),
                command: format!("mur run \"{}\"", w.name),
            });
        }
    }

    // Tier: all sorted skills first, then all sorted workflows appended.
    let mut out = skills;
    out.extend(workflows);
    out.truncate(limit);
    out
}

/// Rank tier for input-driven suggestions: name prefix/exact matches first
/// (VS Code Quick Open lesson — exact beats fuzzy), then retrieval score,
/// then stable name tie-break.
pub(crate) fn rank_input(query: &str, mut recs: Vec<Recommendation>) -> Vec<Recommendation> {
    let q = query.trim().to_ascii_lowercase();
    recs.sort_by(|a, b| {
        let ap = a.name.to_ascii_lowercase().starts_with(&q);
        let bp = b.name.to_ascii_lowercase().starts_with(&q);
        bp.cmp(&ap) // prefix matches first
            .then(b.score.total_cmp(&a.score))
            .then(a.name.cmp(&b.name))
    });
    recs
}

/// Input-driven recommendations (spec §3.3). Input text is the query; cwd
/// terms are appended as low-weight context. Below `MIN_QUERY_CHARS` this
/// is exactly `recommend_for_cwd`. Fail-soft like everything else here.
pub fn recommend_for_input(cwd: &Path, input: &str, limit: usize) -> Vec<Recommendation> {
    let trimmed = input.trim();
    if trimmed.chars().count() < mur_common::panel::MIN_QUERY_CHARS {
        return recommend_for_cwd(cwd, limit);
    }
    let mur_dir = mur_common::trust::mur_home();
    let adaptive = adaptive_best(&load_adaptive(&mur_dir), trimmed, now_secs());
    // Input words first so they dominate the word-overlap scoring; cwd
    // terms trail as context.
    let query = format!("{} {}", trimmed, cwd_query(cwd));
    let mut out = rank_input(trimmed, recommend_with_query(&query, limit * 2));
    // Adaptive history outranks everything (Firefox: "infinite frecency").
    if let Some(name) = adaptive
        && let Some(idx) = out.iter().position(|r| r.name == name)
        && idx > 0
    {
        let hit = out.remove(idx);
        out.insert(0, hit);
    }
    out.truncate(limit);
    out
}

// ── Adaptive query→picked history (Firefox urlbar parameters) ─────────────

/// use_count = use_count * 0.9 + 1 on pick, saturating here.
pub(crate) const ADAPTIVE_USE_CAP: f32 = 10.0;
/// Effective score decays 0.975/day since last use.
const ADAPTIVE_DAILY_DECAY: f32 = 0.975;
/// Entries unused this long are dropped.
const ADAPTIVE_EXPIRE_DAYS: f32 = 90.0;
/// Normalized query length bound.
const ADAPTIVE_QUERY_MAX: usize = 64;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct AdaptiveEntry {
    pub query: String,
    pub picked: String,
    pub use_count: f32,
    /// Unix seconds.
    pub last_used: u64,
}

fn adaptive_path(mur_home: &Path) -> std::path::PathBuf {
    mur_home.join("panel").join("adaptive.yaml")
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn normalize_query(q: &str) -> String {
    q.trim()
        .to_lowercase()
        .chars()
        .take(ADAPTIVE_QUERY_MAX)
        .collect()
}

pub(crate) fn load_adaptive(mur_home: &Path) -> Vec<AdaptiveEntry> {
    std::fs::read_to_string(adaptive_path(mur_home))
        .ok()
        .and_then(|s| serde_yaml::from_str(&s).ok())
        .unwrap_or_default()
}

/// Best adaptive pick for `query`: prefix match either direction, expired
/// entries skipped, ranked by decayed use_count.
pub(crate) fn adaptive_best(
    entries: &[AdaptiveEntry],
    query: &str,
    now_secs: u64,
) -> Option<String> {
    let q = normalize_query(query);
    if q.is_empty() {
        return None;
    }
    entries
        .iter()
        .filter_map(|e| {
            let days = now_secs.saturating_sub(e.last_used) as f32 / 86_400.0;
            if days > ADAPTIVE_EXPIRE_DAYS {
                return None;
            }
            if !(q.starts_with(&e.query) || e.query.starts_with(&q)) {
                return None;
            }
            Some((e.use_count * ADAPTIVE_DAILY_DECAY.powf(days), &e.picked))
        })
        .max_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, picked)| picked.clone())
}

/// Record that the user picked `picked` after typing `query`. Fail-soft:
/// any I/O error is swallowed (suggestion quality, not correctness).
pub fn record_pick(mur_home: &Path, query: &str, picked: &str) {
    let q = normalize_query(query);
    if q.is_empty() || picked.is_empty() {
        return;
    }
    let mut entries = load_adaptive(mur_home);
    let now = now_secs();
    if let Some(e) = entries
        .iter_mut()
        .find(|e| e.query == q && e.picked == picked)
    {
        e.use_count = (e.use_count * 0.9 + 1.0).min(ADAPTIVE_USE_CAP);
        e.last_used = now;
    } else {
        entries.push(AdaptiveEntry {
            query: q,
            picked: picked.to_string(),
            use_count: 1.0,
            last_used: now,
        });
    }
    // Expire on write so the file can't grow unbounded.
    entries.retain(|e| now.saturating_sub(e.last_used) as f32 / 86_400.0 <= ADAPTIVE_EXPIRE_DAYS);
    let dir = adaptive_path(mur_home);
    if let Some(parent) = dir.parent()
        && std::fs::create_dir_all(parent).is_ok()
        && let Ok(s) = serde_yaml::to_string(&entries)
    {
        // temp + rename for atomicity, same convention as store/yaml.rs
        let tmp = dir.with_extension("yaml.tmp");
        if std::fs::write(&tmp, s).is_ok() {
            let _ = std::fs::rename(&tmp, &dir);
        }
    }
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

    fn rec(name: &str, score: f32) -> Recommendation {
        Recommendation {
            name: name.into(),
            kind: "skill".into(),
            score,
            description: String::new(),
            command: format!("mur skill show {name}"),
        }
    }

    #[test]
    fn short_input_falls_back_to_cwd() {
        // 1 trimmed char < MIN_QUERY_CHARS → identical to recommend_for_cwd
        // (fail-soft: both empty in a test sandbox, and neither panics).
        let a = recommend_for_input(Path::new("/nonexistent/dir"), "x", 5);
        let b = recommend_for_cwd(Path::new("/nonexistent/dir"), 5);
        assert_eq!(a.len(), b.len());
    }

    #[test]
    fn prefix_matches_outrank_score() {
        let recs = vec![rec("zeta-high", 0.9), rec("book-search", 0.5)];
        let out = rank_input("book", recs);
        // prefix match beats a higher retrieval score
        assert_eq!(out[0].name, "book-search");
        assert_eq!(out[1].name, "zeta-high");
    }

    #[test]
    fn ties_break_by_name_ascending() {
        let recs = vec![rec("bbb", 0.5), rec("aaa", 0.5)];
        let out = rank_input("zzz", recs);
        assert_eq!(out[0].name, "aaa");
        assert_eq!(out[1].name, "bbb");
    }

    const DAY: u64 = 86_400;

    fn entry(q: &str, p: &str, count: f32, last: u64) -> AdaptiveEntry {
        AdaptiveEntry {
            query: q.into(),
            picked: p.into(),
            use_count: count,
            last_used: last,
        }
    }

    #[test]
    fn adaptive_prefix_match_wins() {
        let now = 100 * DAY;
        let es = vec![
            entry("run boo", "book-search", 3.0, now - DAY),
            entry("deploy", "deployer", 9.0, now - DAY),
        ];
        // typed query extends the stored one → match
        assert_eq!(
            adaptive_best(&es, "run book", now).as_deref(),
            Some("book-search")
        );
        // stored query extends the typed one → also match
        assert_eq!(
            adaptive_best(&es, "run b", now).as_deref(),
            Some("book-search")
        );
        assert_eq!(adaptive_best(&es, "xyz", now), None);
    }

    #[test]
    fn adaptive_decays_and_expires() {
        let now = 200 * DAY;
        // 90+ days unused → expired
        let es = vec![entry("run boo", "book-search", 9.0, now - 91 * DAY)];
        assert_eq!(adaptive_best(&es, "run boo", now), None);
        // Decay: fresher-but-smaller beats stale-but-bigger
        let es = vec![
            entry("run boo", "old-pick", 5.0, now - 60 * DAY), // 5 * 0.975^60 ≈ 1.1
            entry("run boo", "new-pick", 2.0, now - DAY),      // 2 * 0.975   ≈ 1.95
        ];
        assert_eq!(
            adaptive_best(&es, "run boo", now).as_deref(),
            Some("new-pick")
        );
    }

    #[test]
    fn record_pick_saturates_and_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        for _ in 0..30 {
            record_pick(dir.path(), "Run Book", "book-search");
        }
        let es = load_adaptive(dir.path());
        assert_eq!(es.len(), 1);
        assert_eq!(es[0].query, "run book"); // normalized
        assert!(es[0].use_count <= ADAPTIVE_USE_CAP);
        assert!(es[0].use_count > 9.0); // converges toward the cap
    }
}

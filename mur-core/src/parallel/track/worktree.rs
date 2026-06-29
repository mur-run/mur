//! Worktree lifecycle for parallel tracks.

use anyhow::Result;
use std::path::Path;

use crate::parallel::backend::detect_backend;
use mur_common::parallel::ParallelConfig;

use super::{Track, TrackSet};

pub fn create_tracks(config: &ParallelConfig, project: &Path) -> Result<TrackSet> {
    let backend = detect_backend(project);
    let mut tracks = Vec::with_capacity(config.tracks.len());
    for tc in &config.tracks {
        let worktree_path = backend.create_track(&tc.name)?;
        tracks.push(Track {
            config: tc.clone(),
            worktree_path,
        });
    }
    Ok(TrackSet { tracks })
}

pub fn destroy_tracks(tracks: &TrackSet, project: &Path) {
    let backend = detect_backend(project);
    for t in &tracks.tracks {
        if let Err(e) = backend.destroy(&t.worktree_path) {
            eprintln!(
                "warn: failed to destroy track {} at {:?}: {e}",
                t.config.name, t.worktree_path
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::parallel::{JudgeConfig, ParallelConfig, PreFilterKind, Rubric, TrackConfig};
    use std::path::PathBuf;

    /// Walk up from `from` looking for a `.git` DIRECTORY (main repo root, not a worktree).
    fn find_main_repo(from: &Path) -> Option<PathBuf> {
        let mut cur = from.to_path_buf();
        loop {
            if cur.join(".git").is_dir() {
                return Some(cur);
            }
            if !cur.pop() {
                return None;
            }
        }
    }

    fn make_config(suffix: &str) -> ParallelConfig {
        ParallelConfig {
            mode: Default::default(),
            tracks: vec![
                TrackConfig {
                    name: format!("p1-test-{suffix}-a"),
                    approach: "functional".into(),
                    model: None,
                },
                TrackConfig {
                    name: format!("p1-test-{suffix}-b"),
                    approach: "performance".into(),
                    model: None,
                },
            ],
            judge: JudgeConfig {
                model: "claude-haiku-4-5".into(),
                rubric: Rubric::default(),
            },
            pre_filter: vec![PreFilterKind::CargoCheck],
        }
    }

    #[test]
    fn create_tracks_returns_one_track_per_config() {
        let cwd = std::env::current_dir().unwrap();
        let Some(repo) = find_main_repo(&cwd) else {
            eprintln!("skip: not in git repo");
            return;
        };
        let cfg = make_config("create");
        let ts = create_tracks(&cfg, &repo).unwrap();
        assert_eq!(ts.tracks.len(), 2);
        assert!(ts.tracks[0].config.name.contains("create"));
        for t in &ts.tracks {
            assert!(t.worktree_path.exists(), "{:?} should exist", t.worktree_path);
        }
        destroy_tracks(&ts, &repo);
    }

    #[test]
    fn destroy_tracks_removes_worktrees() {
        let cwd = std::env::current_dir().unwrap();
        let Some(repo) = find_main_repo(&cwd) else {
            eprintln!("skip: not in git repo");
            return;
        };
        let cfg = make_config("destroy");
        let ts = create_tracks(&cfg, &repo).unwrap();
        let paths: Vec<_> = ts.tracks.iter().map(|t| t.worktree_path.clone()).collect();
        destroy_tracks(&ts, &repo);
        for p in &paths {
            assert!(!p.exists(), "{p:?} should be removed");
        }
    }
}

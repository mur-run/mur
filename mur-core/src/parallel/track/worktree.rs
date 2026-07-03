//! Worktree lifecycle for parallel tracks.

use anyhow::{Context, Result};
use std::path::Path;

use crate::parallel::backend::{ParallelBackend, cow, detect_backend};
use mur_common::parallel::ParallelConfig;

use super::{Track, TrackSet};

pub fn create_tracks(config: &ParallelConfig, project: &Path) -> Result<TrackSet> {
    // One safety snapshot before creating ANY track — was per-track inside the
    // git backend (and absent for ZFS); now once and backend-agnostic. Non-fatal.
    cow::take_local_snapshot();
    let backend = detect_backend(project);
    create_tracks_with(backend.as_ref(), config)
}

/// Backend-injected core (testable). On any per-track failure, best-effort
/// destroy the worktrees already created before returning the error — otherwise
/// a mid-way failure leaks the 0..N-1 worktrees and a same-name retry collides
/// with git's "already exists".
pub fn create_tracks_with(
    backend: &dyn ParallelBackend,
    config: &ParallelConfig,
) -> Result<TrackSet> {
    let mut created: Vec<Track> = Vec::with_capacity(config.tracks.len());
    for tc in &config.tracks {
        match backend.create_track(&tc.name) {
            Ok(worktree_path) => created.push(Track {
                config: tc.clone(),
                worktree_path,
            }),
            Err(e) => {
                for t in &created {
                    let _ = backend.destroy(&t.worktree_path);
                }
                return Err(e).with_context(|| format!("create track '{}'", tc.name));
            }
        }
    }
    Ok(TrackSet { tracks: created })
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

    /// Throwaway git repo with one commit — hermetic ground for worktree ops.
    fn temp_repo() -> tempfile::TempDir {
        let td = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            let st = std::process::Command::new("git")
                .args(args)
                .current_dir(td.path())
                .status()
                .unwrap();
            assert!(st.success(), "git {args:?} failed");
        };
        run(&["init", "-q"]);
        run(&[
            "-c",
            "user.email=test@test",
            "-c",
            "user.name=test",
            "commit",
            "--allow-empty",
            "-q",
            "-m",
            "init",
        ]);
        td
    }

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
            partition: None,
        }
    }

    // ── #17 leak-rollback: backend-injected, no real worktrees touched ──────
    use crate::parallel::backend::ParallelBackend;
    use std::sync::Mutex;

    struct FailOnNth {
        fail_at: usize,
        created: Mutex<usize>,
        destroyed: Mutex<Vec<PathBuf>>,
    }
    impl ParallelBackend for FailOnNth {
        fn create_track(&self, name: &str) -> anyhow::Result<PathBuf> {
            let mut n = self.created.lock().unwrap();
            if *n == self.fail_at {
                anyhow::bail!("simulated create failure");
            }
            *n += 1;
            Ok(PathBuf::from(format!("/wt/{name}")))
        }
        fn base_snapshot(&self, _t: &Path) -> anyhow::Result<String> {
            Ok(String::new())
        }
        fn diff_files(&self, _t: &Path, _s: &str) -> anyhow::Result<Vec<PathBuf>> {
            Ok(vec![])
        }
        fn promote(&self, _t: &Path, _g: &Path) -> anyhow::Result<()> {
            Ok(())
        }
        fn destroy(&self, track: &Path) -> anyhow::Result<()> {
            self.destroyed.lock().unwrap().push(track.to_path_buf());
            Ok(())
        }
    }

    fn names_config(names: &[&str]) -> ParallelConfig {
        ParallelConfig {
            mode: Default::default(),
            tracks: names
                .iter()
                .map(|n| TrackConfig {
                    name: (*n).into(),
                    approach: String::new(),
                    model: None,
                })
                .collect(),
            judge: JudgeConfig {
                model: "m".into(),
                rubric: Rubric::default(),
            },
            pre_filter: vec![],
            partition: None,
        }
    }

    #[test]
    fn create_tracks_rolls_back_already_created_on_failure() {
        let backend = FailOnNth {
            fail_at: 2,
            created: Mutex::new(0),
            destroyed: Mutex::new(vec![]),
        };
        let res = create_tracks_with(&backend, &names_config(&["a", "b", "c"]));
        assert!(res.is_err(), "should fail at the 3rd track");
        let destroyed = backend.destroyed.lock().unwrap();
        assert_eq!(
            destroyed.len(),
            2,
            "the 2 created worktrees must be torn down (no leak)"
        );
        assert!(destroyed.contains(&PathBuf::from("/wt/a")));
        assert!(destroyed.contains(&PathBuf::from("/wt/b")));
    }

    #[test]
    fn create_tracks_with_all_ok_destroys_nothing() {
        let backend = FailOnNth {
            fail_at: 99,
            created: Mutex::new(0),
            destroyed: Mutex::new(vec![]),
        };
        let ts = create_tracks_with(&backend, &names_config(&["a", "b"])).unwrap();
        assert_eq!(ts.tracks.len(), 2);
        assert!(backend.destroyed.lock().unwrap().is_empty());
    }

    #[test]
    fn create_tracks_returns_one_track_per_config() {
        let td = temp_repo();
        let repo = td.path().to_path_buf();
        let cfg = make_config("create");
        let ts = create_tracks(&cfg, &repo).unwrap();
        assert_eq!(ts.tracks.len(), 2);
        assert!(ts.tracks[0].config.name.contains("create"));
        for t in &ts.tracks {
            assert!(
                t.worktree_path.exists(),
                "{:?} should exist",
                t.worktree_path
            );
        }
        destroy_tracks(&ts, &repo);
    }

    #[test]
    fn destroy_tracks_removes_worktrees() {
        let td = temp_repo();
        let repo = td.path().to_path_buf();
        let cfg = make_config("destroy");
        let ts = create_tracks(&cfg, &repo).unwrap();
        let paths: Vec<_> = ts.tracks.iter().map(|t| t.worktree_path.clone()).collect();
        destroy_tracks(&ts, &repo);
        for p in &paths {
            assert!(!p.exists(), "{p:?} should be removed");
        }
    }
}

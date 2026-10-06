//! Acceptance for the `mur-track` extraction: the crate must expose the
//! `ParallelBackend` seam and the always-available git-worktree backend must
//! round-trip `create_track → mutate → diff_files → destroy` on a throwaway
//! repo, through the public API only.

use mur_track::{GitWorktreeBackend, ParallelBackend};
use std::path::Path;
use std::process::Command;

fn git(repo: &Path, args: &[&str]) {
    let st = Command::new("git")
        .args(["-c", "user.email=t@t", "-c", "user.name=t"])
        .args(args)
        .current_dir(repo)
        .status()
        .unwrap();
    assert!(st.success(), "git {args:?} failed");
}

fn temp_git_repo() -> tempfile::TempDir {
    let td = tempfile::tempdir().unwrap();
    git(td.path(), &["init", "-q"]);
    std::fs::write(td.path().join("a.txt"), "a\n").unwrap();
    std::fs::write(td.path().join("b.txt"), "b\n").unwrap();
    git(td.path(), &["add", "."]);
    git(td.path(), &["commit", "-q", "-m", "init"]);
    td
}

#[test]
fn worktree_backend_reports_only_the_file_that_changed_then_destroys_cleanly() {
    let repo = temp_git_repo();
    let backend: Box<dyn ParallelBackend> =
        Box::new(GitWorktreeBackend::new(repo.path().to_path_buf()));

    let track = backend.create_track("smoke-track").unwrap();
    assert!(track.join("a.txt").exists(), "track is a full checkout");
    let base = backend.base_snapshot(&track).unwrap();

    // Mutate one file on the track and record it, as a turn would.
    std::fs::write(track.join("a.txt"), "a changed\n").unwrap();
    git(&track, &["commit", "-q", "-am", "turn"]);

    let changed = backend.diff_files(&track, &base).unwrap();
    assert_eq!(
        changed,
        vec![track.join("a.txt")],
        "diff names exactly the mutated file"
    );

    backend.destroy(&track).unwrap();
    assert!(!track.exists(), "destroy removes the track directory");
    assert_eq!(
        std::fs::read_to_string(repo.path().join("a.txt")).unwrap(),
        "a\n",
        "the base checkout is untouched"
    );
}

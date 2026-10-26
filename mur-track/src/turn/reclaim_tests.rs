use super::reclaim::*;
use super::{TreeClone, TurnTrack, WORKTREES_DIR};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn repo() -> (tempfile::TempDir, PathBuf) {
    let td = tempfile::tempdir().unwrap();
    git(td.path(), &["init", "-q"]);
    std::fs::write(td.path().join("a.txt"), "a").unwrap();
    std::fs::write(td.path().join("b.txt"), "b").unwrap();
    git(td.path(), &["add", "."]);
    git(td.path(), &["commit", "-q", "-m", "init"]);
    let root = super::canonicalize(td.path()).unwrap();
    (td, root)
}

fn owner_file(track: &TurnTrack) -> PathBuf {
    super::common_git_dir(track.project())
        .unwrap()
        .join("worktrees")
        .join(track.path().file_name().unwrap())
        .join(OWNER_FILE)
}

/// A pid that cannot belong to a live process.
const DEAD_PID: u32 = 999_999_999;

fn set_owner(track: &TurnTrack, json: &str) {
    std::fs::write(owner_file(track), json).unwrap();
}

#[test]
fn live_owner_is_never_stale() {
    let (_td, p) = repo();
    let _t = TurnTrack::create(&p, "turn-live", TreeClone::Copy).unwrap();
    assert!(stale_tracks(&p, Duration::ZERO).unwrap().is_empty());
}

#[test]
fn released_track_is_stale_after_grace_only() {
    let (_td, p) = repo();
    let t = TurnTrack::create(&p, "turn-kept", TreeClone::Copy).unwrap();
    t.release().unwrap();
    assert!(
        stale_tracks(&p, Duration::from_secs(3600))
            .unwrap()
            .is_empty()
    );
    let stale = stale_tracks(&p, Duration::ZERO).unwrap();
    assert_eq!(stale.len(), 1);
    assert_eq!(stale[0].name, "turn-kept");
    assert_eq!(stale[0].reason, StaleReason::Released);
}

#[test]
fn dead_owner_is_stale() {
    let (_td, p) = repo();
    let t = TurnTrack::create(&p, "turn-dead", TreeClone::Copy).unwrap();
    set_owner(&t, &format!("{{\"pid\":{DEAD_PID}}}"));
    let stale = stale_tracks(&p, Duration::ZERO).unwrap();
    assert_eq!(stale[0].reason, StaleReason::OwnerDead);
}

#[test]
fn legacy_track_waits_for_the_legacy_grace() {
    let (_td, p) = repo();
    let t = TurnTrack::create(&p, "turn-old", TreeClone::Copy).unwrap();
    std::fs::remove_file(owner_file(&t)).unwrap();
    assert!(stale_tracks(&p, Duration::ZERO).unwrap().is_empty());
}

#[test]
fn non_turn_worktrees_and_stray_dirs_are_ignored() {
    let (_td, p) = repo();
    let fleet = p.join(WORKTREES_DIR).join("fleet-x");
    git(
        &p,
        &["worktree", "add", "-q", "--detach", fleet.to_str().unwrap()],
    );
    std::fs::create_dir_all(p.join(WORKTREES_DIR).join("stray")).unwrap();
    assert!(stale_tracks(&p, Duration::ZERO).unwrap().is_empty());
}

#[test]
fn reclaim_saves_the_diff_then_removes_the_worktree() {
    let (td, p) = repo();
    let t = TurnTrack::create(&p, "turn-r", TreeClone::Copy).unwrap();
    std::fs::write(t.path().join("a.txt"), "edited").unwrap();
    std::fs::write(t.path().join("new.txt"), "new").unwrap();
    std::fs::remove_file(t.path().join("b.txt")).unwrap();
    t.release().unwrap();
    let stale = stale_tracks(&p, Duration::ZERO).unwrap();
    let patch = td.path().join("out/turn-r.patch");
    reclaim(&p, &stale[0], &patch).unwrap();

    assert!(!t.path().exists());
    let list = Command::new("git")
        .args(["worktree", "list"])
        .current_dir(&p)
        .output()
        .unwrap();
    assert!(!String::from_utf8_lossy(&list.stdout).contains("turn-r"));
    let text = std::fs::read_to_string(&patch).unwrap();
    assert!(text.contains("+edited"), "{text}");
    assert!(text.contains("new.txt"), "{text}");
    assert!(!text.contains("b.txt"), "{text}");
    assert_eq!(
        std::fs::read_to_string(deleted_list(&patch)).unwrap(),
        "b.txt\n"
    );
    // The project is untouched, and the patch applies onto it.
    assert_eq!(std::fs::read_to_string(p.join("a.txt")).unwrap(), "a");
    git(&p, &["apply", "--check", patch.to_str().unwrap()]);
}

#[test]
fn gutted_track_without_git_pointer_is_still_found_and_reclaimed() {
    let (td, p) = repo();
    let t = TurnTrack::create(&p, "turn-gut", TreeClone::Copy).unwrap();
    t.release().unwrap();
    for e in std::fs::read_dir(t.path()).unwrap().flatten() {
        let path = e.path();
        if path.is_dir() {
            std::fs::remove_dir_all(path).unwrap();
        } else {
            std::fs::remove_file(path).unwrap();
        }
    }
    let stale = stale_tracks(&p, Duration::ZERO).unwrap();
    assert_eq!(stale.len(), 1);
    reclaim(&p, &stale[0], &td.path().join("gut.patch")).unwrap();
    assert!(!t.path().exists());
    // The project repo was never mistaken for the track.
    assert_eq!(std::fs::read_to_string(p.join("a.txt")).unwrap(), "a");
    assert!(p.join("b.txt").exists());
}

#[test]
fn unwritable_patch_keeps_the_worktree() {
    let (td, p) = repo();
    let t = TurnTrack::create(&p, "turn-keep", TreeClone::Copy).unwrap();
    std::fs::write(t.path().join("a.txt"), "only copy").unwrap();
    t.release().unwrap();
    let stale = stale_tracks(&p, Duration::ZERO).unwrap();
    // A file where the patch dir should be: the write must fail.
    let blocker = td.path().join("blocked");
    std::fs::write(&blocker, "").unwrap();
    assert!(reclaim(&p, &stale[0], &blocker.join("x.patch")).is_err());
    assert_eq!(
        std::fs::read_to_string(t.path().join("a.txt")).unwrap(),
        "only copy"
    );
}

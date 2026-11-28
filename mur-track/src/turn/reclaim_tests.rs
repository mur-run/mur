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

/// An earlier salvage under the same name — a reused track name, or another
/// repo sharing the patch dir — is never overwritten: the new patch takes
/// the next free `-N` name.
#[test]
fn reclaim_never_overwrites_an_existing_patch() {
    let (td, p) = repo();
    let want = td.path().join("out/turn-r.patch");
    std::fs::create_dir_all(want.parent().unwrap()).unwrap();
    std::fs::write(&want, "earlier salvage").unwrap();
    std::fs::write(td.path().join("out/turn-r-2.deleted.txt"), "earlier list").unwrap();

    let t = TurnTrack::create(&p, "turn-r", TreeClone::Copy).unwrap();
    std::fs::write(t.path().join("a.txt"), "edited").unwrap();
    t.release().unwrap();
    let stale = stale_tracks(&p, Duration::ZERO).unwrap();
    let got = reclaim(&p, &stale[0], &want).unwrap();

    assert_eq!(got, td.path().join("out/turn-r-3.patch"));
    assert_eq!(std::fs::read_to_string(&want).unwrap(), "earlier salvage");
    assert_eq!(
        std::fs::read_to_string(td.path().join("out/turn-r-2.deleted.txt")).unwrap(),
        "earlier list"
    );
    assert!(std::fs::read_to_string(&got).unwrap().contains("+edited"));
}

/// Make `TurnTrack::create` fail *after* `git worktree add`: an ignored file
/// `git status` never reads, but the clone cannot. `None` when permissions
/// are not enforced (running as root), so the caller skips.
#[cfg(unix)]
fn unreadable_ignored_file(p: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(p.join(".gitignore"), "secret\n").unwrap();
    git(p, &["add", ".gitignore"]);
    git(p, &["commit", "-q", "-m", "ignore"]);
    let f = p.join("secret");
    std::fs::write(&f, "s").unwrap();
    std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o000)).unwrap();
    std::fs::read(&f).is_err().then_some(f)
}

#[cfg(unix)]
fn restore(f: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(f, std::fs::Permissions::from_mode(0o644)).unwrap();
}

/// #2618: a create that fails mid-clone must not leave a registered worktree
/// behind (it would have no base and no owner file).
#[cfg(unix)]
#[test]
fn failed_create_leaves_no_worktree() {
    let (_td, p) = repo();
    let Some(f) = unreadable_ignored_file(&p) else {
        return;
    };
    let err = TurnTrack::create(&p, "turn-broken", TreeClone::Copy);
    restore(&f);
    assert!(err.is_err(), "create should fail on the unreadable file");
    assert!(!p.join(WORKTREES_DIR).join("turn-broken").exists());
    let admin = super::common_git_dir(&p).unwrap().join("worktrees");
    assert!(!admin.join("turn-broken").exists());
    // The name is free again.
    TurnTrack::create(&p, "turn-broken", TreeClone::Copy).unwrap();
}

/// Register a worktree the way a crashed create would leave it: added, but
/// no base file and no owner file.
fn half_created(p: &Path, name: &str) -> PathBuf {
    let path = p.join(WORKTREES_DIR).join(name);
    git(
        p,
        &["worktree", "add", "--detach", "-q", path.to_str().unwrap()],
    );
    path
}

fn age_gitdir(p: &Path, name: &str, by: Duration) {
    let f = super::common_git_dir(p)
        .unwrap()
        .join("worktrees")
        .join(name)
        .join("gitdir");
    let then = std::time::SystemTime::now() - by;
    std::fs::File::options()
        .write(true)
        .open(f)
        .unwrap()
        .set_modified(then)
        .unwrap();
}

/// #2618: a half-created turn track is reclaimable once past LEGACY_GRACE,
/// and its changes are still salvaged into the patch.
#[test]
fn incomplete_turn_track_is_stale_after_legacy_grace() {
    let (_td, p) = repo();
    let path = half_created(&p, "turn-orphan");
    std::fs::write(path.join("a.txt"), "edited").unwrap();
    assert!(stale_tracks(&p, Duration::ZERO).unwrap().is_empty());
    age_gitdir(&p, "turn-orphan", LEGACY_GRACE + Duration::from_secs(60));
    let stale = stale_tracks(&p, Duration::ZERO).unwrap();
    assert_eq!(stale.len(), 1);
    assert_eq!(stale[0].reason, StaleReason::Incomplete);
    let td = tempfile::tempdir().unwrap();
    let saved = reclaim(&p, &stale[0], &td.path().join("orphan.patch")).unwrap();
    assert!(std::fs::read_to_string(saved).unwrap().contains("edited"));
    assert!(!path.exists());
}

/// A hand-made worktree under `.worktrees/` is never an incomplete track,
/// however old.
#[test]
fn non_turn_worktree_is_never_incomplete() {
    let (_td, p) = repo();
    half_created(&p, "fix-1234");
    age_gitdir(&p, "fix-1234", LEGACY_GRACE * 10);
    assert!(stale_tracks(&p, Duration::ZERO).unwrap().is_empty());
}

/// A `turn-…` worktree that holds commits of its own (a fleet track, a
/// hand-made one) is never incomplete: the patch would not carry those
/// commits, so removing it would lose them.
#[test]
fn turn_named_worktree_with_commits_is_never_incomplete() {
    let (_td, p) = repo();
    let path = half_created(&p, "turn-foo");
    std::fs::write(path.join("a.txt"), "committed").unwrap();
    git(&path, &["commit", "-q", "-am", "wip"]);
    age_gitdir(&p, "turn-foo", LEGACY_GRACE * 10);
    assert!(stale_tracks(&p, Duration::ZERO).unwrap().is_empty());
}

/// A track that failed before its index was copied never had a checkout;
/// its salvage must not list every tracked path as deleted.
#[test]
fn track_without_index_writes_no_deleted_list() {
    let (_td, p) = repo();
    let path = p.join(WORKTREES_DIR).join("turn-bare");
    git(
        &p,
        &[
            "worktree",
            "add",
            "--detach",
            "--no-checkout",
            "-q",
            path.to_str().unwrap(),
        ],
    );
    std::fs::write(path.join("new.txt"), "fresh").unwrap();
    age_gitdir(&p, "turn-bare", LEGACY_GRACE + Duration::from_secs(60));
    let stale = stale_tracks(&p, Duration::ZERO).unwrap();
    assert_eq!(stale.len(), 1);
    let td = tempfile::tempdir().unwrap();
    let saved = reclaim(&p, &stale[0], &td.path().join("bare.patch")).unwrap();
    assert!(std::fs::read_to_string(&saved).unwrap().contains("fresh"));
    assert!(!deleted_list(&saved).exists());
}

/// Created from a feature branch that was deleted afterwards (squash-merge):
/// HEAD is on no ref, but adds nothing to its creation commit — incomplete.
#[test]
fn half_created_from_deleted_branch_is_incomplete() {
    let (_td, p) = repo();
    git(&p, &["switch", "-q", "-c", "feat"]);
    git(&p, &["commit", "-q", "--allow-empty", "-m", "feat"]);
    half_created(&p, "turn-gone");
    git(&p, &["switch", "-q", "--detach", "HEAD~1"]);
    git(&p, &["branch", "-q", "-D", "feat"]);
    age_gitdir(&p, "turn-gone", LEGACY_GRACE * 10);
    let stale = stale_tracks(&p, Duration::ZERO).unwrap();
    assert_eq!(stale.len(), 1);
    assert_eq!(stale[0].reason, StaleReason::Incomplete);
}

/// Created while the project sat on a detached, unreachable HEAD (mid
/// rebase): still commit-free, so still incomplete.
#[test]
fn half_created_on_detached_unreachable_head_is_incomplete() {
    let (_td, p) = repo();
    git(&p, &["switch", "-q", "--detach"]);
    git(&p, &["commit", "-q", "--allow-empty", "-m", "loose"]);
    half_created(&p, "turn-loose");
    age_gitdir(&p, "turn-loose", LEGACY_GRACE * 10);
    let stale = stale_tracks(&p, Duration::ZERO).unwrap();
    assert_eq!(stale.len(), 1);
    assert_eq!(stale[0].reason, StaleReason::Incomplete);
}

/// When git cannot answer (HEAD unreadable, no reflog), the track is kept.
#[test]
fn git_failure_keeps_turn_named_worktree() {
    let (_td, p) = repo();
    half_created(&p, "turn-broken");
    let git_dir = super::common_git_dir(&p)
        .unwrap()
        .join("worktrees")
        .join("turn-broken");
    std::fs::write(git_dir.join("HEAD"), "not a ref\n").unwrap();
    std::fs::remove_dir_all(git_dir.join("logs")).unwrap();
    age_gitdir(&p, "turn-broken", LEGACY_GRACE * 10);
    assert!(stale_tracks(&p, Duration::ZERO).unwrap().is_empty());
}

/// A clone that died midway leaves an index but missing files. Those paths
/// were never cloned, not deleted: no deleted-list.
#[test]
fn half_cloned_track_with_index_writes_no_deleted_list() {
    let (_td, p) = repo();
    let path = half_created(&p, "turn-half");
    std::fs::remove_file(path.join("b.txt")).unwrap();
    std::fs::write(path.join("new.txt"), "fresh").unwrap();
    age_gitdir(&p, "turn-half", LEGACY_GRACE * 10);
    let stale = stale_tracks(&p, Duration::ZERO).unwrap();
    assert_eq!(stale.len(), 1);
    let td = tempfile::tempdir().unwrap();
    let saved = reclaim(&p, &stale[0], &td.path().join("half.patch")).unwrap();
    assert!(std::fs::read_to_string(&saved).unwrap().contains("fresh"));
    assert!(!deleted_list(&saved).exists());
}

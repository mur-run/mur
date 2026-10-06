use super::*;
use std::process::Command;

fn git(dir: &Path, args: &[&str]) {
    let st = Command::new("git")
        .args(["-c", "user.email=t@t", "-c", "user.name=t"])
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(st.success(), "git {args:?} failed in {}", dir.display());
}

fn git_out(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?} failed");
    String::from_utf8(out.stdout).unwrap()
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap()
}

/// A repo with one commit, then a dirty tracked file and an untracked one —
/// the state a user's checkout is usually in when a turn starts.
fn dirty_repo() -> tempfile::TempDir {
    let td = tempfile::tempdir().unwrap();
    git(td.path(), &["init", "-q"]);
    std::fs::write(td.path().join(".gitignore"), "target/\n.worktrees/\n").unwrap();
    std::fs::write(td.path().join("keep.txt"), "keep").unwrap();
    std::fs::write(td.path().join("gone.txt"), "gone").unwrap();
    std::fs::create_dir(td.path().join("src")).unwrap();
    std::fs::write(td.path().join("src/lib.rs"), "fn a() {}").unwrap();
    git(td.path(), &["add", "."]);
    git(td.path(), &["commit", "-q", "-m", "init"]);
    // Dirty state the turn must inherit but never report.
    std::fs::write(td.path().join("keep.txt"), "keep-dirty").unwrap();
    std::fs::write(td.path().join("untracked.txt"), "new").unwrap();
    // Ignored build output must not be part of the tree clone.
    std::fs::create_dir(td.path().join("target")).unwrap();
    std::fs::write(td.path().join("target/big.o"), "bin").unwrap();
    td
}

/// `tempdir` on macOS lives under /var → /private/var; canonicalize so paths
/// compare equal with what git reports.
fn root(td: &tempfile::TempDir) -> PathBuf {
    std::fs::canonicalize(td.path()).unwrap()
}

#[test]
fn create_clones_dirty_tree_and_shares_git() {
    let td = dirty_repo();
    let project = root(&td);
    let track = TurnTrack::create(&project, "turn-a", TreeClone::Copy).unwrap();
    let p = track.path();
    assert_eq!(p, project.join(WORKTREES_DIR).join("turn-a"));
    assert_eq!(
        read(&p.join("keep.txt")),
        "keep-dirty",
        "dirty state inherited"
    );
    assert_eq!(read(&p.join("untracked.txt")), "new", "untracked inherited");
    assert!(
        p.join(".git").is_file(),
        ".git is a worktree pointer, not a clone"
    );
    assert!(
        !p.join("target").exists(),
        "ignored build output is not cloned by the tree copy"
    );
    // Shared object store: the track resolves the same HEAD as the project.
    assert_eq!(
        git_out(p, &["rev-parse", "HEAD"]),
        git_out(&project, &["rev-parse", "HEAD"])
    );
    assert!(
        track.diff_files().unwrap().is_empty(),
        "nothing changed yet, so nothing is reported — including pre-existing dirt"
    );
}

#[test]
fn diff_reports_only_the_turns_edits_relative_to_project() {
    let td = dirty_repo();
    let project = root(&td);
    let track = TurnTrack::create(&project, "turn-b", TreeClone::Copy).unwrap();
    let p = track.path().to_path_buf();
    std::fs::write(p.join("src/lib.rs"), "fn a() { changed }").unwrap();
    std::fs::write(p.join("brand-new.txt"), "hi").unwrap();
    std::fs::remove_file(p.join("gone.txt")).unwrap();
    // Touching ignored output must stay invisible.
    std::fs::create_dir_all(p.join("target")).unwrap();
    std::fs::write(p.join("target/x.o"), "x").unwrap();

    let mut got = track.diff_files().unwrap();
    got.sort();
    assert_eq!(
        got,
        vec![
            PathBuf::from("brand-new.txt"),
            PathBuf::from("gone.txt"),
            PathBuf::from("src/lib.rs"),
        ]
    );
}

#[test]
fn promote_is_last_write_wins_and_propagates_deletions() {
    let td = dirty_repo();
    let project = root(&td);
    let track = TurnTrack::create(&project, "turn-c", TreeClone::Copy).unwrap();
    let p = track.path().to_path_buf();
    std::fs::write(p.join("src/lib.rs"), "fn a() { changed }").unwrap();
    std::fs::write(p.join("brand-new.txt"), "hi").unwrap();
    std::fs::remove_file(p.join("gone.txt")).unwrap();
    // The user edits the same file meanwhile — P0 policy: the turn wins.
    std::fs::write(project.join("src/lib.rs"), "fn a() { user }").unwrap();

    let promoted = track.promote().unwrap();
    assert_eq!(promoted.len(), 3);
    assert_eq!(read(&project.join("src/lib.rs")), "fn a() { changed }");
    assert_eq!(read(&project.join("brand-new.txt")), "hi");
    assert!(!project.join("gone.txt").exists(), "deletion propagated");
    // Untouched dirt is left exactly as the user had it.
    assert_eq!(read(&project.join("keep.txt")), "keep-dirty");
    assert_eq!(read(&project.join("untracked.txt")), "new");
}

#[test]
fn destroy_removes_dir_and_registration_but_keeps_branch_commits() {
    let td = dirty_repo();
    let project = root(&td);
    let track = TurnTrack::create(&project, "turn-d", TreeClone::Copy).unwrap();
    let p = track.path().to_path_buf();
    // The agent's intended workflow: branch → commit inside the track.
    git(&p, &["checkout", "-q", "-b", "feat/from-track"]);
    std::fs::write(p.join("src/lib.rs"), "fn a() { committed }").unwrap();
    git(&p, &["add", "-A"]);
    git(&p, &["commit", "-q", "-m", "from track"]);

    track.destroy().unwrap();
    assert!(!p.exists());
    assert!(
        !git_out(&project, &["worktree", "list"]).contains("turn-d"),
        "worktree registration removed"
    );
    let msg = git_out(&project, &["log", "-1", "--format=%s", "feat/from-track"]);
    assert_eq!(msg.trim(), "from track", "branch survives the worktree");
}

#[test]
fn create_refuses_unsafe_names_and_non_repos() {
    let td = tempfile::tempdir().unwrap();
    let err = TurnTrack::create(td.path(), "turn-x", TreeClone::Copy).unwrap_err();
    assert!(err.to_string().contains("not a git repository"), "{err}");

    let td = dirty_repo();
    for evil in ["../up", "a/b", "UPPER", ""] {
        let err = TurnTrack::create(&root(&td), evil, TreeClone::Copy).unwrap_err();
        assert!(
            err.to_string().contains("invalid track name"),
            "{evil}: {err}"
        );
    }
}

#[test]
fn open_resumes_an_existing_track_with_its_base() {
    let td = dirty_repo();
    let project = root(&td);
    let track = TurnTrack::create(&project, "turn-e", TreeClone::Copy).unwrap();
    std::fs::write(track.path().join("brand-new.txt"), "hi").unwrap();
    let reopened = TurnTrack::open(&project, "turn-e").unwrap();
    assert_eq!(
        reopened.diff_files().unwrap(),
        vec![PathBuf::from("brand-new.txt")]
    );
}

#[test]
fn display_path_maps_track_paths_back_to_the_project() {
    let td = dirty_repo();
    let project = root(&td);
    let track = TurnTrack::create(&project, "turn-f", TreeClone::Copy).unwrap();
    let inside = track.path().join("src/lib.rs");
    assert_eq!(track.display_path(&inside), project.join("src/lib.rs"));
    let outside = PathBuf::from("/elsewhere/x");
    assert_eq!(track.display_path(&outside), outside);
}

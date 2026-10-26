use super::*;
use mur_track::{TreeClone, TurnTrack};
use std::process::Command;

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
    git(td.path(), &["add", "."]);
    git(td.path(), &["commit", "-q", "-m", "init"]);
    let root = mur_track::turn::canonicalize(td.path()).unwrap();
    (td, root)
}

fn opts(dry_run: bool) -> PruneOpts {
    PruneOpts {
        dry_run,
        grace: Duration::ZERO,
        json: true,
    }
}

fn run_json(home: &Path, dir: &Path, dry_run: bool) -> serde_json::Value {
    let mut buf = Vec::new();
    prune(home, dir, &opts(dry_run), &mut buf).unwrap();
    serde_json::from_slice(&buf).unwrap()
}

#[test]
fn dry_run_lists_but_keeps_the_track() {
    let (_td, p) = repo();
    let home = tempfile::tempdir().unwrap();
    let t = TurnTrack::create(&p, "turn-kept", TreeClone::Copy).unwrap();
    t.release().unwrap();
    let rows = run_json(home.path(), &p, true);
    assert_eq!(rows[0]["name"], "turn-kept");
    assert_eq!(rows[0]["reason"], "released");
    assert_eq!(rows[0]["status"], "would_reclaim");
    assert!(t.path().exists(), "dry run must not touch the track");
    assert!(!patch_dir(home.path(), &p).exists());
}

#[test]
fn prune_saves_patch_then_removes_released_track() {
    let (_td, p) = repo();
    let home = tempfile::tempdir().unwrap();
    let t = TurnTrack::create(&p, "turn-kept", TreeClone::Copy).unwrap();
    std::fs::write(t.path().join("a.txt"), "edited").unwrap();
    t.release().unwrap();
    let track_path = t.path().to_path_buf();
    drop(t);

    let rows = run_json(home.path(), &p, false);
    assert_eq!(rows[0]["status"], "reclaimed");
    assert!(!track_path.exists());
    let patch = PathBuf::from(rows[0]["patch"].as_str().unwrap());
    assert!(patch.starts_with(patch_dir(home.path(), &p)));
    assert!(std::fs::read_to_string(patch).unwrap().contains("edited"));
}

#[test]
fn a_live_sessions_track_is_never_pruned() {
    let (_td, p) = repo();
    let home = tempfile::tempdir().unwrap();
    // Owned by this (live) process and not released.
    let live = TurnTrack::create(&p, "turn-live", TreeClone::Copy).unwrap();
    let rows = run_json(home.path(), &p, false);
    assert_eq!(rows.as_array().unwrap().len(), 0);
    assert!(live.path().exists());
}

#[test]
fn prune_from_inside_a_track_sweeps_the_main_project() {
    let (_td, p) = repo();
    let home = tempfile::tempdir().unwrap();
    let live = TurnTrack::create(&p, "turn-live", TreeClone::Copy).unwrap();
    let kept = TurnTrack::create(&p, "turn-kept", TreeClone::Copy).unwrap();
    kept.release().unwrap();
    let rows = run_json(home.path(), live.path(), true);
    let names: Vec<_> = rows
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, ["turn-kept"]);
}

/// An ordinary linked checkout is a project in its own right: its tracks
/// live under its own `.worktrees/`, and prune sweeps those — not the main
/// checkout's.
#[test]
fn prune_in_a_linked_checkout_sweeps_its_own_tracks() {
    let (td, p) = repo();
    let home = tempfile::tempdir().unwrap();
    let main_kept = TurnTrack::create(&p, "turn-main", TreeClone::Copy).unwrap();
    main_kept.release().unwrap();
    let linked = td.path().join("linked");
    git(
        &p,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            linked.to_str().unwrap(),
        ],
    );
    let linked = mur_track::turn::canonicalize(&linked).unwrap();
    let own = TurnTrack::create(&linked, "turn-own", TreeClone::Copy).unwrap();
    own.release().unwrap();

    assert_eq!(
        mur_track::turn::reclaim::project_root(&linked).unwrap(),
        linked
    );
    assert_eq!(
        mur_track::turn::reclaim::project_root(own.path()).unwrap(),
        linked
    );
    let rows = run_json(home.path(), &linked, false);
    let names: Vec<_> = rows
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, ["turn-own"]);
    assert!(!own.path().exists());
    assert!(main_kept.path().exists(), "main checkout's track untouched");
}

/// Two repositories with the same checkout name get separate patch dirs,
/// so neither salvage can land on the other's.
#[test]
fn same_basename_repos_get_distinct_patch_dirs() {
    let home = tempfile::tempdir().unwrap();
    let a = Path::new("/work/client/app");
    let b = Path::new("/work/server/app");
    assert_ne!(patch_dir(home.path(), a), patch_dir(home.path(), b));
    assert_eq!(patch_dir(home.path(), a), patch_dir(home.path(), a));
    let name = patch_dir(home.path(), a);
    assert!(
        name.file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("app-")
    );
}

/// Pruning a reused track name keeps the earlier salvage beside the new one.
#[test]
fn prune_keeps_an_earlier_patch_of_the_same_track_name() {
    let (_td, p) = repo();
    let home = tempfile::tempdir().unwrap();
    let dir = patch_dir(home.path(), &p);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("turn-x.patch"), "earlier").unwrap();
    let t = TurnTrack::create(&p, "turn-x", TreeClone::Copy).unwrap();
    std::fs::write(t.path().join("a.txt"), "edited").unwrap();
    t.release().unwrap();
    let rows = run_json(home.path(), &p, false);
    let patch = PathBuf::from(rows[0]["patch"].as_str().unwrap());
    assert_eq!(patch, dir.join("turn-x-2.patch"));
    assert_eq!(
        std::fs::read_to_string(dir.join("turn-x.patch")).unwrap(),
        "earlier"
    );
}

#[test]
fn empty_diff_reports_no_patch() {
    let (_td, p) = repo();
    let home = tempfile::tempdir().unwrap();
    let t = TurnTrack::create(&p, "turn-clean", TreeClone::Copy).unwrap();
    t.release().unwrap();
    let rows = run_json(home.path(), &p, false);
    assert_eq!(rows[0]["status"], "reclaimed");
    assert!(rows[0].get("patch").is_none());
}

/// A bare repository has no checkout to hold `.worktrees/`. (A plain temp
/// dir is no test of "not a repo": it may sit inside one, e.g. `~/.mur`.)
#[test]
fn bare_repo_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q", "--bare"]);
    let home = tempfile::tempdir().unwrap();
    let mut buf = Vec::new();
    let err = prune(home.path(), dir.path(), &opts(true), &mut buf).unwrap_err();
    assert!(format!("{err:#}").contains("bare"), "{err:#}");
}

#[test]
fn grace_parses_units_and_rejects_junk() {
    assert_eq!(parse_grace("0").unwrap(), Duration::ZERO);
    assert_eq!(parse_grace("10m").unwrap(), Duration::from_secs(600));
    assert!(parse_grace("soon").is_err());
}

#[test]
fn configured_grace_defaults_without_config() {
    let home = tempfile::tempdir().unwrap();
    let want = mur_common::config::TURN_TRACK_DEFAULT_STALE_GRACE_MINS * 60;
    assert_eq!(configured_grace(home.path()), Duration::from_secs(want));
}

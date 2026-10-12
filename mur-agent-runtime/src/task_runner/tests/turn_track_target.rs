//! Per-turn Cargo target dirs follow their track: created by Cargo, removed
//! when the track is destroyed, kept while a failed promote keeps the track,
//! and swept once the worktree is gone (turn-track warm target design §4.6).

use super::*;
use mur_track::turn::target::turn_target_dir;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Default `turn_track.warm_build_cache.turn_target_subdir`.
const SUBDIR: &str = mur_common::config::TURN_TRACK_DEFAULT_TURN_TARGET_SUBDIR;
/// Turn-dir removal runs off the closing thread; give it this long.
const REMOVAL_WAIT: Duration = Duration::from_secs(2);

fn git_repo() -> (tempfile::TempDir, PathBuf) {
    let td = tempfile::tempdir().unwrap();
    let root = mur_track::turn::canonicalize(td.path()).unwrap();
    let git = |args: &[&str]| {
        let st = std::process::Command::new("git")
            .args(["-c", "user.email=t@t", "-c", "user.name=t"])
            .args(args)
            .current_dir(&root)
            .status()
            .unwrap();
        assert!(st.success());
    };
    git(&["init", "-q"]);
    std::fs::write(root.join("a.txt"), "a\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "init"]);
    (td, root)
}

fn runner_for(project: &Path) -> (TaskRunner, crate::tools::fs_policy::SessionCwd) {
    let cwd = crate::tools::fs_policy::SessionCwd::new(project.to_path_buf());
    let runner = TaskRunner::new_stub_echo()
        .with_tools(vec![Arc::new(
            crate::tools::write_file::WriteFileTool::new_for_test(
                cwd.clone(),
                mur_common::agent::FilesystemEntitlement::default(),
            ),
        )])
        .with_session_cwd(cwd.clone(), vec![project.to_string_lossy().into_owned()]);
    cwd.begin_turn("t1", None, Some(project.to_path_buf()));
    (runner, cwd)
}

/// What Cargo would leave behind after a build in the turn.
fn fake_build(dir: &Path) {
    std::fs::create_dir_all(dir.join("debug/deps")).unwrap();
    std::fs::write(dir.join("debug/deps/liba.rlib"), b"x").unwrap();
}

fn gone_within(p: &Path, wait: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < wait {
        if !p.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    !p.exists()
}

fn turn_dir_of(open: &super::super::turn_track::OpenTrack) -> PathBuf {
    let name = open.track.path().file_name().unwrap().to_str().unwrap();
    turn_target_dir(open.track.project(), SUBDIR, name)
}

#[test]
fn close_removes_turn_dir() {
    let (_td, project) = git_repo();
    let (runner, _cwd) = runner_for(&project);
    let open = runner.open_turn_track("t1").expect("track at a repo root");
    let dir = turn_dir_of(&open);
    fake_build(&dir);
    std::fs::write(open.track.path().join("a.txt"), "edited\n").unwrap();

    let mut ledger = crate::turn_ledger::TurnLedger::default();
    runner.close_turn_track("t1", open, &mut ledger);

    assert!(ledger.track_kept.is_none());
    assert!(gone_within(&dir, REMOVAL_WAIT), "turn dir removed");
}

#[cfg(unix)]
#[test]
fn failed_promote_keeps_turn_dir() {
    use std::os::unix::fs::PermissionsExt;
    let (_td, project) = git_repo();
    let (runner, _cwd) = runner_for(&project);
    let open = runner.open_turn_track("t1").expect("track at a repo root");
    let dir = turn_dir_of(&open);
    fake_build(&dir);
    std::fs::write(open.track.path().join("a.txt"), "edited\n").unwrap();
    // A read-only project root: promote cannot replace a.txt.
    let set_mode = |m| std::fs::set_permissions(&project, std::fs::Permissions::from_mode(m));
    set_mode(0o555).unwrap();

    let mut ledger = crate::turn_ledger::TurnLedger::default();
    runner.close_turn_track("t1", open, &mut ledger);
    set_mode(0o755).unwrap();

    assert!(ledger.track_kept.is_some(), "promote failed, track kept");
    assert!(
        !gone_within(&dir, Duration::from_millis(300)),
        "a kept track keeps its turn dir"
    );
}

#[test]
fn opening_a_track_sweeps_orphan_turn_dirs() {
    let (_td, project) = git_repo();
    let orphan = turn_target_dir(&project, SUBDIR, "turn-crashed");
    fake_build(&orphan);
    let (runner, _cwd) = runner_for(&project);

    let open = runner.open_turn_track("t1").expect("track at a repo root");

    assert!(!orphan.exists(), "orphan swept when the track opened");
    let mut ledger = crate::turn_ledger::TurnLedger::default();
    runner.close_turn_track("t1", open, &mut ledger);
}

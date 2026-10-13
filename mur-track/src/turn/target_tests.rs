use super::WORKTREES_DIR;
use super::target::*;
use std::path::Path;

fn mkdir(p: &Path) {
    std::fs::create_dir_all(p).unwrap();
}

/// A turn dir with some content, like a real target clone.
fn make_turn_dir(project: &Path, name: &str) -> std::path::PathBuf {
    let d = turn_target_dir(project, "turns", name);
    mkdir(&d.join("debug/deps"));
    std::fs::write(d.join("debug/deps/libx.rlib"), b"x").unwrap();
    d
}

#[test]
fn turn_target_dir_layout() {
    assert_eq!(
        turn_target_dir(Path::new("/p"), "turns", "turn-01ab"),
        Path::new("/p/target/turns/turn-01ab")
    );
}

#[test]
fn remove_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let d = make_turn_dir(tmp.path(), "turn-a");
    remove_turn_target(&d).unwrap();
    assert!(!d.exists());
    remove_turn_target(&d).unwrap();
}

#[test]
fn sweep_removes_orphan_keeps_live() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path();
    mkdir(&p.join(WORKTREES_DIR).join("turn-a"));
    let live = make_turn_dir(p, "turn-a");
    let orphan = make_turn_dir(p, "turn-b");

    let report = sweep_orphan_turn_targets(p, "turns");

    assert!(live.exists());
    assert!(!orphan.exists());
    assert_eq!(report.removed, vec![orphan]);
    assert!(report.failed.is_empty());
}

#[test]
fn sweep_missing_subdir_is_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let report = sweep_orphan_turn_targets(tmp.path(), "turns");
    assert!(report.removed.is_empty());
    assert!(report.failed.is_empty());
}

#[test]
fn sweep_and_remove_race() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().to_path_buf();
    let d = make_turn_dir(&p, "turn-race");
    let handles: Vec<_> = (0..8)
        .map(|i| {
            let (p, d) = (p.clone(), d.clone());
            std::thread::spawn(move || {
                if i % 2 == 0 {
                    remove_turn_target(&d).unwrap();
                } else {
                    let r = sweep_orphan_turn_targets(&p, "turns");
                    assert!(r.failed.is_empty(), "{:?}", r.failed);
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    assert!(!d.exists());
}

#[test]
fn sweep_clears_stale_trash() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path();
    let trash = p.join("target/turns/.trash-turn-x-1-2");
    mkdir(&trash.join("debug"));

    let report = sweep_orphan_turn_targets(p, "turns");

    assert!(!trash.exists());
    assert!(report.removed.is_empty(), "trash is not a turn dir");
}

//! `trash empty` / `trash now` deletion (spec §5, D5).

use super::purge;
use std::fs;
use std::os::unix::fs::symlink;

fn root() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap().join("trash");
    fs::create_dir_all(&root).unwrap();
    (tmp, root)
}

#[test]
fn directory_entry_is_removed() {
    // Regression: `remove_file` failed on the directories `rm -r` now trashes.
    let (_t, root) = root();
    let entry = root.join("0d6c").join("build");
    fs::create_dir_all(entry.join("sub")).unwrap();
    fs::write(entry.join("sub/a.o"), "x").unwrap();

    purge(&root, &entry).unwrap();

    assert!(!entry.exists());
    assert!(!root.join("0d6c").exists(), "empty entry dir is cleaned up");
    assert!(root.is_dir(), "trash root survives");
}

#[test]
fn link_pointing_outside_is_removed_target_kept() {
    let (t, root) = root();
    let outside = t.path().join("precious");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("keep.txt"), "x").unwrap();
    let entry = root.join("9a1f").join("link");
    fs::create_dir_all(entry.parent().unwrap()).unwrap();
    symlink(&outside, &entry).unwrap();

    purge(&root, &entry).unwrap();

    assert!(fs::symlink_metadata(&entry).is_err(), "link removed");
    assert!(outside.join("keep.txt").exists(), "target untouched");
}

#[test]
fn root_and_paths_outside_it_are_refused() {
    let (t, root) = root();
    assert!(purge(&root, &root).is_err());
    assert!(root.is_dir());

    let outside = t.path().join("elsewhere.txt");
    fs::write(&outside, "x").unwrap();
    assert!(purge(&root, &outside).is_err());
    assert!(outside.exists());
}

#[test]
fn already_gone_is_not_an_error() {
    let (_t, root) = root();
    purge(&root, &root.join("nope").join("x")).unwrap();
}

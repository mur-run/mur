//! Tests for D3–D5: layout, `move_across`, path safety.

use std::fs;
use std::io;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::time::Duration;

use crate::layout::{entry_path, partial_path, staging_root, sweep_partials, trash_root};
use crate::mover::{MoveOutcome, move_with, move_with_staging};
use crate::safety::{Refusal, check_target, remove_within};
use crate::{move_across, resolve_no_follow};

fn exdev(_: &Path, _: &Path) -> io::Result<()> {
    Err(io::Error::from(io::ErrorKind::CrossesDevices))
}

#[test]
fn same_name_twice_gets_two_entry_dirs() {
    let t = tempfile::tempdir().unwrap();
    let root = trash_root(t.path());
    let src = t.path().join("a.txt");
    let mut dsts = Vec::new();
    for body in ["one", "two"] {
        fs::write(&src, body).unwrap();
        let dst = entry_path(&root, uuid::Uuid::new_v4(), &src).unwrap();
        assert!(matches!(
            move_across(&src, &dst).unwrap(),
            MoveOutcome::Moved
        ));
        dsts.push(dst);
    }
    assert_ne!(dsts[0], dsts[1]);
    assert_eq!(fs::read_to_string(&dsts[0]).unwrap(), "one");
    assert_eq!(fs::read_to_string(&dsts[1]).unwrap(), "two");
    assert!(!src.exists());
}

#[test]
fn symlinks_move_as_links_and_targets_survive() {
    let t = tempfile::tempdir().unwrap();
    let real_dir = t.path().join("real");
    fs::create_dir(&real_dir).unwrap();
    fs::write(real_dir.join("keep"), "x").unwrap();
    let real_file = t.path().join("f");
    fs::write(&real_file, "y").unwrap();
    symlink(&real_dir, t.path().join("ldir")).unwrap();
    symlink(&real_file, t.path().join("lfile")).unwrap();

    for (name, fake_exdev) in [("ldir", false), ("lfile", true)] {
        let src = t.path().join(name);
        let dst = t.path().join("trash").join(name);
        let out = if fake_exdev {
            move_with(&src, &dst, exdev).unwrap()
        } else {
            move_across(&src, &dst).unwrap()
        };
        assert!(matches!(out, MoveOutcome::Moved));
        assert!(fs::symlink_metadata(&dst).unwrap().file_type().is_symlink());
        assert!(fs::symlink_metadata(&src).is_err());
    }
    assert_eq!(fs::read_to_string(real_dir.join("keep")).unwrap(), "x");
    assert_eq!(fs::read_to_string(&real_file).unwrap(), "y");
}

#[test]
fn exdev_copies_tree_then_removes_source() {
    let t = tempfile::tempdir().unwrap();
    let src = t.path().join("dir");
    fs::create_dir_all(src.join("sub")).unwrap();
    fs::write(src.join("sub/f"), "deep").unwrap();
    symlink("sub/f", src.join("link")).unwrap();
    let dst = t.path().join("trash/id/dir");

    assert!(matches!(
        move_with(&src, &dst, exdev).unwrap(),
        MoveOutcome::Moved
    ));
    assert!(!src.exists());
    assert!(!partial_path(&dst).exists());
    assert_eq!(fs::read_to_string(dst.join("sub/f")).unwrap(), "deep");
    assert_eq!(fs::read_link(dst.join("link")).unwrap(), Path::new("sub/f"));
}

#[test]
fn exdev_with_unremovable_source_keeps_the_copy() {
    use std::os::unix::fs::PermissionsExt;
    let t = tempfile::tempdir().unwrap();
    let parent = t.path().join("locked");
    fs::create_dir(&parent).unwrap();
    let src = parent.join("f");
    fs::write(&src, "z").unwrap();
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o555)).unwrap();
    let dst = t.path().join("trash/id/f");

    let out = move_with(&src, &dst, exdev);
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(out.unwrap(), MoveOutcome::CopiedSourceKept(_)));
    assert_eq!(fs::read_to_string(&dst).unwrap(), "z");
    assert!(src.exists());
}

#[test]
fn move_refuses_existing_destination_and_missing_source() {
    let t = tempfile::tempdir().unwrap();
    let src = t.path().join("a");
    let dst = t.path().join("b");
    fs::write(&src, "a").unwrap();
    fs::write(&dst, "b").unwrap();
    assert_eq!(
        move_across(&src, &dst).unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
    assert_eq!(fs::read_to_string(&dst).unwrap(), "b");
    let gone = t.path().join("gone");
    assert_eq!(
        move_across(&gone, &t.path().join("c")).unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
}

#[test]
fn check_target_refusals() {
    let t = tempfile::tempdir().unwrap();
    let home = t.path().join("agents/pm");
    fs::create_dir_all(home.join("trash/id")).unwrap();
    fs::write(home.join("trash/id/f"), "").unwrap();
    fs::write(t.path().join("ok"), "").unwrap();

    let check = |p: &Path| check_target(p, &home).unwrap();
    assert_eq!(check(Path::new("/")), Err(Refusal::FilesystemRoot));
    assert_eq!(check(&home), Err(Refusal::AgentHome));
    assert_eq!(check(&t.path().join("agents")), Err(Refusal::AgentHome));
    assert_eq!(check(&home.join("trash")), Err(Refusal::InsideTrash));
    assert_eq!(check(&home.join("trash/id/f")), Err(Refusal::InsideTrash));
    // Reached through `..`, still inside.
    assert_eq!(
        check(&home.join("trash/id/../id")),
        Err(Refusal::InsideTrash)
    );
    assert!(check(&t.path().join("ok")).is_ok());
    assert!(check_target(&t.path().join("missing"), &home).is_err());
}

#[test]
fn symlink_into_trash_is_trashable_as_a_link() {
    // A link living outside that points into the trash is just a link.
    let t = tempfile::tempdir().unwrap();
    let home = t.path().join("home");
    fs::create_dir_all(home.join("trash")).unwrap();
    let link = t.path().join("l");
    symlink(home.join("trash"), &link).unwrap();
    let resolved = check_target(&link, &home).unwrap().unwrap();
    assert_eq!(resolved, resolve_no_follow(&link).unwrap());
}

#[test]
fn remove_within_stays_inside_the_root() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("trash");
    let outside = t.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("precious"), "p").unwrap();
    fs::create_dir_all(root.join("e1/dir/sub")).unwrap();
    fs::create_dir_all(root.join("e2")).unwrap();
    symlink(&outside, root.join("e2/link")).unwrap();

    remove_within(&root, &root.join("e2/link")).unwrap();
    assert_eq!(fs::read_to_string(outside.join("precious")).unwrap(), "p");
    remove_within(&root, &root.join("e1")).unwrap(); // directory: regression for remove_file
    assert!(!root.join("e1").exists());
    assert!(remove_within(&root, &root).is_err());
    assert!(remove_within(&root, &outside).is_err());
    assert!(remove_within(&root, &root.join("e2/../../outside")).is_err());
    assert!(root.exists() && outside.exists());
}

/// Review #2164 #1: a retained entry whose own name ends in `.partial`, with
/// an mtime far in the past (rename and copy both keep it), is not staging.
#[test]
fn sweep_never_touches_retained_entries_named_partial() {
    let t = tempfile::tempdir().unwrap();
    let root = trash_root(t.path());
    let old = std::time::SystemTime::now() - Duration::from_secs(7 * 24 * 3600);
    let file = root
        .join(uuid::Uuid::new_v4().to_string())
        .join("report.partial");
    let dir = root
        .join(uuid::Uuid::new_v4().to_string())
        .join("build.partial");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, "keep me").unwrap();
    fs::create_dir_all(dir.join("inner")).unwrap();
    // Entry dirs too: a real retained entry is older than the sweep's floor.
    for p in [&file, &dir, file.parent().unwrap(), dir.parent().unwrap()] {
        fs::File::open(p).unwrap().set_modified(old).unwrap();
    }

    assert_eq!(sweep_partials(&root, Duration::from_secs(3600)).unwrap(), 0);
    assert_eq!(fs::read_to_string(&file).unwrap(), "keep me");
    assert!(dir.join("inner").is_dir());
}

/// A cross-device move into the trash stages outside every entry dir, so a
/// crash leaves nothing inside `<trash>/<id>/` for the sweep to judge.
#[test]
fn exdev_into_trash_stages_outside_entry_dirs() {
    let t = tempfile::tempdir().unwrap();
    let root = trash_root(t.path());
    let src = t.path().join("old.partial");
    fs::write(&src, "x").unwrap();
    let dst = entry_path(&root, uuid::Uuid::new_v4(), &src).unwrap();
    // Copy fails on a FIFO-free path: make the first rename EXDEV and the
    // staged copy observable by failing the final rename into the entry dir.
    let staged = std::cell::RefCell::new(None);
    let out = move_with_staging(&src, &dst, &staging_root(&root), |a, b| {
        if b == dst.as_path() && a != src.as_path() {
            *staged.borrow_mut() = Some(a.to_path_buf());
            return Err(io::Error::other("crash before final rename"));
        }
        exdev(a, b)
    });
    assert!(out.is_err());
    let staged = staged.into_inner().expect("copy was staged");
    assert!(staged.starts_with(staging_root(&root)), "{staged:?}");
}

#[test]
fn sweep_removes_stale_staging_but_not_fresh() {
    let t = tempfile::tempdir().unwrap();
    let root = trash_root(t.path());
    let stage = staging_root(&root).join("crashed");
    fs::create_dir_all(stage.join("x.partial/inner")).unwrap();
    assert_eq!(sweep_partials(&root, Duration::from_secs(3600)).unwrap(), 0);
    assert!(stage.exists());
    assert_eq!(sweep_partials(&root, Duration::ZERO).unwrap(), 1);
    assert!(!stage.exists());
    assert_eq!(
        sweep_partials(&t.path().join("nope"), Duration::ZERO).unwrap(),
        0
    );
}

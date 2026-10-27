//! Review #2168: a move never replaces or removes anything it did not create.

use std::fs;
use std::io;
use std::os::unix::fs::symlink;
use std::path::Path;

use crate::mover::{MoveOutcome, RESTORE_STAGING_PREFIX, move_with};
use crate::publish::{claim_then_rename, rename_noreplace};

fn exdev(_: &Path, _: &Path) -> io::Result<()> {
    Err(io::Error::from(io::ErrorKind::CrossesDevices))
}

type Rename = fn(&Path, &Path) -> io::Result<()>;
/// The native syscall and the portable fallback must behave the same.
const RENAMES: [(&str, Rename); 2] = [
    ("native", rename_noreplace),
    ("fallback", claim_then_rename),
];

/// What can already sit at a destination.
fn occupy(at: &Path, kind: &str) {
    match kind {
        "file" => fs::write(at, "theirs").unwrap(),
        "dir" => {
            fs::create_dir(at).unwrap();
            fs::write(at.join("keep"), "theirs").unwrap();
        }
        "dangling" => symlink(at.with_extension("nowhere"), at).unwrap(),
        _ => unreachable!(),
    }
}

fn assert_occupied(at: &Path, kind: &str) {
    match kind {
        "file" => assert_eq!(fs::read_to_string(at).unwrap(), "theirs"),
        "dir" => assert_eq!(fs::read_to_string(at.join("keep")).unwrap(), "theirs"),
        "dangling" => assert!(fs::symlink_metadata(at).unwrap().file_type().is_symlink()),
        _ => unreachable!(),
    }
}

#[test]
fn noreplace_refuses_every_kind_of_existing_destination() {
    for (how, rename) in RENAMES {
        for src_is_dir in [false, true] {
            for kind in ["file", "dir", "dangling"] {
                let t = tempfile::tempdir().unwrap();
                let (src, dst) = (t.path().join("src"), t.path().join("dst"));
                if src_is_dir {
                    fs::create_dir(&src).unwrap();
                    fs::write(src.join("mine"), "ours").unwrap();
                } else {
                    fs::write(&src, "ours").unwrap();
                }
                occupy(&dst, kind);

                let err = rename(&src, &dst).unwrap_err();

                let ctx = format!("{how} dir={src_is_dir} over {kind}");
                assert_eq!(err.kind(), io::ErrorKind::AlreadyExists, "{ctx}: {err}");
                assert_occupied(&dst, kind);
                assert!(fs::symlink_metadata(&src).is_ok(), "{ctx}: source kept");
            }
        }
    }
}

#[test]
fn noreplace_moves_files_dirs_and_links_when_free() {
    for (how, rename) in RENAMES {
        let t = tempfile::tempdir().unwrap();
        fs::write(t.path().join("f"), "x").unwrap();
        fs::create_dir(t.path().join("d")).unwrap();
        fs::write(t.path().join("d/inner"), "y").unwrap();
        symlink("nowhere", t.path().join("l")).unwrap();

        for name in ["f", "d", "l"] {
            let (src, dst) = (t.path().join(name), t.path().join(format!("{name}2")));
            rename(&src, &dst).unwrap_or_else(|e| panic!("{how} {name}: {e}"));
            assert!(fs::symlink_metadata(&src).is_err(), "{how} {name}");
        }
        assert_eq!(fs::read_to_string(t.path().join("f2")).unwrap(), "x");
        assert_eq!(fs::read_to_string(t.path().join("d2/inner")).unwrap(), "y");
        assert_eq!(
            fs::read_link(t.path().join("l2")).unwrap(),
            Path::new("nowhere")
        );
    }
}

/// Review #2168 #2: something created at `dst` after the up-front check must
/// survive, on the direct path and on the cross-device publish.
#[test]
fn destination_created_after_the_check_is_never_replaced() {
    for cross_device in [false, true] {
        let t = tempfile::tempdir().unwrap();
        let (src, dst) = (t.path().join("trashed"), t.path().join("work/report"));
        fs::write(&src, "restored").unwrap();

        let err = move_with(&src, &dst, |a, b| {
            fs::write(b, "recreated").unwrap(); // the racing writer
            if cross_device {
                exdev(a, b)
            } else {
                rename_noreplace(a, b)
            }
        })
        .unwrap_err();

        let ctx = format!("cross_device={cross_device}");
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists, "{ctx}: {err}");
        assert_eq!(fs::read_to_string(&dst).unwrap(), "recreated", "{ctx}");
        assert_eq!(fs::read_to_string(&src).unwrap(), "restored", "{ctx}");
        assert_eq!(staged_left(dst.parent().unwrap()), 0, "{ctx}");
    }
}

/// Review #2168 #1: a sibling that happens to use a staging-like name is not
/// ours — neither on success nor when the publish fails and cleanup runs.
#[test]
fn cross_device_move_leaves_partial_named_siblings_alone() {
    for kind in ["file", "dir", "dangling"] {
        for publish_fails in [false, true] {
            let t = tempfile::tempdir().unwrap();
            let (src, dst) = (t.path().join("trashed"), t.path().join("work/report"));
            fs::write(&src, "restored").unwrap();
            fs::create_dir(t.path().join("work")).unwrap();
            let sibling = t.path().join("work/report.partial");
            occupy(&sibling, kind);

            let out = move_with(&src, &dst, |a, b| {
                if publish_fails {
                    fs::write(b, "recreated").unwrap();
                }
                exdev(a, b)
            });

            let ctx = format!("{kind} publish_fails={publish_fails}");
            assert_occupied(&sibling, kind);
            if publish_fails {
                assert!(out.is_err(), "{ctx}");
                assert_eq!(fs::read_to_string(&dst).unwrap(), "recreated", "{ctx}");
            } else {
                assert!(matches!(out.unwrap(), MoveOutcome::Moved), "{ctx}");
                assert_eq!(fs::read_to_string(&dst).unwrap(), "restored", "{ctx}");
            }
            assert_eq!(staged_left(&t.path().join("work")), 0, "{ctx}");
        }
    }
}

fn staged_left(dir: &Path) -> usize {
    fs::read_dir(dir)
        .unwrap()
        .filter(|e| {
            let name = e.as_ref().unwrap().file_name();
            name.to_string_lossy().contains(RESTORE_STAGING_PREFIX)
        })
        .count()
}

/// The race window is closed by the kernel, which a test cannot interleave
/// with. So pin that, where the kernel has it, the no-replace rename really
/// is the kernel's atomic one — not a check followed by `fs::rename`.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn kernel_noreplace_rename_is_used_where_available() {
    let t = tempfile::tempdir().unwrap();
    let (src, dst) = (t.path().join("a"), t.path().join("b"));
    fs::write(&src, "ours").unwrap();
    fs::write(&dst, "theirs").unwrap();

    let err = crate::publish::native(&src, &dst)
        .expect("this platform has a kernel no-replace rename")
        .unwrap_err();

    assert_eq!(err.raw_os_error(), Some(libc::EEXIST), "{err}");
    assert_eq!(fs::read_to_string(&dst).unwrap(), "theirs");
    rename_noreplace(&src, &t.path().join("c")).unwrap();
    assert!(!src.exists());
}

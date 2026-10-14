//! Head file I/O (spec 7.3.1 steps 2-4): where the head lives, that a write
//! replaces atomically, and that every failure is an `Err`, never a silent
//! half-written head.

use chrono::{TimeZone, Utc};
use mur_channel::head::{Head, HeadFields};
use mur_channel::head_io::{head_path, read_head, write_head};
use mur_common::identity::AgentIdentity;

fn head(seq: u64) -> Head {
    Head::seal(
        &AgentIdentity::generate(),
        HeadFields {
            channel_id: "chan-1".into(),
            seq,
            lines: seq,
            chain_hash: "aa".repeat(32),
            key_version: None,
            legacy_through: None,
        },
        Utc.with_ymd_and_hms(2026, 10, 14, 0, 0, 0).unwrap(),
    )
}

#[test]
fn path_is_dir_slash_channel_id_dot_head() {
    let p = head_path(std::path::Path::new("/h"), "chan-1").unwrap();
    assert_eq!(p, std::path::Path::new("/h/chan-1.head"));
}

#[test]
fn unsafe_channel_ids_are_refused() {
    for bad in ["", ".", "..", "a/b", "a\\b", "../x", "a\0b"] {
        assert!(
            head_path(std::path::Path::new("/h"), bad).is_err(),
            "{bad:?}"
        );
    }
}

#[test]
fn read_of_a_missing_head_is_none() {
    let dir = tempfile::tempdir().unwrap();
    assert!(read_head(dir.path(), "chan-1").unwrap().is_none());
    // Also when the heads directory itself does not exist yet.
    assert!(
        read_head(&dir.path().join("channel-heads"), "chan-1")
            .unwrap()
            .is_none()
    );
}

#[test]
fn first_write_creates_the_directory_and_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let heads = dir.path().join("channel-heads");
    let h = head(3);
    write_head(&heads, &h).unwrap();
    assert_eq!(read_head(&heads, "chan-1").unwrap(), Some(h));
}

#[test]
fn a_second_write_replaces_the_first() {
    let dir = tempfile::tempdir().unwrap();
    write_head(dir.path(), &head(1)).unwrap();
    write_head(dir.path(), &head(2)).unwrap();
    assert_eq!(read_head(dir.path(), "chan-1").unwrap().unwrap().seq, 2);
}

#[test]
fn a_successful_write_leaves_no_temp_file() {
    let dir = tempfile::tempdir().unwrap();
    write_head(dir.path(), &head(1)).unwrap();
    let names: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(names, vec!["chan-1.head".to_string()]);
}

#[test]
fn a_stale_temp_from_a_crash_does_not_block_the_write() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("chan-1.head.tmp"), b"half a hea").unwrap();
    write_head(dir.path(), &head(5)).unwrap();
    assert_eq!(read_head(dir.path(), "chan-1").unwrap().unwrap().seq, 5);
}

#[test]
fn a_corrupt_head_is_an_error_not_none() {
    // "No head" and "unreadable head" are different verdicts (NoHead vs a
    // forged/damaged head); collapsing them would hide tampering.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("chan-1.head"), b"{not json").unwrap();
    assert!(read_head(dir.path(), "chan-1").is_err());
}

#[test]
fn heads_dir_that_is_a_file_fails_the_write() {
    let dir = tempfile::tempdir().unwrap();
    let blocker = dir.path().join("channel-heads");
    std::fs::write(&blocker, b"i am a file").unwrap();
    assert!(write_head(&blocker, &head(1)).is_err());
}

#[test]
fn a_failed_rename_is_an_error_and_cleans_up_its_temp() {
    // A directory squatting on the head path makes the final rename fail.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("chan-1.head")).unwrap();
    let err = write_head(dir.path(), &head(1)).unwrap_err();
    assert!(format!("{err:#}").contains("rename"), "{err:#}");
    assert!(!dir.path().join("chan-1.head.tmp").exists());
}

// ---- crash recovery: the on-disk states a crash can leave behind ----------
//
// `write_head` is temp write + fsync, rename, dir fsync. A crash lands between
// two steps; the disk then holds one of the states below, and a reader must
// see either the old head or the new one, never a hybrid.

fn plant_tmp(dir: &std::path::Path, h: &Head) {
    std::fs::write(dir.join("chan-1.head.tmp"), serde_json::to_vec(h).unwrap()).unwrap();
}

#[test]
fn crash_after_temp_fsync_before_rename_keeps_the_old_head() {
    // The temp holds a complete, valid newer head, but the rename never ran.
    // The reader must keep returning the sealed head, not the unpublished one.
    let dir = tempfile::tempdir().unwrap();
    write_head(dir.path(), &head(1)).unwrap();
    plant_tmp(dir.path(), &head(2));

    assert_eq!(read_head(dir.path(), "chan-1").unwrap().unwrap().seq, 1);
}

#[test]
fn the_write_after_such_a_crash_publishes_and_clears_the_temp() {
    let dir = tempfile::tempdir().unwrap();
    write_head(dir.path(), &head(1)).unwrap();
    plant_tmp(dir.path(), &head(2));

    write_head(dir.path(), &head(3)).unwrap();

    assert_eq!(read_head(dir.path(), "chan-1").unwrap().unwrap().seq, 3);
    assert!(!dir.path().join("chan-1.head.tmp").exists());
}

#[test]
fn crash_mid_first_write_reads_as_no_head_not_as_damage() {
    // First seal of a channel crashed with a torn temp and no head file: the
    // channel simply has no head yet. Reporting damage here would make a crash
    // look like tampering.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("chan-1.head.tmp"), b"{\"v\":1,\"chan").unwrap();

    assert!(read_head(dir.path(), "chan-1").unwrap().is_none());
}

#[test]
fn a_failed_temp_create_leaves_the_old_head_untouched() {
    // A directory squatting on the temp name makes step 1 fail. A write that
    // went straight to the head file would succeed here; the temp+rename order
    // must instead fail and leave the sealed head exactly as it was.
    let dir = tempfile::tempdir().unwrap();
    write_head(dir.path(), &head(1)).unwrap();
    let before = std::fs::read(dir.path().join("chan-1.head")).unwrap();
    std::fs::create_dir(dir.path().join("chan-1.head.tmp")).unwrap();

    let err = write_head(dir.path(), &head(2)).unwrap_err();

    assert!(format!("{err:#}").contains("temp"), "{err:#}");
    assert_eq!(
        std::fs::read(dir.path().join("chan-1.head")).unwrap(),
        before
    );
    assert_eq!(read_head(dir.path(), "chan-1").unwrap().unwrap().seq, 1);
}

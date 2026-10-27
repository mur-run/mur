//! `trash restore` (#2158): no overwrite, retention-wide lookup, cross-device.

use super::{find_entry, lookup_days, restore_entry};
use chrono::{Local, Utc};
use mur_common::action::{ActionEvent, DeletionConfig, RestoreMeta, TrashEntry, TrashStatus};
use mur_trash::MoveOutcome;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use uuid::Uuid;

fn exdev(_: &Path, _: &Path) -> io::Result<()> {
    Err(io::Error::from(io::ErrorKind::CrossesDevices))
}

fn entry(original: PathBuf, trash: PathBuf) -> TrashEntry {
    TrashEntry {
        id: Uuid::new_v4(),
        original_path: original,
        trash_path: Some(trash),
        file_size_bytes: 1,
        created_by_task_id: Uuid::new_v4(),
        created_at: Utc::now(),
        execute_at: Utc::now(),
        retention_until: None,
        status: TrashStatus::Retained,
        restore_metadata: RestoreMeta {
            owner_uid: 0,
            owner_gid: 0,
            permissions: 0o644,
        },
    }
}

/// `<tmp>/work/<name>` and its trashed copy under `<tmp>/trash/<id>/<name>`.
fn trashed(tmp: &Path, name: &str, body: &str) -> TrashEntry {
    let e = entry(tmp.join("work").join(name), PathBuf::new());
    let trash = tmp.join("trash").join(e.id.to_string()).join(name);
    fs::create_dir_all(trash.parent().unwrap()).unwrap();
    fs::create_dir_all(tmp.join("work")).unwrap();
    fs::write(&trash, body).unwrap();
    TrashEntry {
        trash_path: Some(trash),
        ..e
    }
}

#[test]
fn refuses_when_original_path_exists() {
    let tmp = tempfile::tempdir().unwrap();
    let e = trashed(tmp.path(), "notes.md", "old");
    fs::write(&e.original_path, "recreated").unwrap();

    let err = restore_entry(&e, mur_trash::move_across).unwrap_err();

    assert!(err.to_string().contains("not overwriting"), "{err}");
    assert_eq!(fs::read_to_string(&e.original_path).unwrap(), "recreated");
    assert!(e.trash_path.as_ref().unwrap().exists(), "trash copy kept");
}

#[test]
fn refuses_over_a_dangling_link() {
    // `Path::exists` follows links and says false here; the check must not.
    let tmp = tempfile::tempdir().unwrap();
    let e = trashed(tmp.path(), "a.txt", "old");
    std::os::unix::fs::symlink(tmp.path().join("nowhere"), &e.original_path).unwrap();

    let err = restore_entry(&e, mur_trash::move_across).unwrap_err();
    assert!(err.to_string().contains("not overwriting"), "{err}");
    assert!(
        fs::symlink_metadata(&e.original_path)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn restores_across_devices() {
    let tmp = tempfile::tempdir().unwrap();
    let e = trashed(tmp.path(), "report.txt", "body");

    let out = restore_entry(&e, |a, b| mur_trash::move_with(a, b, exdev)).unwrap();

    assert!(matches!(out, MoveOutcome::Moved));
    assert_eq!(fs::read_to_string(&e.original_path).unwrap(), "body");
    assert!(!e.trash_path.as_ref().unwrap().exists());
}

#[test]
fn restores_a_directory_into_a_missing_parent() {
    let tmp = tempfile::tempdir().unwrap();
    let mut e = trashed(tmp.path(), "placeholder", "");
    let trash_dir = e.trash_path.as_ref().unwrap().with_file_name("build");
    fs::create_dir_all(trash_dir.join("sub")).unwrap();
    fs::write(trash_dir.join("sub/a.o"), "x").unwrap();
    e.trash_path = Some(trash_dir);
    e.original_path = tmp.path().join("gone/parent/build");

    restore_entry(&e, |a, b| mur_trash::move_with(a, b, exdev)).unwrap();

    assert_eq!(
        fs::read_to_string(e.original_path.join("sub/a.o")).unwrap(),
        "x"
    );
}

#[test]
fn missing_trash_copy_is_an_error_not_a_restore() {
    let tmp = tempfile::tempdir().unwrap();
    let e = trashed(tmp.path(), "x", "");
    fs::remove_file(e.trash_path.as_ref().unwrap()).unwrap();

    let err = restore_entry(&e, mur_trash::move_across).unwrap_err();
    assert!(err.to_string().contains("not found"), "{err}");
}

#[test]
fn lookup_covers_retention_beyond_thirty_days() {
    let deletion = |days| DeletionConfig {
        trash_retention_days: days,
        ..Default::default()
    };
    assert!(
        lookup_days(&deletion(7)) >= 30,
        "never shorter than the old window"
    );
    assert!(lookup_days(&deletion(90)) > 90);
    assert!(lookup_days(&deletion(u32::MAX)) == u32::MAX, "no overflow");
}

#[test]
fn finds_an_entry_written_forty_days_ago() {
    let tmp = tempfile::tempdir().unwrap();
    let ledger = tmp.path().join("ledger");
    fs::create_dir_all(&ledger).unwrap();
    let e = entry(tmp.path().join("a"), tmp.path().join("t/a"));
    let day = Local::now().date_naive() - chrono::Duration::days(40);
    let line = serde_json::to_string(&ActionEvent::TrashCreated { entry: e.clone() }).unwrap();
    fs::write(
        ledger.join(format!("{}.jsonl", day.format("%Y-%m-%d"))),
        line + "\n",
    )
    .unwrap();

    let short = DeletionConfig {
        trash_retention_days: 30,
        ..Default::default()
    };
    let long = DeletionConfig {
        trash_retention_days: 60,
        ..Default::default()
    };

    assert!(
        find_entry(&ledger, &short, e.id).is_err(),
        "outside a 30-day retention"
    );
    assert_eq!(find_entry(&ledger, &long, e.id).unwrap().id, e.id);
}

/// Review #2168 #2: a file recreated after the up-front check — even during a
/// long cross-device copy — is refused, not replaced; the trash copy stays.
#[test]
fn file_recreated_mid_restore_is_not_replaced() {
    for cross_device in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        let e = trashed(tmp.path(), "notes.md", "old");

        let err = restore_entry(&e, |a, b| {
            mur_trash::move_with(a, b, |a, b| {
                fs::write(b, "recreated").unwrap(); // after restore_entry's check
                if cross_device {
                    exdev(a, b)
                } else {
                    mur_trash::rename_noreplace(a, b)
                }
            })
        })
        .unwrap_err();

        assert!(err.to_string().contains("not overwriting"), "{err}");
        assert_eq!(fs::read_to_string(&e.original_path).unwrap(), "recreated");
        assert_eq!(
            fs::read_to_string(e.trash_path.as_ref().unwrap()).unwrap(),
            "old"
        );
    }
}

/// Review #2168 #1: an unrelated `<name>.partial` beside the original path
/// is neither overwritten nor deleted by a cross-device restore.
#[test]
fn cross_device_restore_leaves_a_partial_named_sibling_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let e = trashed(tmp.path(), "report", "body");
    let sibling = tmp.path().join("work/report.partial");
    fs::create_dir(&sibling).unwrap();
    fs::write(sibling.join("keep"), "theirs").unwrap();

    restore_entry(&e, |a, b| mur_trash::move_with(a, b, exdev)).unwrap();

    assert_eq!(fs::read_to_string(&e.original_path).unwrap(), "body");
    assert_eq!(fs::read_to_string(sibling.join("keep")).unwrap(), "theirs");
    assert_eq!(fs::read_dir(tmp.path().join("work")).unwrap().count(), 2);
}

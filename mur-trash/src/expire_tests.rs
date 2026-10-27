//! Retention expiry deletes once, inside the trash only (#2174).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{Duration as ChronoDuration, Local, Utc};
use mur_common::action::{ActionEvent, DeletionConfig, RestoreMeta, TrashEntry, TrashStatus};
use mur_common::ledger::Ledger;
use uuid::Uuid;

use crate::expire::{ExpireReport, expire_due, lookup_days};
use crate::layout::{entry_path, ledger_dir, trash_root};

fn deletion() -> DeletionConfig {
    mur_common::action::ActionPipelineConfig::default().deletion
}

fn home() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().canonicalize().unwrap().join("agent");
    fs::create_dir_all(&home).unwrap();
    (tmp, home)
}

/// A retained entry for a file `name` placed in the trash, created
/// `age_days` ago under the default retention.
fn trashed(home: &Path, name: &str, age_days: i64) -> TrashEntry {
    let id = Uuid::new_v4();
    let dst = entry_path(&trash_root(home), id, Path::new(name)).unwrap();
    fs::create_dir_all(dst.parent().unwrap()).unwrap();
    fs::write(&dst, "body").unwrap();
    let created = Utc::now() - ChronoDuration::days(age_days);
    TrashEntry {
        id,
        original_path: home.join(name),
        trash_path: Some(dst),
        file_size_bytes: 4,
        created_by_task_id: Uuid::nil(),
        created_at: created,
        execute_at: created,
        retention_until: Some(
            created + ChronoDuration::days(deletion().trash_retention_days.into()),
        ),
        status: TrashStatus::Retained,
        restore_metadata: RestoreMeta {
            owner_uid: 0,
            owner_gid: 0,
            permissions: 0,
        },
    }
}

/// Write `event` into the day file `age_days` back, as the ledger would have
/// on that day — the 7-day replay this replaces never read such files.
fn record_on_day(home: &Path, age_days: i64, event: &ActionEvent) {
    let dir = ledger_dir(home);
    fs::create_dir_all(&dir).unwrap();
    let day = Local::now().date_naive() - ChronoDuration::days(age_days);
    let path = dir.join(format!("{}.jsonl", day.format("%Y-%m-%d")));
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    writeln!(f, "{}", serde_json::to_string(event).unwrap()).unwrap();
}

fn expired_records(home: &Path) -> Vec<Uuid> {
    Ledger::<ActionEvent>::scan_days(&ledger_dir(home), lookup_days(&deletion()))
        .into_iter()
        .filter_map(|r| match r.ok()? {
            ActionEvent::TrashExpired { entry_id } => Some(entry_id),
            _ => None,
        })
        .collect()
}

#[test]
fn entry_31_days_old_is_deleted_under_default_config() {
    let (_t, home) = home();
    let entry = trashed(&home, "old.txt", 31);
    let file = entry.trash_path.clone().unwrap();
    record_on_day(
        &home,
        31,
        &ActionEvent::TrashCreated {
            entry: entry.clone(),
        },
    );

    let report = expire_due(&home, &deletion(), Utc::now()).unwrap();

    assert_eq!(report.expired, vec![entry.id]);
    assert!(report.failed.is_empty());
    assert!(fs::symlink_metadata(&file).is_err(), "file deleted");
    assert!(!file.parent().unwrap().exists(), "empty entry dir removed");
    assert!(trash_root(&home).exists(), "trash root kept");
    assert_eq!(expired_records(&home), vec![entry.id]);
}

#[test]
fn second_pass_is_a_no_op() {
    let (_t, home) = home();
    let entry = trashed(&home, "old.txt", 31);
    record_on_day(
        &home,
        31,
        &ActionEvent::TrashCreated {
            entry: entry.clone(),
        },
    );

    expire_due(&home, &deletion(), Utc::now()).unwrap();
    let again = expire_due(&home, &deletion(), Utc::now()).unwrap();

    assert_eq!(again, ExpireReport::default());
    assert_eq!(expired_records(&home).len(), 1, "recorded once");
}

#[test]
fn entry_inside_retention_is_kept() {
    let (_t, home) = home();
    let entry = trashed(&home, "fresh.txt", 3);
    let file = entry.trash_path.clone().unwrap();
    record_on_day(&home, 3, &ActionEvent::TrashCreated { entry });

    let report = expire_due(&home, &deletion(), Utc::now()).unwrap();

    assert_eq!(report, ExpireReport::default());
    assert!(file.exists());
    assert!(expired_records(&home).is_empty());
}

#[test]
fn restored_or_deleted_entries_are_not_expired_again() {
    let (_t, home) = home();
    let restored = trashed(&home, "a.txt", 31);
    let purged = trashed(&home, "b.txt", 31);
    for e in [&restored, &purged] {
        record_on_day(&home, 31, &ActionEvent::TrashCreated { entry: e.clone() });
    }
    record_on_day(
        &home,
        5,
        &ActionEvent::TrashRestored {
            entry_id: restored.id,
        },
    );
    record_on_day(
        &home,
        5,
        &ActionEvent::TrashPermDeleted {
            entry_id: purged.id,
            reason: mur_common::action::PermDeleteReason::UserNow,
        },
    );

    let report = expire_due(&home, &deletion(), Utc::now()).unwrap();

    assert_eq!(report, ExpireReport::default());
    assert!(expired_records(&home).is_empty());
}

#[test]
fn symlink_in_entry_is_removed_not_followed() {
    let (_t, home) = home();
    let outside = home.join("keep");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("precious.txt"), "keep me").unwrap();

    let mut entry = trashed(&home, "link", 31);
    let link = entry.trash_path.clone().unwrap();
    fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    entry.trash_path = Some(link.clone());
    record_on_day(
        &home,
        31,
        &ActionEvent::TrashCreated {
            entry: entry.clone(),
        },
    );

    let report = expire_due(&home, &deletion(), Utc::now()).unwrap();

    assert_eq!(report.expired, vec![entry.id]);
    assert!(fs::symlink_metadata(&link).is_err(), "link removed");
    assert_eq!(
        fs::read_to_string(outside.join("precious.txt")).unwrap(),
        "keep me"
    );
}

#[test]
fn path_outside_trash_is_refused_and_retried() {
    let (_t, home) = home();
    let victim = home.join("victim.txt");
    fs::write(&victim, "keep me").unwrap();
    let mut entry = trashed(&home, "x.txt", 31);
    entry.trash_path = Some(victim.clone()); // tampered ledger
    record_on_day(
        &home,
        31,
        &ActionEvent::TrashCreated {
            entry: entry.clone(),
        },
    );

    let report = expire_due(&home, &deletion(), Utc::now()).unwrap();

    assert!(report.expired.is_empty());
    assert_eq!(report.failed.len(), 1);
    assert_eq!(report.failed[0].0, entry.id);
    assert!(victim.exists(), "nothing outside the trash deleted");
    assert!(
        expired_records(&home).is_empty(),
        "not recorded until deleted"
    );
}

#[test]
fn already_missing_file_still_expires() {
    let (_t, home) = home();
    let entry = trashed(&home, "gone.txt", 31);
    fs::remove_file(entry.trash_path.as_ref().unwrap()).unwrap();
    record_on_day(
        &home,
        31,
        &ActionEvent::TrashCreated {
            entry: entry.clone(),
        },
    );

    let report = expire_due(&home, &deletion(), Utc::now()).unwrap();

    assert_eq!(report.expired, vec![entry.id]);
}

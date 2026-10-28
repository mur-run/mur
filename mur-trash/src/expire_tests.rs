//! Retention expiry deletes once, inside the trash only (#2174).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{Duration as ChronoDuration, Local, Utc};
use mur_common::action::{ActionEvent, DeletionConfig, RestoreMeta, TrashEntry, TrashStatus};
use mur_common::ledger::Ledger;
use uuid::Uuid;

use crate::expire::{ExpireReport, expire_due, expiry_cursor_path, lookup_days, retained_bytes};
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
    Ledger::<ActionEvent>::scan_since(&ledger_dir(home), None)
        .events
        .into_iter()
        .filter_map(|(_, e)| match e {
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

/// Review #2176 P1: a backlog entry created long before any replay window
/// ending today (pre-fix backlog, daemon offline for weeks) still expires.
#[test]
fn entry_older_than_the_lookup_window_still_expires() {
    let (_t, home) = home();
    let age = i64::from(lookup_days(&deletion())) + 30;
    let entry = trashed(&home, "ancient.txt", age);
    let file = entry.trash_path.clone().unwrap();
    record_on_day(
        &home,
        age,
        &ActionEvent::TrashCreated {
            entry: entry.clone(),
        },
    );

    let report = expire_due(&home, &deletion(), Utc::now()).unwrap();

    assert_eq!(report.expired, vec![entry.id]);
    assert!(fs::symlink_metadata(&file).is_err(), "file deleted");
    assert_eq!(expired_records(&home), vec![entry.id]);
}

/// Review #2176 P1: a delete that keeps failing until its creation day is
/// past the lookup window is still retried, and succeeds once it can.
#[test]
fn failed_delete_is_retried_after_creation_leaves_the_window() {
    use std::os::unix::fs::PermissionsExt;
    let (_t, home) = home();
    let entry = trashed(&home, "stuck.txt", 31);
    let file = entry.trash_path.clone().unwrap();
    let entry_dir = file.parent().unwrap().to_path_buf();
    record_on_day(
        &home,
        31,
        &ActionEvent::TrashCreated {
            entry: entry.clone(),
        },
    );

    // Read-only entry dir: unlinking the file inside fails.
    fs::set_permissions(&entry_dir, fs::Permissions::from_mode(0o555)).unwrap();
    let first = expire_due(&home, &deletion(), Utc::now()).unwrap();
    fs::set_permissions(&entry_dir, fs::Permissions::from_mode(0o755)).unwrap();
    if first.failed.is_empty() {
        return; // running as root: permissions do not block, nothing to test
    }
    assert!(first.expired.is_empty());
    assert!(
        expired_records(&home).is_empty(),
        "not recorded until deleted"
    );

    // Much later: the creation day is now far outside `lookup_days`.
    let later = Utc::now() + ChronoDuration::days(i64::from(lookup_days(&deletion())) * 2);
    let retry = expire_due(&home, &deletion(), later).unwrap();

    assert_eq!(retry.expired, vec![entry.id]);
    assert!(
        fs::symlink_metadata(&file).is_err(),
        "file deleted on retry"
    );
    assert_eq!(expired_records(&home), vec![entry.id]);
}

/// The cursor never skips an entry still in retention, and advances past
/// days whose entries are all settled.
#[test]
fn cursor_holds_at_outstanding_entries_and_advances_past_settled_ones() {
    let (_t, home) = home();
    let old = trashed(&home, "old.txt", 100);
    record_on_day(
        &home,
        100,
        &ActionEvent::TrashCreated { entry: old.clone() },
    );
    let mut kept = trashed(&home, "kept.txt", 60);
    kept.retention_until = Some(Utc::now() + ChronoDuration::days(365));
    record_on_day(
        &home,
        60,
        &ActionEvent::TrashCreated {
            entry: kept.clone(),
        },
    );

    let report = expire_due(&home, &deletion(), Utc::now()).unwrap();
    assert_eq!(report.expired, vec![old.id]);

    let cursor = fs::read_to_string(expiry_cursor_path(&home)).unwrap();
    let day_60 = Local::now().date_naive() - ChronoDuration::days(60);
    assert_eq!(cursor.trim(), day_60.format("%Y-%m-%d").to_string());
    assert!(
        kept.trash_path.as_ref().unwrap().exists(),
        "in retention: kept"
    );
}

/// A corrupt cursor falls back to a full replay rather than skipping days.
#[test]
fn unreadable_cursor_replays_everything() {
    let (_t, home) = home();
    let age = i64::from(lookup_days(&deletion())) + 10;
    let entry = trashed(&home, "x.txt", age);
    record_on_day(
        &home,
        age,
        &ActionEvent::TrashCreated {
            entry: entry.clone(),
        },
    );
    let cursor = expiry_cursor_path(&home);
    fs::create_dir_all(cursor.parent().unwrap()).unwrap();
    fs::write(&cursor, "not a date").unwrap();

    let report = expire_due(&home, &deletion(), Utc::now()).unwrap();

    assert_eq!(report.expired, vec![entry.id]);
}

/// Review #2176 P2: an old day file unreadable during one pass must not let
/// the cursor skip it; once access is restored a later pass expires its entry.
#[cfg(unix)]
#[test]
fn unreadable_day_file_is_not_skipped_by_the_cursor() {
    use std::os::unix::fs::PermissionsExt;
    let (_t, home) = home();
    let entry = trashed(&home, "old.txt", 100);
    record_on_day(
        &home,
        100,
        &ActionEvent::TrashCreated {
            entry: entry.clone(),
        },
    );
    let day = Local::now().date_naive() - ChronoDuration::days(100);
    let day_file = ledger_dir(&home).join(format!("{}.jsonl", day.format("%Y-%m-%d")));

    fs::set_permissions(&day_file, fs::Permissions::from_mode(0o000)).unwrap();
    let first = expire_due(&home, &deletion(), Utc::now()).unwrap();
    fs::set_permissions(&day_file, fs::Permissions::from_mode(0o644)).unwrap();
    if first.expired == vec![entry.id] {
        return; // running as root: permissions do not block reads
    }
    assert_eq!(first, ExpireReport::default(), "entry unseen this pass");
    let cursor = fs::read_to_string(expiry_cursor_path(&home)).unwrap();
    assert!(
        cursor.trim() <= day.format("%Y-%m-%d").to_string().as_str(),
        "cursor pinned at or before the unreadable day, got {cursor}"
    );

    let second = expire_due(&home, &deletion(), Utc::now()).unwrap();
    assert_eq!(second.expired, vec![entry.id]);
    assert!(fs::symlink_metadata(entry.trash_path.as_ref().unwrap()).is_err());
    assert_eq!(expired_records(&home), vec![entry.id]);
}

/// Review #2176 P2: an unreadable day file may hold the entry's existing
/// `TrashExpired`. Acting on that partial history would record it twice, so
/// a pass over an incomplete scan deletes and records nothing.
#[cfg(unix)]
#[test]
fn unreadable_settlement_day_does_not_duplicate_the_expiry_record() {
    use std::os::unix::fs::PermissionsExt;
    let (_t, home) = home();
    let entry = trashed(&home, "old.txt", 100);
    record_on_day(
        &home,
        100,
        &ActionEvent::TrashCreated {
            entry: entry.clone(),
        },
    );
    // Already expired by an earlier pass: files gone, record on day 60.
    fs::remove_file(entry.trash_path.as_ref().unwrap()).unwrap();
    record_on_day(&home, 60, &ActionEvent::TrashExpired { entry_id: entry.id });
    let day = Local::now().date_naive() - ChronoDuration::days(60);
    let day_file = ledger_dir(&home).join(format!("{}.jsonl", day.format("%Y-%m-%d")));

    fs::set_permissions(&day_file, fs::Permissions::from_mode(0o000)).unwrap();
    let blind = expire_due(&home, &deletion(), Utc::now()).unwrap();
    fs::set_permissions(&day_file, fs::Permissions::from_mode(0o644)).unwrap();

    assert_eq!(
        blind,
        ExpireReport::default(),
        "no action on partial history"
    );
    let again = expire_due(&home, &deletion(), Utc::now()).unwrap();
    assert_eq!(again, ExpireReport::default(), "already settled");
    assert_eq!(expired_records(&home), vec![entry.id], "recorded once");
}

#[test]
fn retained_bytes_counts_open_entries_only() {
    let (_t, home) = home();
    let mut old = trashed(&home, "old.txt", 20); // past the old 7-day replay
    old.file_size_bytes = 1000;
    let mut gone = trashed(&home, "gone.txt", 2);
    gone.file_size_bytes = 500;
    record_on_day(&home, 20, &ActionEvent::TrashCreated { entry: old });
    record_on_day(
        &home,
        2,
        &ActionEvent::TrashCreated {
            entry: gone.clone(),
        },
    );
    record_on_day(&home, 1, &ActionEvent::TrashRestored { entry_id: gone.id });

    assert_eq!(retained_bytes(&home), 1000);
}

#[test]
fn expiry_frees_its_bytes() {
    let (_t, home) = home();
    let mut entry = trashed(&home, "old.txt", 31);
    entry.file_size_bytes = 4096;
    record_on_day(&home, 31, &ActionEvent::TrashCreated { entry });
    assert_eq!(retained_bytes(&home), 4096);

    expire_due(&home, &deletion(), Utc::now()).unwrap();

    assert_eq!(retained_bytes(&home), 0);
}

/// Review #2177 P1: an open entry whose creation day is past the lookup
/// window still occupies the trash, so it still counts.
#[test]
fn retained_bytes_counts_an_open_entry_older_than_the_lookup_window() {
    let (_t, home) = home();
    let age = i64::from(lookup_days(&deletion())) + 30;
    let mut old = trashed(&home, "old.txt", age);
    old.retention_until = Some(Utc::now() + ChronoDuration::days(365));
    old.file_size_bytes = 900 * 1024;
    record_on_day(&home, age, &ActionEvent::TrashCreated { entry: old });

    assert_eq!(retained_bytes(&home), 900 * 1024);
    // An expiry pass keeps the cursor at the open entry, so it still counts.
    expire_due(&home, &deletion(), Utc::now()).unwrap();
    assert_eq!(retained_bytes(&home), 900 * 1024);
}

/// Review #2177 P1: shortening retention does not hide entries trashed under
/// the longer one; they stay counted until they actually leave.
#[test]
fn retained_bytes_survives_a_shortened_retention() {
    let (_t, home) = home();
    let mut kept = trashed(&home, "kept.txt", 80);
    kept.retention_until = Some(Utc::now() + ChronoDuration::days(10));
    kept.file_size_bytes = 2048;
    record_on_day(&home, 80, &ActionEvent::TrashCreated { entry: kept });
    let short = DeletionConfig {
        trash_retention_days: 1,
        ..deletion()
    };

    let report = expire_due(&home, &short, Utc::now()).unwrap();

    assert!(report.expired.is_empty(), "still within its own retention");
    assert_eq!(retained_bytes(&home), 2048);
}

/// Review #2177 P2: a creation record in a day file that cannot be read must
/// not read as free space; its bytes still on disk are counted.
#[cfg(unix)]
#[test]
fn retained_bytes_does_not_trust_a_replay_with_an_unreadable_day() {
    use std::os::unix::fs::PermissionsExt;
    let (_t, home) = home();
    let mut old = trashed(&home, "old.bin", 10);
    let body = vec![0u8; 64 * 1024];
    fs::write(old.trash_path.as_ref().unwrap(), &body).unwrap();
    old.file_size_bytes = body.len() as u64;
    record_on_day(&home, 10, &ActionEvent::TrashCreated { entry: old });
    let day = Local::now().date_naive() - ChronoDuration::days(10);
    let day_file = ledger_dir(&home).join(format!("{}.jsonl", day.format("%Y-%m-%d")));
    assert_eq!(retained_bytes(&home), body.len() as u64);

    fs::set_permissions(&day_file, fs::Permissions::from_mode(0o000)).unwrap();
    let used = retained_bytes(&home);
    fs::set_permissions(&day_file, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(
        used >= body.len() as u64,
        "unreadable record must not become free space, got {used}"
    );
}

/// Review #2177 P2: a malformed line may be a lost creation record, so the
/// ledger sum is not trusted and the bytes on disk still count.
#[test]
fn retained_bytes_does_not_trust_a_replay_with_a_malformed_line() {
    let (_t, home) = home();
    let entry = trashed(&home, "kept.bin", 3);
    let body = vec![0u8; 32 * 1024];
    fs::write(entry.trash_path.as_ref().unwrap(), &body).unwrap();
    let dir = ledger_dir(&home);
    fs::create_dir_all(&dir).unwrap();
    let day = Local::now().date_naive() - ChronoDuration::days(3);
    // The creation record was torn: present on disk, unparseable.
    fs::write(
        dir.join(format!("{}.jsonl", day.format("%Y-%m-%d"))),
        "{\"TrashCreated\":{\"entry\":{\"id\":\n",
    )
    .unwrap();

    assert!(retained_bytes(&home) >= body.len() as u64);
}

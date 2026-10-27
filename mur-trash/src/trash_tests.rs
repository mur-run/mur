//! Tests for D7–D8: `trash_into` rules, capacity, and ledger records.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use mur_common::action::{ActionEvent, DeletionConfig, PermDeleteReason, TrashStatus};
use mur_common::ledger::Ledger;
use uuid::Uuid;

use crate::detect::{Rule, TrashTarget};
use crate::layout::ledger_dir;
use crate::trash::{TrashOutcome, record_unprotected, trash_into};

const RM: Rule = Rule::Rm {
    recursive: false,
    force: false,
    empty_dirs: false,
};
const RM_RF: Rule = Rule::Rm {
    recursive: true,
    force: true,
    empty_dirs: false,
};

struct Home {
    _t: tempfile::TempDir,
    home: PathBuf,
    work: PathBuf,
}

fn home() -> Home {
    let t = tempfile::tempdir().unwrap();
    let base = t.path().canonicalize().unwrap();
    let home = base.join("agent");
    let work = base.join("work");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&work).unwrap();
    Home { _t: t, home, work }
}

fn target(path: PathBuf, rule: Rule) -> TrashTarget {
    TrashTarget {
        spelled: path.display().to_string(),
        path,
        rule,
        segment: 0,
    }
}

fn events(home: &Path) -> Vec<ActionEvent> {
    Ledger::<ActionEvent>::scan_days(&ledger_dir(home), 1)
        .into_iter()
        .map(|r| r.unwrap())
        .collect()
}

fn run(h: &Home, targets: &[TrashTarget], cfg: &DeletionConfig) -> Vec<TrashOutcome> {
    trash_into(&h.home, targets, cfg, Uuid::nil())
}

#[test]
fn file_is_moved_and_recorded_restorably() {
    let h = home();
    let f = h.work.join("a.txt");
    fs::write(&f, "hello").unwrap();

    let out = run(&h, &[target(f.clone(), RM)], &DeletionConfig::default());
    let TrashOutcome::Trashed {
        entry_id,
        trash_path,
        ..
    } = &out[0]
    else {
        panic!("{out:?}");
    };
    assert!(!f.exists());
    assert_eq!(fs::read_to_string(trash_path).unwrap(), "hello");

    let ev = events(&h.home);
    let [ActionEvent::TrashCreated { entry }] = ev.as_slice() else {
        panic!("{ev:?}");
    };
    assert_eq!(entry.id, *entry_id);
    assert_eq!(entry.original_path, f);
    assert_eq!(entry.trash_path.as_ref(), Some(trash_path));
    assert_eq!(entry.status, TrashStatus::Retained);
    assert_eq!(entry.file_size_bytes, 5);
    let days = (entry.retention_until.unwrap() - entry.created_at).num_days();
    assert_eq!(
        days,
        i64::from(DeletionConfig::default().trash_retention_days)
    );
}

#[test]
fn remover_rules_are_enforced_before_moving() {
    let h = home();
    let full = h.work.join("full");
    let empty = h.work.join("empty");
    let file = h.work.join("f");
    fs::create_dir_all(full.join("x")).unwrap();
    fs::create_dir(&empty).unwrap();
    fs::write(&file, "").unwrap();
    let rm_d = Rule::Rm {
        recursive: false,
        force: false,
        empty_dirs: true,
    };
    let cases = [
        (target(full.clone(), RM), false),   // rm dir
        (target(empty.clone(), RM), false),  // rm empty dir needs -d
        (target(full.clone(), rm_d), false), // rm -d nonempty
        (target(full.clone(), Rule::Rmdir), false),
        (target(file.clone(), Rule::Rmdir), false),
        (target(empty.clone(), Rule::Unlink), false),
        (target(h.work.join("missing"), RM), false),
    ];
    for (t, ok) in &cases {
        let out = run(&h, std::slice::from_ref(t), &DeletionConfig::default());
        assert_eq!(!out[0].is_failure(), *ok, "{t:?} -> {out:?}");
    }
    assert!(full.join("x").exists() && empty.exists() && file.exists());
    assert!(events(&h.home).is_empty(), "failures must not be recorded");

    let ok = run(
        &h,
        &[
            target(empty.clone(), Rule::Rmdir),
            target(full.clone(), RM_RF),
        ],
        &DeletionConfig::default(),
    );
    assert!(
        ok.iter().all(|o| matches!(o, TrashOutcome::Trashed { .. })),
        "{ok:?}"
    );
    assert!(!empty.exists() && !full.exists());
}

#[test]
fn force_ignores_missing_and_keeps_going() {
    let h = home();
    let real = h.work.join("real");
    fs::write(&real, "").unwrap();
    let out = run(
        &h,
        &[
            target(h.work.join("gone"), RM_RF),
            target(real.clone(), RM_RF),
        ],
        &DeletionConfig::default(),
    );
    assert!(matches!(out[0], TrashOutcome::MissingIgnored { .. }));
    assert!(matches!(out[1], TrashOutcome::Trashed { .. }));
    assert!(!real.exists());
}

#[test]
fn refused_targets_are_failures_and_untouched() {
    let h = home();
    let inside = h.home.join("trash").join("old");
    fs::create_dir_all(&inside).unwrap();
    let out = run(
        &h,
        &[target(inside.clone(), RM_RF), target(h.home.clone(), RM_RF)],
        &DeletionConfig::default(),
    );
    assert!(out.iter().all(TrashOutcome::is_failure), "{out:?}");
    assert!(inside.exists() && h.home.exists());
}

#[test]
fn link_is_trashed_not_its_target() {
    let h = home();
    let real = h.work.join("realdir");
    fs::create_dir(&real).unwrap();
    fs::write(real.join("keep"), "x").unwrap();
    let link = h.work.join("link");
    symlink(&real, &link).unwrap();
    // A link to a dir is not a dir: plain `rm` (no -r) removes it.
    let out = run(&h, &[target(link.clone(), RM)], &DeletionConfig::default());
    assert!(matches!(out[0], TrashOutcome::Trashed { .. }), "{out:?}");
    assert!(fs::symlink_metadata(&link).is_err());
    assert_eq!(fs::read_to_string(real.join("keep")).unwrap(), "x");
}

#[test]
fn over_capacity_is_deleted_for_real_and_recorded() {
    let h = home();
    let cfg = DeletionConfig {
        trash_max_mb: 1,
        ..DeletionConfig::default()
    };
    let small = h.work.join("small");
    let big = h.work.join("big");
    fs::write(&small, vec![0u8; 600 * 1024]).unwrap();
    fs::write(&big, vec![0u8; 600 * 1024]).unwrap();

    // The first fits; the second would push the trash past 1 MB.
    let out = run(
        &h,
        &[target(small.clone(), RM), target(big.clone(), RM)],
        &cfg,
    );
    assert!(matches!(out[0], TrashOutcome::Trashed { .. }), "{out:?}");
    assert!(
        matches!(out[1], TrashOutcome::Evicted { cap_mb: 1, .. }),
        "{out:?}"
    );
    assert!(!small.exists() && !big.exists());

    let ev = events(&h.home);
    let ActionEvent::TrashCreated { entry } = &ev[1] else {
        panic!("{ev:?}");
    };
    assert_eq!(entry.status, TrashStatus::PermDeleted);
    assert_eq!(entry.trash_path, None);
    assert_eq!(
        ev[2],
        ActionEvent::TrashPermDeleted {
            entry_id: entry.id,
            reason: PermDeleteReason::CapacityEviction
        }
    );

    // Existing trash counts toward the cap on the next call too.
    fs::write(&big, vec![0u8; 600 * 1024]).unwrap();
    let again = run(&h, &[target(big.clone(), RM)], &cfg);
    assert!(
        matches!(again[0], TrashOutcome::Evicted { .. }),
        "{again:?}"
    );
}

#[test]
fn unprotected_delete_is_recorded() {
    let h = home();
    record_unprotected(&h.home, "rm *.log", "a glob").unwrap();
    let ev = events(&h.home);
    let [
        ActionEvent::UnprotectedDelete {
            command, reason, ..
        },
    ] = ev.as_slice()
    else {
        panic!("{ev:?}");
    };
    assert_eq!((command.as_str(), reason.as_str()), ("rm *.log", "a glob"));
}

/// Review #2164 #2: `rm missing && rm important` — bash stops after the
/// failing `rm`, so `important` must stay where it is.
#[test]
fn failed_segment_short_circuits_the_and_chain() {
    let h = home();
    let important = h.work.join("important.txt");
    fs::write(&important, "precious").unwrap();
    let crate::RmPlan::Rewrite(targets) = crate::plan("rm missing && rm important.txt", &h.work)
    else {
        panic!("expected Rewrite");
    };
    let out = run(&h, &targets, &DeletionConfig::default());
    assert_eq!(out.len(), 1, "{out:?}");
    assert!(matches!(out[0], TrashOutcome::Failed { .. }));
    assert_eq!(fs::read_to_string(&important).unwrap(), "precious");
}

/// Within one `rm`, a bad operand does not stop the rest; and a successful
/// first segment lets the next one run.
#[test]
fn operands_continue_and_successful_segment_continues() {
    let h = home();
    for f in ["a", "b", "c"] {
        fs::write(h.work.join(f), f).unwrap();
    }
    let crate::RmPlan::Rewrite(targets) = crate::plan("rm a && rm missing b && rm c", &h.work)
    else {
        panic!("expected Rewrite");
    };
    let out = run(&h, &targets, &DeletionConfig::default());
    // a: trashed; missing: failed; b: trashed (same rm); c: skipped.
    assert_eq!(out.len(), 3, "{out:?}");
    assert!(matches!(out[0], TrashOutcome::Trashed { .. }));
    assert!(matches!(out[1], TrashOutcome::Failed { .. }));
    assert!(matches!(out[2], TrashOutcome::Trashed { .. }));
    assert!(!h.work.join("a").exists() && !h.work.join("b").exists());
    assert_eq!(fs::read_to_string(h.work.join("c")).unwrap(), "c");
}

/// `rm -f missing && rm b`: `-f` makes the missing operand a success.
#[test]
fn force_missing_does_not_break_the_chain() {
    let h = home();
    fs::write(h.work.join("b"), "b").unwrap();
    let crate::RmPlan::Rewrite(targets) = crate::plan("rm -f missing && rm b", &h.work) else {
        panic!("expected Rewrite");
    };
    run(&h, &targets, &DeletionConfig::default());
    assert!(!h.work.join("b").exists());
}

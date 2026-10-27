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
        after_cd: Vec::new(),
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
fn capacity_comes_from_the_ledger_not_a_disk_walk() {
    let h = home();
    let cfg = DeletionConfig {
        trash_max_mb: 1,
        ..DeletionConfig::default()
    };
    // Bytes in the trash dir that no entry owns are not walked or counted.
    let stray = h.home.join("trash").join("stray");
    fs::create_dir_all(&stray).unwrap();
    fs::write(stray.join("blob"), vec![0u8; 2 * 1024 * 1024]).unwrap();

    let f = h.work.join("f");
    fs::write(&f, vec![0u8; 600 * 1024]).unwrap();
    let out = run(&h, &[target(f.clone(), RM)], &cfg);
    assert!(matches!(out[0], TrashOutcome::Trashed { .. }), "{out:?}");

    // A restore frees its bytes, so the same size fits again.
    let ActionEvent::TrashCreated { entry } = &events(&h.home)[0] else {
        panic!();
    };
    Ledger::<ActionEvent>::open(&ledger_dir(&h.home))
        .unwrap()
        .append(&ActionEvent::TrashRestored { entry_id: entry.id })
        .unwrap();
    fs::write(&f, vec![0u8; 600 * 1024]).unwrap();
    let again = run(&h, &[target(f.clone(), RM)], &cfg);
    assert!(
        matches!(again[0], TrashOutcome::Trashed { .. }),
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

/// A cross-device trash must stage under `<trash>/.staging/`, never beside
/// the entry: the sweep only looks there (review #2164 #1). Guards the
/// `trash_into` → staged-move wiring, which the mover tests cannot see.
#[test]
fn exdev_trash_stages_under_staging_root() {
    use std::cell::RefCell;
    use std::io;

    use crate::layout::{staging_root, trash_root};

    let h = home();
    let f = h.work.join("big.bin");
    fs::write(&f, "payload").unwrap();
    let calls = RefCell::new(Vec::<(PathBuf, PathBuf)>::new());
    let out = crate::trash::trash_into_with(
        &h.home,
        &[target(f.clone(), RM)],
        &DeletionConfig::default(),
        Uuid::nil(),
        &|a: &Path, b: &Path| {
            calls.borrow_mut().push((a.to_path_buf(), b.to_path_buf()));
            if a == f.as_path() {
                return Err(io::Error::from(io::ErrorKind::CrossesDevices));
            }
            fs::rename(a, b)
        },
    );
    let TrashOutcome::Trashed { trash_path, .. } = &out[0] else {
        panic!("{out:?}");
    };
    assert!(!f.exists());
    assert_eq!(fs::read_to_string(trash_path).unwrap(), "payload");

    let stage = staging_root(&trash_root(&h.home));
    let calls = calls.into_inner();
    let into_entry = calls
        .iter()
        .rfind(|(_, b)| b.as_path() == trash_path.as_path())
        .unwrap_or_else(|| panic!("no rename into the entry: {calls:?}"));
    assert!(into_entry.0.starts_with(&stage), "{calls:?}");
    // Nothing left behind in staging after a clean move.
    let leftovers = fs::read_dir(&stage).map(|d| d.count()).unwrap_or(0);
    assert_eq!(leftovers, 0);
}

/// Bash stops at a failed `cd`; an absolute target after it is independent of
/// the bad base, so only an explicit check keeps it (review #2164 P1).
#[test]
fn failed_cd_stops_later_segments() {
    let h = home();
    let keep = h.work.join("important");
    fs::write(&keep, "keep").unwrap();
    let done = h.work.join("a");
    fs::write(&done, "a").unwrap();
    for cmd in [
        format!("cd missing && rm {}", keep.display()),
        format!(
            "rm {} && cd missing && rm {}",
            done.display(),
            keep.display()
        ),
    ] {
        let crate::RmPlan::Rewrite(targets) = crate::plan(&cmd, &h.work) else {
            panic!("{cmd}: not rewritten")
        };
        let out = trash_into(&h.home, &targets, &DeletionConfig::default(), Uuid::nil());
        assert!(keep.exists(), "{cmd}: {out:?}");
        assert!(
            out.last().is_some_and(TrashOutcome::is_failure),
            "{cmd}: {out:?}"
        );
        assert!(
            !out.iter()
                .any(|o| matches!(o, TrashOutcome::Trashed { target, .. } if target.path == keep)),
            "{cmd}: {out:?}"
        );
    }
    assert!(!done.exists(), "the segment before the cd still runs");
}

/// A `cd` into a real directory still lets the rewrite proceed.
#[test]
fn successful_cd_runs_later_segments() {
    let h = home();
    fs::create_dir_all(h.work.join("sub")).unwrap();
    let f = h.work.join("sub/x");
    fs::write(&f, "x").unwrap();
    let crate::RmPlan::Rewrite(targets) = crate::plan("cd sub && rm x", &h.work) else {
        panic!()
    };
    let out = trash_into(&h.home, &targets, &DeletionConfig::default(), Uuid::nil());
    assert!(matches!(out[..], [TrashOutcome::Trashed { .. }]), "{out:?}");
    assert!(!f.exists());
}

/// A directory that exists but denies search permission fails `cd` just like
/// a missing one, so later segments must not run (review #2172 #1).
#[test]
fn unsearchable_cd_stops_later_segments() {
    use std::os::unix::fs::PermissionsExt;
    if unsafe { libc::geteuid() } == 0 {
        return; // root bypasses permission bits; the shell would cd fine
    }
    let h = home();
    let blocked = h.work.join("blocked");
    fs::create_dir_all(&blocked).unwrap();
    let keep = h.work.join("important");
    fs::write(&keep, "keep").unwrap();
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o600)).unwrap();
    let cmd = format!("cd blocked && rm {}", keep.display());
    let crate::RmPlan::Rewrite(targets) = crate::plan(&cmd, &h.work) else {
        panic!("not rewritten")
    };
    let out = trash_into(&h.home, &targets, &DeletionConfig::default(), Uuid::nil());
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(keep.exists(), "{out:?}");
    assert!(matches!(&out[..], [o] if o.is_failure()), "{out:?}");
}

/// Each `cd` is checked once, when the shell would run it; a directory entered
/// earlier and removed later must not fail a later segment (review #2172 #2).
#[test]
fn completed_cd_is_not_rechecked() {
    let h = home();
    let a = h.work.join("a");
    fs::create_dir_all(&a).unwrap();
    let keep = h.work.join("keep");
    fs::write(&keep, "k").unwrap();
    let cmd = format!(
        "cd {} && cd {} && rmdir a && rm {}",
        a.display(),
        h.work.display(),
        keep.display()
    );
    let crate::RmPlan::Rewrite(targets) = crate::plan(&cmd, &h.work) else {
        panic!("not rewritten")
    };
    let out = trash_into(&h.home, &targets, &DeletionConfig::default(), Uuid::nil());
    assert!(
        matches!(
            &out[..],
            [TrashOutcome::Trashed { .. }, TrashOutcome::Trashed { .. }]
        ),
        "{out:?}"
    );
    assert!(!a.exists() && !keep.exists());
}

/// Review #2177 P1: a retained entry whose creation record is older than the
/// lookup window still holds its bytes, so the cap still counts them.
#[test]
fn aged_out_creation_record_still_counts_toward_the_cap() {
    let h = home();
    let cfg = DeletionConfig {
        trash_max_mb: 1,
        ..DeletionConfig::default()
    };
    let old = h.work.join("old");
    fs::write(&old, vec![0u8; 900 * 1024]).unwrap();
    let out = run(&h, &[target(old, RM)], &cfg);
    assert!(matches!(out[0], TrashOutcome::Trashed { .. }), "{out:?}");

    // Move today's ledger file far behind the lookup window.
    let dir = ledger_dir(&h.home);
    let today = chrono::Local::now().date_naive();
    let age = i64::from(crate::expire::lookup_days(&cfg)) + 30;
    let aged = today - chrono::Duration::days(age);
    fs::rename(
        dir.join(format!("{}.jsonl", today.format("%Y-%m-%d"))),
        dir.join(format!("{}.jsonl", aged.format("%Y-%m-%d"))),
    )
    .unwrap();

    let new = h.work.join("new");
    fs::write(&new, vec![0u8; 900 * 1024]).unwrap();
    let again = run(&h, &[target(new, RM)], &cfg);
    assert!(
        matches!(again[0], TrashOutcome::Evicted { cap_mb: 1, .. }),
        "{again:?}"
    );
}

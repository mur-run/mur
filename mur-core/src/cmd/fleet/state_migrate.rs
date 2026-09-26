//! One-way move of fleet run state out of `fleets/<name>/` (the definition)
//! into `fleet-state/<name>/` (see `mur_common::paths::FLEET_STATE`).
//!
//! Runs at daemon start and on every `mur fleet` / `mur deep-research`
//! invocation. Idempotent and cheap — a `read_dir` plus a few `stat`s per
//! fleet — so it needs no "already migrated" marker, and a marker would miss
//! files a sandboxed run could not move (below).
//!
//! Best-effort by contract: a failure is a warning, never an error, because
//! the caller is about to do the user's actual work. The expected failure is a
//! sandboxed `fleet_run` child: `fleets/` is read-only to it, so it cannot
//! rename out of there. It still writes the NEW location, which is why a
//! destination that already exists is merged, never overwritten — the next
//! unsandboxed `mur` finishes the move.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use mur_common::fleet::valid_fleet_name;

/// Entries a run (or `judge` / `cherry` / `partition` / `concurrent`) writes
/// in a fleet's directory, moved whole when the destination is absent.
///
/// Not `tracks/`: `tracks.json` records each worktree by ABSOLUTE path, so
/// moving the checkouts would break the file that points at them. They stay
/// where they are and keep working.
const MOVE_WHOLE: [&str; 6] = [
    "tracks.json",
    "parallel_state",
    "cherry-result",
    "judge_stats.json",
    "concurrent_stats.json",
    mur_common::paths::FLEET_PROGRESS_FILE,
];

const JOBS: &str = "jobs";
const EVENTS: &str = "events.jsonl";

/// Move every legacy run-state entry for every fleet. Never fails.
pub fn migrate_all(mur_home: &Path) {
    let Ok(entries) = std::fs::read_dir(mur_home.join(mur_common::paths::FLEETS)) else {
        return;
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        // Only real fleet names: the name becomes a path under fleet-state/.
        if !valid_fleet_name(&name) || !e.path().is_dir() {
            continue;
        }
        migrate_one(mur_home, &name);
    }
}

fn migrate_one(mur_home: &Path, name: &str) {
    let src = mur_home.join(mur_common::paths::FLEETS).join(name);
    let dst = mur_common::paths::fleet_state_dir(mur_home, name);
    let legacy: Vec<&str> = MOVE_WHOLE
        .iter()
        .copied()
        .chain([JOBS, EVENTS])
        .filter(|f| src.join(f).symlink_metadata().is_ok())
        .collect();
    if legacy.is_empty() {
        return;
    }
    if let Err(e) = std::fs::create_dir_all(&dst) {
        warn(name, &dst, &e);
        return;
    }
    for f in legacy {
        let (from, to) = (src.join(f), dst.join(f));
        let res = match f {
            JOBS => merge_dir(&from, &to),
            EVENTS => merge_log(&from, &to),
            mur_common::paths::FLEET_PROGRESS_FILE if to.exists() => {
                // The new slot is the newer run by construction; the old one
                // is a superseded last-run record.
                std::fs::remove_file(&from)
            }
            _ if to.symlink_metadata().is_ok() => {
                tracing::warn!(
                    fleet = name,
                    "{} and {} both exist — left the old one in place; remove it once you have checked which to keep",
                    from.display(),
                    to.display()
                );
                Ok(())
            }
            _ => rename(&from, &to),
        };
        if let Err(e) = res {
            warn(name, &from, &e);
        }
    }
}

fn warn(fleet: &str, path: &Path, e: &std::io::Error) {
    tracing::warn!(
        fleet,
        "could not move {} to {}/ ({e}); run any `mur fleet` command outside an agent sandbox, or restart murmurd, to finish",
        path.display(),
        mur_common::paths::FLEET_STATE
    );
}

/// `rename`, treating a source that vanished mid-way (a concurrent migrator
/// won) as done.
fn rename(from: &Path, to: &Path) -> std::io::Result<()> {
    match std::fs::rename(from, to) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        r => r,
    }
}

/// Move each file of `from` into `to` unless `to` already has it (job ids are
/// UUIDv7, so a clash is the same job), then drop `from` if it is empty.
fn merge_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    if to.symlink_metadata().is_err() {
        return rename(from, to);
    }
    for e in std::fs::read_dir(from)?.flatten() {
        let target = to.join(e.file_name());
        if target.symlink_metadata().is_err() {
            rename(&e.path(), &target)?;
        }
    }
    // Not remove_dir_all: anything left is a clash worth keeping.
    let _ = std::fs::remove_dir(from);
    Ok(())
}

/// Append the legacy event log to the live one, then remove it.
///
/// The source is first renamed to a claim name so exactly one of two racing
/// migrators appends it (the loser sees NotFound). The append takes the same
/// exclusive `flock` as `event_log::append_event`, so it cannot tear a line a
/// concurrent run is writing. Nothing reads this log in order — each line
/// carries its own timestamp — so appending older lines after newer ones is
/// harmless.
fn merge_log(from: &Path, to: &Path) -> std::io::Result<()> {
    if to.symlink_metadata().is_err() {
        return rename(from, to);
    }
    let claimed = from.with_extension(format!("jsonl.migrating-{}", std::process::id()));
    rename(from, &claimed)?;
    if claimed.symlink_metadata().is_err() {
        return Ok(());
    }
    let mut body = Vec::new();
    std::fs::File::open(&claimed)?.read_to_end(&mut body)?;
    if !body.is_empty() && !body.ends_with(b"\n") {
        body.push(b'\n');
    }
    {
        use fs2::FileExt;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(to)?;
        f.lock_exclusive()?;
        let end = f.seek(SeekFrom::End(0))?;
        // A live log whose last line lacks its newline would fuse with ours.
        if end > 0 {
            let mut last = [0u8; 1];
            f.seek(SeekFrom::Start(end - 1))?;
            f.read_exact(&mut last)?;
            f.seek(SeekFrom::End(0))?;
            if last[0] != b'\n' {
                f.write_all(b"\n")?;
            }
        }
        f.write_all(&body)?;
        f.flush()?;
    }
    std::fs::remove_file(&claimed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn legacy(home: &Path, name: &str) -> std::path::PathBuf {
        let d = home.join("fleets").join(name);
        std::fs::create_dir_all(d.join("jobs")).unwrap();
        std::fs::write(d.join("fleet.yaml"), "name: x\n").unwrap();
        std::fs::write(d.join(".stopped"), "stopped\n").unwrap();
        std::fs::write(d.join(".last_run"), "1").unwrap();
        d
    }

    #[test]
    fn moves_run_state_and_leaves_the_definition() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let d = legacy(home, "dev");
        std::fs::write(d.join("jobs/a.yaml"), "id: a\n").unwrap();
        std::fs::write(d.join("events.jsonl"), "{\"e\":1}\n").unwrap();
        std::fs::write(d.join(".run_progress.json"), "{}").unwrap();
        std::fs::create_dir_all(d.join("tracks/track-a")).unwrap();
        std::fs::write(d.join("tracks.json"), "{}").unwrap();

        migrate_all(home);

        let s = mur_common::paths::fleet_state_dir(home, "dev");
        assert!(s.join("jobs/a.yaml").exists());
        assert!(s.join("events.jsonl").exists());
        assert!(s.join(".run_progress.json").exists());
        assert!(s.join("tracks.json").exists());
        for gone in ["jobs", "events.jsonl", ".run_progress.json", "tracks.json"] {
            assert!(!d.join(gone).exists(), "{gone} should have moved");
        }
        // Definition, kill-switch, daemon stamp and the worktrees stay put.
        for kept in ["fleet.yaml", ".stopped", ".last_run", "tracks/track-a"] {
            assert!(d.join(kept).exists(), "{kept} must stay in fleets/");
        }

        // Idempotent: a second pass finds nothing and changes nothing.
        migrate_all(home);
        assert!(s.join("jobs/a.yaml").exists());
    }

    /// A sandboxed run could not move the old files but DID write the new
    /// location; the next unsandboxed pass must merge, not clobber.
    #[test]
    fn merges_into_state_a_sandboxed_run_already_wrote() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let d = legacy(home, "dev");
        std::fs::write(d.join("jobs/old.yaml"), "id: old\n").unwrap();
        std::fs::write(d.join("events.jsonl"), "{\"e\":\"old\"}").unwrap();
        std::fs::write(d.join(".run_progress.json"), "old").unwrap();

        let s = mur_common::paths::fleet_state_dir(home, "dev");
        std::fs::create_dir_all(s.join("jobs")).unwrap();
        std::fs::write(s.join("jobs/new.yaml"), "id: new\n").unwrap();
        std::fs::write(s.join("events.jsonl"), "{\"e\":\"new\"}\n").unwrap();
        std::fs::write(s.join(".run_progress.json"), "new").unwrap();

        migrate_all(home);

        assert!(s.join("jobs/old.yaml").exists() && s.join("jobs/new.yaml").exists());
        assert!(!d.join("jobs").exists(), "emptied legacy jobs/ is removed");
        let log = std::fs::read_to_string(s.join("events.jsonl")).unwrap();
        assert_eq!(log, "{\"e\":\"new\"}\n{\"e\":\"old\"}\n");
        assert!(!d.join("events.jsonl").exists());
        assert_eq!(
            std::fs::read_to_string(s.join(".run_progress.json")).unwrap(),
            "new"
        );
        assert!(!d.join(".run_progress.json").exists());
    }

    #[test]
    fn a_clash_it_cannot_merge_keeps_both() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let d = legacy(home, "dev");
        std::fs::write(d.join("tracks.json"), "old").unwrap();
        let s = mur_common::paths::fleet_state_dir(home, "dev");
        std::fs::create_dir_all(&s).unwrap();
        std::fs::write(s.join("tracks.json"), "new").unwrap();

        migrate_all(home);

        assert_eq!(
            std::fs::read_to_string(d.join("tracks.json")).unwrap(),
            "old"
        );
        assert_eq!(
            std::fs::read_to_string(s.join("tracks.json")).unwrap(),
            "new"
        );
    }

    #[test]
    fn a_fleet_with_nothing_to_move_gets_no_state_dir() {
        let tmp = tempfile::tempdir().unwrap();
        legacy(tmp.path(), "idle");
        std::fs::remove_dir(tmp.path().join("fleets/idle/jobs")).unwrap();
        migrate_all(tmp.path());
        assert!(!tmp.path().join("fleet-state").exists());
    }

    /// The failure a sandboxed child hits: the source tree is read-only.
    /// It must warn and return, never panic or error.
    #[cfg(unix)]
    #[test]
    fn a_read_only_source_is_left_in_place() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let d = legacy(home, "dev");
        std::fs::write(d.join("events.jsonl"), "{}\n").unwrap();
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o555)).unwrap();

        migrate_all(home);

        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(d.join("events.jsonl").exists());
    }
}

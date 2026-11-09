//! One-time move of fleet run directories out of `runs/` into each fleet's
//! own tree, `fleet-state/<fleet>/runs/<run_id>/` (#2316).
//!
//! `runs/` is writable by every sandboxed agent, so a fleet run's checkpoint
//! there could be rewritten by an agent of another fleet and then trusted by
//! `mur fleet resume`. New runs are written to the fleet tree directly; this
//! moves the ones recorded before that.
//!
//! **Once, never again.** A migrator that keeps adopting whatever appears in
//! `runs/` would itself be the attack: an agent plants a `run.json` naming
//! another fleet, and the trusted migrator moves it into that fleet's tree.
//! So the move is gated on [`MARKER`] in the `fleet-state/` root, created
//! with `create_new` BEFORE anything moves:
//!
//! - only one process ever wins the creation, so two concurrent `mur`
//!   invocations cannot both migrate;
//! - no sandbox can write the `fleet-state/` root (it carves in only
//!   `fleet-state/<fleet>/`), so an agent-triggered `mur fleet` never
//!   migrates, and cannot delete the marker to re-arm it;
//! - a run that fails to move stays in `runs/`, where `mur job status` still
//!   reports it but `mur fleet resume` does not trust it — the warning names
//!   `mur fleet stop` as the way out;
//! - a process that dies mid-move leaves the runs it had not reached in
//!   `runs/` for good, WITHOUT that warning (the marker already exists, so
//!   nothing retries). A paused fleet run found there after an upgrade is
//!   one of those: `mur fleet stop <fleet>` discards it.
//!
//! **Only real directories of plain files move.** `runs/` was writable by
//! every agent until this move, so an entry there may be a symlink (to
//! another run's directory, renamed aside) or hold a symlinked or
//! hard-linked file. Moved into the fleet tree, such an entry would read the
//! trusted path out of a tree any agent still writes. Those entries stay in
//! `runs/`, with the same warning; the trusted readers also refuse links
//! (`mur_common::paths::read_unlinked`), so one missed here is not trusted
//! either.
//!
//! **Limits.** A process that already holds an open handle on a file or
//! directory in `runs/<id>` keeps write access after the rename; the move
//! cannot revoke it. The `.migrated-from-runs` flag and resume's warning are
//! the only mitigation. A run whose process is still alive is left in
//! `runs/` rather than moved from under it.
//!
//! Best-effort like `state_migrate`: never an error, the caller is about to
//! do the user's real work.

use std::path::Path;

use mur_common::fleet::valid_fleet_name;

/// Written to the `fleet-state/` root once the move has been claimed.
pub const MARKER: &str = ".runs-migrated";

/// Dropped into a run directory after it was moved from `runs/`. While in
/// `runs/` any agent could write it, so a moved run with no signed
/// `run_paused` cannot be vouched for; resume warns on this flag.
pub const MIGRATED_FLAG: &str = ".migrated-from-runs";

/// Whether this fleet run directory was moved out of the shared `runs/`.
pub fn was_migrated(mur_home: &Path, fleet: &str, run_id: &str) -> bool {
    mur_common::paths::fleet_run_dir(mur_home, fleet, run_id)
        .join(MIGRATED_FLAG)
        .symlink_metadata()
        .is_ok()
}

/// Files whose content names a run's fleet, most specific first.
const RUN_FILE: &str = "run.json";
const SIDECAR_FILE: &str = "sidecar.json";

/// Claim the one-time move and perform it. Never fails.
pub fn migrate_once(mur_home: &Path) {
    // Claimed even when there is nothing to move: a fresh home must not stay
    // armed until an agent has created `runs/` and planted a fleet run in it.
    if !claim(mur_home) {
        return;
    }
    let Ok(entries) = std::fs::read_dir(mur_home.join(mur_common::paths::RUNS)) else {
        return;
    };
    for e in entries.flatten() {
        let Some(run_id) = e.file_name().to_str().map(str::to_string) else {
            continue;
        };
        // `file_type` does not follow a symlink, unlike `Path::is_dir`.
        let real_dir = e.file_type().is_ok_and(|t| t.is_dir());
        if !mur_common::paths::is_single_segment(&run_id) || !real_dir {
            continue;
        }
        if mur_common::paths::holds_links(&e.path()) {
            // Only worth a warning when it would otherwise have moved.
            if let Some(fleet) = fleet_of_dir(&e.path()) {
                warn_left(&fleet, &run_id, "it holds a symlink or hard link");
            }
            continue;
        }
        if let Some(fleet) = fleet_of_dir(&e.path()) {
            if still_running(&e.path()) {
                warn_left(&fleet, &run_id, "its process is still running");
                continue;
            }
            move_run(mur_home, &fleet, &run_id);
        }
    }
}

/// Create the marker; `true` only for the one caller that created it. Any
/// failure — it exists, or this process may not write the `fleet-state/`
/// root (a sandbox) — means "not mine to migrate".
fn claim(mur_home: &Path) -> bool {
    let root = mur_home.join(mur_common::paths::FLEET_STATE);
    if std::fs::create_dir_all(&root).is_err() {
        return false;
    }
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(root.join(MARKER))
        .is_ok()
}

/// The fleet a legacy run directory belongs to, or `None` for a run of no
/// fleet (`parallel_jobs`, workflows), which stays in `runs/`.
fn fleet_of_dir(dir: &Path) -> Option<String> {
    let read = |f: &str| -> Option<serde_json::Value> {
        serde_json::from_slice(&std::fs::read(dir.join(f)).ok()?).ok()
    };
    let from_record = |v: serde_json::Value| -> Option<String> {
        (v.get("kind")?.as_str()? == "fleet")
            .then(|| v.get("channel_id")?.as_str().map(str::to_string))
            .flatten()
            .and_then(|c| mur_common::fleet::fleet_name_from_channel_id(&c).map(str::to_string))
    };
    let named = |v: serde_json::Value| v.get("fleet")?.as_str().map(str::to_string);
    read(RUN_FILE)
        .and_then(from_record)
        .or_else(|| read(SIDECAR_FILE).and_then(from_record))
        .or_else(|| read(crate::cmd::fleet::loop_run::checkpoint::CHECKPOINT_FILE).and_then(named))
        .or_else(|| {
            read(mur_track::fleet_dispatch_request::FLEET_DISPATCH_RECORD_FILE).and_then(named)
        })
        .filter(|f| valid_fleet_name(f))
}

/// A run whose record is neither finished nor paused and whose process is
/// alive: an older binary is still writing `runs/<id>`, and moving it would
/// leave its later updates finding no directory.
fn still_running(dir: &Path) -> bool {
    let Some(v) = std::fs::read(dir.join(RUN_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
    else {
        return false;
    };
    let state = v.get("state").and_then(|s| s.as_str()).unwrap_or("");
    let done = matches!(state, "done" | "failed" | "stopped" | "skipped" | "paused");
    let pid = v
        .get("pid")
        .and_then(|p| p.as_u64())
        .and_then(|p| u32::try_from(p).ok());
    !done && pid.is_some_and(mur_common::lock_file::pid_alive)
}

fn move_run(mur_home: &Path, fleet: &str, run_id: &str) {
    let from = mur_home.join(mur_common::paths::RUNS).join(run_id);
    let to = mur_common::paths::fleet_run_dir(mur_home, fleet, run_id);
    let result = if to.symlink_metadata().is_ok() {
        // Never merge or overwrite: the fleet tree is the trusted copy.
        Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "the fleet already has a run with this id",
        ))
    } else {
        to.parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::rename(&from, &to))
            // Best-effort: a missing flag only loses the resume warning.
            .inspect(|()| {
                let _ = std::fs::write(to.join(MIGRATED_FLAG), b"");
            })
    };
    if let Err(e) = result {
        warn_left(
            fleet,
            run_id,
            &format!("moving {} to {} failed: {e}", from.display(), to.display()),
        );
    }
}

fn warn_left(fleet: &str, run_id: &str, why: &str) {
    tracing::warn!(
        fleet,
        run_id,
        "fleet run left in runs/ ({why}); it still reports in `mur job status`, but \
         `mur fleet resume {fleet}` will not continue it — `mur fleet stop {fleet}` discards it"
    );
}

#[cfg(test)]
#[path = "run_migrate_tests.rs"]
mod tests;

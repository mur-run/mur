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
//!   `mur fleet stop` as the way out.
//!
//! Best-effort like `state_migrate`: never an error, the caller is about to
//! do the user's real work.

use std::path::Path;

use mur_common::fleet::valid_fleet_name;

/// Written to the `fleet-state/` root once the move has been claimed.
pub const MARKER: &str = ".runs-migrated";

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
        if !mur_common::paths::is_single_segment(&run_id) || !e.path().is_dir() {
            continue;
        }
        if let Some(fleet) = fleet_of_dir(&e.path()) {
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
    };
    if let Err(e) = result {
        tracing::warn!(
            fleet,
            run_id,
            "could not move {} to {} ({e}); it still reports in `mur job status`, but \
             `mur fleet resume {fleet}` will not continue it — `mur fleet stop {fleet}` discards it",
            from.display(),
            to.display()
        );
    }
}

#[cfg(test)]
#[path = "run_migrate_tests.rs"]
mod tests;

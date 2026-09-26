//! Directory names under `<mur_home>` that a fleet / deep-research run writes.
//!
//! One list, two readers that must never disagree: `mur-core` creates and
//! writes these while a run executes, and `mur-agent-runtime` carves exactly
//! these into the kernel sandbox for an agent-triggered run (`fleet_run`).
//! They lived as two separate literals and drifted — `runs/` was writable by
//! the CLI and denied to the sandbox, so every agent-triggered run failed to
//! register itself and `mur fleet status` reported nothing for a run that was
//! live (fleet develop-rust, 2026-09-09). The list lives here because the
//! runtime must not depend on `mur-core`.

use std::path::{Path, PathBuf};

/// Run-status records: `<mur_home>/runs/<run_id>/run.json`.
pub const RUNS: &str = "runs";

/// Fleet definitions: `<mur_home>/fleets/<name>/fleet.yaml`, plus the
/// operator's `.stopped` kill-switch and the daemon's `.last_run` stamp.
///
/// Configuration, not run state — deliberately absent from
/// [`RUN_STATE_DIRS`]. A run that could write here could rewrite its own
/// fleet's members, limits and HITL pre-approvals, or clear its own
/// kill-switch.
pub const FLEETS: &str = "fleets";

/// What a fleet's runs write, per fleet: `<mur_home>/fleet-state/<name>/`
/// (job queue, progress record, event log, parallel-track state).
///
/// Its own tree, not `runs/`: that store is keyed by run_id and lists every
/// directory in it as a run, so a fleet name dropped in there reads as a
/// phantom run. Not `fleets/`: that is configuration (see [`FLEETS`]).
pub const FLEET_STATE: &str = "fleet-state";

/// The fleet's single progress record, inside [`fleet_state_dir`]. Kept after
/// a run as the last-run record; overwritten by the next run. Read by both
/// the CLI and the runtime's `fleet_run` live-run guard, so named once here.
pub const FLEET_PROGRESS_FILE: &str = ".run_progress.json";

/// Every directory an in-sandbox fleet run must be able to write.
///
/// Add here FIRST, then use it. A directory the runner writes but this list
/// omits fails only in the sandboxed path — the one nobody runs by hand.
///
/// [`FLEET_STATE`] is listed for the CLI side and for tests; the sandbox
/// carves in only the `fleet_run.fleets` subdirectories of it (see
/// `mur-agent-runtime` `sandbox/policy.rs`), so an agent cannot queue work
/// for a fleet the operator never let it run.
pub const RUN_STATE_DIRS: [&str; 5] =
    [FLEET_STATE, "commander", "conversations", "artifacts", RUNS];

/// `<mur_home>/fleet-state/<name>/` — every file a run of `name` writes.
pub fn fleet_state_dir(mur_home: &Path, name: &str) -> PathBuf {
    mur_home.join(FLEET_STATE).join(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_is_listed() {
        // The drift that broke agent-triggered runs was exactly this: the
        // writer knew about `runs`, the sandbox list did not.
        assert!(RUN_STATE_DIRS.contains(&RUNS));
    }

    #[test]
    fn fleet_definitions_are_not_run_state() {
        // The whole point of FLEET_STATE: a run writes there, never into
        // the tree that holds the fleet's own definition and kill-switch.
        assert!(!RUN_STATE_DIRS.contains(&FLEETS));
        assert!(RUN_STATE_DIRS.contains(&FLEET_STATE));
        assert_ne!(FLEET_STATE, RUNS);
    }

    #[test]
    fn fleet_definitions_are_not_an_authoring_grant() {
        // Same boundary, the other writer: the seeded concierge gets write on
        // every AUTHORING_DIRS entry, so `fleets/` there hands it the fleet's
        // members, limits, HITL pre-approvals and `.stopped` kill-switch.
        assert!(!crate::agent::AUTHORING_DIRS.contains(&FLEETS));
        assert!(!crate::agent::AUTHORING_DIRS.contains(&FLEET_STATE));
    }
}

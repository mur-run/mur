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

/// Run-status records: `<mur_home>/runs/<run_id>/run.json`.
pub const RUNS: &str = "runs";

/// Every directory an in-sandbox fleet run must be able to write.
///
/// Add here FIRST, then use it. A directory the runner writes but this list
/// omits fails only in the sandboxed path — the one nobody runs by hand.
pub const RUN_STATE_DIRS: [&str; 5] = ["fleets", "commander", "conversations", "artifacts", RUNS];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_is_listed() {
        // The drift that broke agent-triggered runs was exactly this: the
        // writer knew about `runs`, the sandbox list did not.
        assert!(RUN_STATE_DIRS.contains(&RUNS));
    }
}

//! `mur deep-research run <fleet>` — thin wrapper over the EXISTING guarded
//! fleet loop (`cmd/fleet/loop_run.rs`, the same entry point
//! `mur fleet run --loop` uses). Deliberately does not reimplement any loop
//! logic: it resolves the named fleet and calls
//! [`crate::cmd::fleet::loop_run::cmd_fleet_run_loop`] with the caller's
//! guard overrides.
//!
//! ## What this proves vs. what it does not (Task 10 re-scope)
//!
//! The guard rails (iteration cap, deadline, budget ceiling, kill-switch,
//! commander governance) run BEFORE any delegation and are fully
//! deterministic — see the `tests` module below, which drives this wrapper
//! through a commander-kill short-circuit exactly as
//! `cmd/fleet/loop_run.rs`'s own `commander_kill_halts_loop_and_local_start_cannot_clear_it`
//! test does, proving `cmd_deep_research_run` correctly reaches
//! `run_guarded` and that the guard stops the loop before any work happens.
//!
//! What this does NOT and CANNOT prove without live infrastructure: the
//! full decompose → research → verify → synthesize → marker-convergence
//! loop. `run_guarded`'s per-iteration delegation
//! (`cmd/fleet/loop_run.rs:execute_dag`) dials **live agent runtime
//! sockets** via `crate::a2a_dial::dial_message_streaming`; a worker
//! actually researching (calling the stub/real gateway, having an LLM
//! decide to emit the `RESEARCH_COMPLETE` marker) requires a RUNNING
//! `mur-agent-runtime` process with an LLM backend attached. That
//! live-agent convergence run — workers hitting `stub_gateway`, an LLM
//! synthesizing a cited report, the marker landing on the channel — is
//! Task 11's operator E2E, not something this automated test suite can
//! exercise headlessly.

use std::path::Path;

use anyhow::Result;

/// Resolve `name` as a fleet and run its guarded loop. Identical semantics
/// to `mur fleet run <name> --loop`; see
/// [`crate::cmd::fleet::loop_run::cmd_fleet_run_loop`] for the guard
/// precedence (CLI flag > fleet.yaml > default) and stop reasons.
pub async fn cmd_deep_research_run(
    mur_home: &Path,
    name: &str,
    max_iterations: Option<u32>,
    deadline: Option<String>,
    budget_usd: Option<f64>,
) -> Result<()> {
    crate::cmd::fleet::loop_run::cmd_fleet_run_loop(
        mur_home,
        name,
        max_iterations,
        deadline,
        budget_usd,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::fleet::Fleet;

    /// Mirrors `cmd/fleet/loop_run.rs`'s
    /// `commander_kill_halts_loop_and_local_start_cannot_clear_it` setup,
    /// but drives it through `mur deep-research run` (this module's
    /// wrapper) rather than `run_guarded` directly — proving the new
    /// command actually reaches the guarded loop and its kill-switch,
    /// without requiring any live agent runtime or gateway process.
    #[tokio::test]
    async fn deep_research_run_stops_on_commander_kill_before_any_delegation() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();

        // commander identity + pinned key
        let cdir = home.join("commander");
        std::fs::create_dir_all(&cdir).unwrap();
        mur_common::identity::AgentIdentity::generate()
            .save(&cdir)
            .unwrap();

        // a deep-research-flavored fleet + channel (2 workers, structured
        // marker convergence — never reached because the kill fires first).
        let fleet = Fleet {
            name: "dr-test".into(),
            display_name: String::new(),
            goal: "Research topic X and produce a cited report".into(),
            router: None,
            members: vec!["research_worker_1".into(), "research_worker_2".into()],
            team_id: None,
            channel_id: "fleet-dr-test".into(),
            rules: vec![],
            skills: vec![],
            loop_cfg: Some(mur_common::fleet::FleetLoop {
                trigger: "manual".into(),
                max_iterations: 4,
                budget_usd: 1.0,
                deadline: String::new(),
                done_when: "marker:RESEARCH_COMPLETE".into(),
            }),
            parallel: None,
        };
        crate::cmd::fleet::store::save_fleet(home, &fleet).unwrap();
        mur_channel::ChannelService::open(home)
            .unwrap()
            .create_for_fleet(
                "dr-test",
                "mur",
                &["research_worker_1".into(), "research_worker_2".into()],
            )
            .unwrap();

        // plant a commander kill for this fleet
        crate::cmd::commander::cmd_commander_directive(home, "dr-test", "kill", None, 1000)
            .unwrap();

        // `mur deep-research run dr-test` must stop before any delegation —
        // if it reached execute_dag it would try to dial nonexistent live
        // agent sockets and this test would hang or error, not return Ok.
        cmd_deep_research_run(home, "dr-test", Some(1), None, None)
            .await
            .expect("guarded loop stop is a clean Ok(()), not an Err");

        // an audit Governance entry was recorded — same evidence the
        // loop_run.rs unit test checks, proving the real guard fired
        // (not a no-op wrapper that swallowed the kill).
        let audit =
            std::fs::read_to_string(home.join("conversations").join("audit.jsonl")).unwrap();
        assert!(
            audit.contains("\"kind\":\"governance\"") && audit.contains("\"decision\":\"halted\""),
            "expected a governance halt audit row, got: {audit}"
        );
    }
}

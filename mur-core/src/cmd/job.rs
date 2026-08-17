//! `mur job` — query and stop runs. Rendering only: every verdict comes from
//! `run_status::classify`, which is the sole derivation point (spec §4).

use std::path::Path;

use anyhow::{Context, Result};
use clap::Subcommand;

use crate::run_status::{Liveness, RunStatus, State, store};

#[derive(Subcommand, Debug)]
pub enum JobAction {
    /// List runs. Hides cleanly finished runs unless `--all`.
    List {
        /// Include runs that finished, failed, or were stopped.
        #[arg(long)]
        all: bool,
    },
    /// Show one run in detail.
    Status {
        /// Run id (from `mur job list`).
        run_id: String,
    },
    /// Mark a run stopped. Does NOT signal or kill the orchestrator process.
    ///
    /// The first reason is safety: a record rebuilt from the channel carries
    /// `pid: 0`, and on Unix `kill(0, sig)` targets the CALLER's entire
    /// process group — `mur job stop` on a rebuilt run would kill the user's
    /// own shell. Never pass a `RunState.pid` to a signalling call.
    /// The second is layering: enforcement belongs with the executor, and
    /// Plan B makes the run loop honour a `Stopped` record. Until then this
    /// marks intent, and the CLI must say so rather than implying the work
    /// has halted.
    Stop {
        /// Run id (from `mur job list`).
        run_id: String,
    },
}

/// Whether a run appears in `mur job list` without `--all`.
///
/// A crashed run — `State::Running` with `Liveness::Dead` — is deliberately
/// visible: nothing wrote a terminal state for it, and it is precisely what an
/// operator is looking for.
pub fn visible_in_list(status: &RunStatus) -> bool {
    !status.state.is_terminal()
}

fn load_status(mur_home: &Path, run_id: &str) -> Result<Option<RunStatus>> {
    crate::run_status::status_of(mur_home, run_id)
}

fn liveness_label(l: Liveness) -> &'static str {
    match l {
        Liveness::Alive => "alive",
        Liveness::Stalled => "STALLED",
        Liveness::Dead => "DEAD",
        Liveness::Unknown => "unknown",
        Liveness::NotApplicable => "-",
    }
}

fn state_label(s: State) -> &'static str {
    match s {
        State::Running => "running",
        State::Blocked => "blocked",
        State::Done => "done",
        State::Failed => "failed",
        State::Stopped => "stopped",
    }
}

/// Render one run's full status — the shared renderer for `mur job status`
/// AND `mur fleet status`. Spec §4: these two surfaces derive through
/// `status_of` and render through this ONE function; two renderers is how
/// two surfaces drift into disagreeing about one fact.
pub fn print_status(w: &mut dyn std::io::Write, s: &RunStatus) {
    let _ = writeln!(w, "run       {}", s.run.run_id);
    let _ = writeln!(w, "kind      {:?}", s.run.kind);
    let _ = writeln!(w, "label     {}", s.run.label);
    let _ = writeln!(w, "state     {}", state_label(s.state));
    let _ = writeln!(w, "liveness  {}", liveness_label(s.liveness));
    // A rebuilt record carries pid 0 (no process is known), and a bare
    // `pid 0` reads like a real pid out of context — say what it is. The
    // signal is the structural one used everywhere else: no heartbeat and
    // pid 0 together mean "rebuilt from the channel".
    if s.run.last_heartbeat_at.is_none() && s.run.pid == 0 {
        let _ = writeln!(w, "pid       unknown (record rebuilt)");
    } else {
        let _ = writeln!(w, "pid       {}", s.run.pid);
    }
    let _ = writeln!(w, "started   {}", s.run.started_at.to_rfc3339());
    match s.run.last_heartbeat_at {
        Some(b) => {
            let _ = writeln!(w, "heartbeat {}", b.to_rfc3339());
        }
        None => {
            let _ = writeln!(w, "heartbeat unknown (record was rebuilt from the channel)");
        }
    }
    if let Some(c) = &s.run.channel_id {
        let _ = writeln!(w, "channel   {c}");
    }
    if let Some(b) = &s.run.blocked_on {
        let _ = writeln!(
            w,
            "blocked   {} — {} (since {})",
            b.hitl_id,
            b.summary,
            b.since.to_rfc3339()
        );
    }
    for step in &s.run.steps {
        let _ = writeln!(
            w,
            "  step {:<12} {:<9} {}",
            step.id,
            state_label(step.state),
            step.member.as_deref().unwrap_or("-")
        );
    }
}

pub fn run(mur_home: &Path, action: JobAction) -> Result<()> {
    match action {
        JobAction::List { all } => {
            let mut rows = Vec::new();
            for id in store::list_ids(mur_home)? {
                if let Some(status) = load_status(mur_home, &id)?
                    && (all || visible_in_list(&status))
                {
                    rows.push(status);
                }
            }
            rows.sort_by_key(|r| std::cmp::Reverse(r.run.started_at));
            if rows.is_empty() {
                println!("no runs");
                return Ok(());
            }
            println!("{:<28} {:<9} {:<9} LABEL", "RUN", "STATE", "LIVENESS");
            for s in rows {
                println!(
                    "{:<28} {:<9} {:<9} {}",
                    s.run.run_id,
                    state_label(s.state),
                    liveness_label(s.liveness),
                    s.run.label
                );
            }
            Ok(())
        }
        JobAction::Status { run_id } => {
            let Some(s) = load_status(mur_home, &run_id)? else {
                anyhow::bail!("no run recorded for `{run_id}` (try `mur job list --all`)");
            };
            print_status(&mut std::io::stdout(), &s);
            Ok(())
        }
        JobAction::Stop { run_id } => {
            // MUST go through `update`, not load + save: the executor process
            // for this run may still be beating its heartbeat, and a bare
            // read-modify-write here would be reverted by the next beat —
            // leaving a stopped run reporting `running` forever.
            let mut was_terminal = None;
            let existed = store::update(mur_home, &run_id, |record| {
                if record.state.is_terminal() {
                    was_terminal = Some(record.state);
                    return;
                }
                record.state = State::Stopped;
            })
            .with_context(|| format!("stop run `{run_id}`"))?;
            if !existed {
                anyhow::bail!("no run recorded for `{run_id}`");
            }
            if let Some(state) = was_terminal {
                println!("run {run_id} already {}", state_label(state));
                return Ok(());
            }
            println!("run {run_id} marked stopped");
            // Do not overstate this. An operator who reads "stopped", walks
            // away, and leaves the work running is worse off than one who was
            // told the truth.
            println!(
                "note: this marks the run stopped; a running orchestrator is not signalled and \
                 will continue until it finishes. To stop a fleet's loop, use `mur fleet stop <name>`."
            );
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_status::{Liveness, RUN_SCHEMA, RunKind, RunState, State, classify};

    fn status(state: State, pid: u32, beat: Option<i64>) -> crate::run_status::RunStatus {
        let now = chrono::Utc::now();
        classify(
            RunState {
                schema: RUN_SCHEMA,
                run_id: "r".into(),
                channel_id: None,
                kind: RunKind::Job,
                label: "l".into(),
                pid,
                started_at: now,
                last_heartbeat_at: beat.map(|s| now - chrono::Duration::seconds(s)),
                state,
                steps: vec![],
                blocked_on: None,
                binary_version: "0.0.0-test".into(),
                build_sha: "deadbee".into(),
            },
            now,
            chrono::Duration::seconds(30),
        )
    }

    fn dead_pid() -> u32 {
        let mut c = std::process::Command::new("true").spawn().unwrap();
        let pid = c.id();
        c.wait().unwrap();
        pid
    }

    /// `print_status` is the shared renderer for `mur job status` AND
    /// `mur fleet status` (spec §4: one derivation, many renderers — these
    /// two share even their renderer). Rendering into a writer makes that
    /// sharing testable: the state/liveness lines must appear verbatim.
    #[test]
    fn print_status_renders_state_and_liveness_lines() {
        let s = status(State::Running, std::process::id(), Some(1));
        let mut out = Vec::new();
        print_status(&mut out, &s);
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("state     running\n"),
            "missing the state line: {text}"
        );
        assert!(
            text.contains("liveness  alive\n"),
            "missing the liveness line: {text}"
        );
        assert!(
            text.contains(&format!("pid       {}\n", std::process::id())),
            "missing the pid line: {text}"
        );
    }

    /// A rebuilt record (no heartbeat, pid 0) must not print a bare
    /// `pid 0`, which reads like a real pid out of context — it must say
    /// the pid is unknown and why.
    #[test]
    fn print_status_renders_a_rebuilt_pid_honestly() {
        let s = status(State::Running, 0, None);
        let mut out = Vec::new();
        print_status(&mut out, &s);
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("pid       unknown (record rebuilt)\n"),
            "a rebuilt record did not say the pid is unknown: {text}"
        );
        assert!(
            !text.contains("pid       0\n"),
            "a rebuilt record printed a bare pid 0: {text}"
        );
    }

    /// A crashed run — `running` on disk with no process — is the single most
    /// important row in the list. Filtering it out as "not running" would
    /// hide exactly what the operator came to find.
    #[test]
    fn crashed_run_stays_visible_in_the_default_list() {
        let s = status(State::Running, dead_pid(), Some(1));
        assert_eq!(s.liveness, Liveness::Dead);
        assert!(
            visible_in_list(&s),
            "a crashed run was filtered out of the list"
        );
    }

    #[test]
    fn unfinished_runs_are_visible_and_finished_ones_are_not() {
        let live = std::process::id();
        assert!(visible_in_list(&status(State::Running, live, Some(1))));
        assert!(visible_in_list(&status(State::Blocked, live, Some(1))));
        assert!(visible_in_list(&status(State::Running, live, Some(999))));
        for terminal in [State::Done, State::Failed, State::Stopped] {
            assert!(
                !visible_in_list(&status(terminal, live, Some(1))),
                "{terminal:?} should be hidden without --all"
            );
        }
    }
}

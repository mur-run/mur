//! Built-in `fleet_run` tool: delegated fleet execution.
//!
//! Lets an allowlisted agent (e.g. the concierge) trigger a guarded fleet run
//! — `mur deep-research "<q>"` / `mur fleet run <name> --loop` — WITHOUT
//! holding filesystem write grants on `~/.mur`. The spawned `mur` child
//! inherits this process's kernel sandbox; the narrow carve-ins it needs
//! (`fleet-state/<name>/` for each allowlisted fleet, `commander/`,
//! `conversations/`, `runs/` + spawn of the `mur` binary) are added at seal
//! time by `SandboxPolicy::from_entitlements`, gated on the same config
//! allowlist checked here. `fleets/` — the definitions — stays read-only.
//!
//! Deny-by-default, out-of-model: the gate lives in `~/.mur/config.yaml`
//! (`fleet_run.agents` / `fleet_run.fleets`), which no agent has write access
//! to — a prompt-injected agent cannot widen it. The run itself inherits every
//! fleet guard for free: iteration cap, deadline, budget, `.stopped`
//! kill-switch, commander governance, fail-closed HITL (`yes:false`).

use std::path::PathBuf;

use tokio::process::Command;

use super::{ToolError, ToolExecutor, ToolOutput};
use crate::llm::ToolDef;

pub const FLEET_RUN: &str = "fleet_run";

/// The one fleet with a dedicated CLI verb rather than `fleet run <name>`.
/// The fleet name and the subcommand are the same word by coincidence, so
/// naming it once keeps a rename of either from silently half-applying.
const DEEP_RESEARCH: &str = "deep-research";

mod daemon_request;
mod gate;
mod messages;

pub use gate::live_run_id;
pub use gate::{allowed, enabled_in};
pub(crate) use messages::spawn_error_message;
use messages::unreadable_config_message;

pub struct FleetRunTool {
    pub mur_home: PathBuf,
    /// Canonical (on-disk) name of the agent this runtime hosts.
    pub agent_name: String,
    /// This agent's signing identity, loaded before the sandbox sealed.
    ///
    /// Handed to the spawned child on its stdin pipe so the channel events it
    /// writes are signed. Without it the child cannot read `keys/` — denied to
    /// every sandboxed process on purpose — and every event of an
    /// agent-triggered run went in unsigned. `None` in tests and wherever no
    /// identity was loaded; the child then behaves exactly as it did before.
    pub signing: Option<std::sync::Arc<mur_common::identity::AgentIdentity>>,
    /// `identity.key_version` from the profile, carried with the signature so
    /// a verifier can resolve the right key across a rotation.
    pub key_version: u32,
    /// The conversation's working directory, shared with the bash/file tools.
    /// The spawned `mur fleet run` inherits THIS process's cwd, which is the
    /// runtime's home, not the project the user is in — so the target goes on
    /// argv as `--cwd` instead of being guessed from where the child lands
    /// (#1607). `None` only in tests that never reach the spawn.
    pub session_cwd: Option<crate::tools::fs_policy::SessionCwd>,
}

#[async_trait::async_trait]
impl ToolExecutor for FleetRunTool {
    fn name(&self) -> &str {
        FLEET_RUN
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: FLEET_RUN.into(),
            description: "Dispatch a MUR fleet (agent squad) and return a handle at once: \
{run_id, status: dispatched}. Poll mur_job_status <run_id> (or mur fleet status <fleet>) \
for progress; the result lands in the fleet's channel, not in this reply. `mur fleet jobs \
<fleet> --since <run_id>` scopes the job list to just this dispatch — without --since, \
counts include every job already in the fleet's queue, not only this run's. `goal` is this \
run's goal in place of the fleet's standing one (the run loops toward it; fleet.yaml is not \
changed); `job` is one task run once. For the deep-research fleet pass the research question \
as `goal`. Only fleets allowlisted in the \
user's config can be run; the run is bounded by the fleet's limits (deadline / stuck / \
cost_usd) and by `mur fleet stop`."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "fleet": {
                        "type": "string",
                        "description": "Fleet name, e.g. \"deep-research\""
                    },
                    "goal": {
                        "type": "string",
                        "description": "This run's goal, used in place of the fleet's standing goal for this run only (fleet.yaml is not changed). The fleet loops until the router converges on it or a limit trips. For deep-research: the research question."
                    },
                    "job": {
                        "type": "string",
                        "description": "One task to run once, ahead of the fleet's queue. With `goal` as well, it is queued first and the loop drains it. Not for deep-research."
                    },
                    "cwd": {
                        "type": "string",
                        "description": "Absolute path of the project the fleet should work in. Pass the TARGET project, not the directory you happen to be sitting in. Omitted: your current session directory is used and the members are told it was inferred."
                    }
                },
                "required": ["fleet"]
            }),
        }
    }

    async fn execute(&self, input: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let fleet = input
            .get("fleet")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing required field `fleet`".into()))?
            .to_string();
        let goal = input
            .get("goal")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let job = input
            .get("job")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        // Explicit target wins; otherwise the session cwd, flagged as a guess.
        // Relative paths are refused rather than resolved — against what?
        let (cwd, cwd_inferred) = match input.get("cwd").and_then(|v| v.as_str()) {
            Some(p) => {
                let p = PathBuf::from(p);
                if !p.is_absolute() {
                    return Err(ToolError::InvalidInput(format!(
                        "`cwd` must be an absolute path, got `{}`",
                        p.display()
                    )));
                }
                (Some(p), false)
            }
            None => (self.session_cwd.as_ref().map(|c| c.current()), true),
        };
        // Accepted for one release so an older prompt does not break; the run
        // is bounded by the fleet's limits now, not by how long this call may
        // block (spec §3.6).
        let stale_timeout = input.get("timeout_secs").is_some();

        if !mur_common::fleet::valid_fleet_name(&fleet) {
            return Err(ToolError::InvalidInput(format!(
                "invalid fleet name: {fleet}"
            )));
        }
        if fleet == DEEP_RESEARCH && job.is_some() {
            return Err(ToolError::InvalidInput(
                "deep-research takes its question as `goal`, not `job`".into(),
            ));
        }

        // Deny-by-default authorization gate (re-checked here even though
        // registration already gates: defense in depth, and a precise error).
        // Unreadable still refuses, with an accurate error (#1795): on sealed
        // Linux a rename-replaced config.yaml is an inode Landlock never granted.
        let cfg = mur_common::config::Config::try_load(&self.mur_home.join("config.yaml"))
            .map_err(|e| ToolError::NotAuthorized(unreadable_config_message(&e)))?
            .fleet_run;
        if !allowed(&cfg, &self.agent_name, &fleet) {
            return Err(ToolError::NotAuthorized(mur_common::authz::not_authorized(
                &format!(
                    "fleet_run: agent '{}' / fleet '{fleet}' — the user must add them to \
                     `fleet_run.agents` / `fleet_run.fleets` in ~/.mur/config.yaml \
                     (deny-by-default)",
                    self.agent_name
                ),
            )));
        }

        // Read the fleet to refuse a self-delegating one below; the bounds
        // themselves are the loop's business (`mur limits <fleet>`).
        let fleet_yaml = self
            .mur_home
            .join(mur_common::paths::FLEETS)
            .join(&fleet)
            .join("fleet.yaml");
        let doc = std::fs::read_to_string(&fleet_yaml).map_err(|e| {
            ToolError::Execution(format!("fleet '{fleet}' not found ({e}): {fleet_yaml:?}"))
        })?;
        let parsed: mur_common::fleet::Fleet = serde_yaml_ng::from_str(&doc)
            .map_err(|e| ToolError::Execution(format!("invalid fleet.yaml for '{fleet}': {e}")))?;

        // A fleet that lists THIS agent hands the goal back to the process
        // that triggered the run: the concierge calls `fleet_run`, the run
        // dials `channel/delegate` on the concierge, and the concierge is
        // already inside this tool call waiting for it. Seen live — fleet
        // `develop-rust` listed `mur` among six members and the concierge was
        // delegated its own goal (channel seq 106, 2026-09-09).
        if parsed
            .members
            .iter()
            .any(|m| m.eq_ignore_ascii_case(&self.agent_name))
        {
            return Err(ToolError::Execution(format!(
                "fleet '{fleet}' lists '{}' — the agent triggering this run — as a member, so the \
                 run would delegate the same goal back to itself. Drop that member from \
                 fleet.yaml, or run the fleet from the CLI instead.",
                self.agent_name
            )));
        }
        // No budget gate here since 2.80: every fleet is bounded by its
        // resolved limits (built-in deadline at least), and a cost cap means
        // nothing on a local model — see `mur limits <fleet>`.

        // One progress file per fleet means concurrent runs would overwrite
        // each other's state and make the returned handle lie.
        if let Some(active_run_id) = live_run_id(&self.mur_home, &fleet) {
            return Err(ToolError::Execution(format!(
                "a {fleet} run is already live: run_id {active_run_id}"
            )));
        }

        // The handle: minted here, handed to the child with --run-id, and
        // what the caller polls. One path segment under ~/.mur/runs/.
        let run_id = format!("fleet-{fleet}-{}", uuid::Uuid::now_v7());

        // #2116: a fleet routed by ANOTHER agent must be signed with that
        // agent's key, which this sealed process can neither read nor hand
        // over. Ask the daemon — outside every sandbox — to launch it instead.
        let router = parsed.router_or_concierge();
        if !router.eq_ignore_ascii_case(&self.agent_name) {
            let dispatch = mur_track::fleet_dispatch_request::FleetDispatch {
                requested_by: self.agent_name.clone(),
                fleet: fleet.clone(),
                goal,
                job,
                cwd: cwd.map(|d| d.display().to_string()),
                cwd_inferred,
                run_id,
            };
            return daemon_request::dispatch_via_daemon(self, dispatch, router, stale_timeout);
        }

        let args = dispatch_argv(
            &fleet,
            goal.as_deref(),
            job.as_deref(),
            &run_id,
            cwd.as_deref(),
            cwd_inferred,
        );
        // The child's stdio goes to a log beside its run record — `runs/` is
        // already inside the fleet_run carve-in — because nobody is waiting
        // on this pipe any more.
        let log_dir = self.mur_home.join("runs").join(&run_id);
        std::fs::create_dir_all(&log_dir)
            .map_err(|e| ToolError::Execution(format!("create {}: {e}", log_dir.display())))?;
        let log_path = log_dir.join("fleet_run.log");
        let log = std::fs::File::create(&log_path)
            .map_err(|e| ToolError::Execution(format!("create {}: {e}", log_path.display())))?;
        let log_err = log
            .try_clone()
            .map_err(|e| ToolError::Execution(format!("clone log handle: {e}")))?;

        // The same derivation the sandbox granted (`exec_dirs::mur_cli`), not
        // a PATH lookup — see that function for why the two must be one.
        let mur_bin = crate::exec_dirs::mur_cli();
        let path_var = std::env::var("PATH").ok();
        // The signing capability travels on the child's stdin PIPE, never in
        // argv or the environment: `ps eww` reads another same-uid process's
        // environment, so an env var would hand the key back to the bash tool
        // the sandbox exists to keep it away from. The payload is one line and
        // far under a pipe buffer, so the write never blocks even if an older
        // child never reads it — it just sees EOF.
        let handoff = self
            .signing
            .as_ref()
            .map(|id| mur_common::identity::SigningHandoff {
                agent: self.agent_name.clone(),
                key_version: self.key_version,
                secret: id.secret_bytes_for_handoff(),
            });
        let mut cmd = Command::new(&mur_bin);
        cmd.args(&args)
            .env("PATH", super::bash::augmented_path(path_var.as_deref()))
            .stdout(std::process::Stdio::from(log))
            .stderr(std::process::Stdio::from(log_err))
            // Detached on purpose: the run outlives this tool call and this
            // turn. The fleet's limits and `mur fleet stop` bound it.
            .kill_on_drop(false);
        if handoff.is_some() {
            cmd.stdin(std::process::Stdio::piped())
                .env(mur_common::identity::SIGNING_HANDOFF_ENV, "1");
        } else {
            cmd.stdin(std::process::Stdio::null());
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| ToolError::Execution(spawn_error_message(&mur_bin, &e)))?;
        if let Some(h) = handoff
            && let Some(mut stdin) = child.stdin.take()
        {
            use tokio::io::AsyncWriteExt;
            let mut line = serde_json::to_string(&h).unwrap_or_default();
            line.push('\n');
            // Best-effort: an unwritten handoff leaves the child on the
            // on-disk path, which is loud and fail-closed on its own.
            let _ = stdin.write_all(line.as_bytes()).await;
            let _ = stdin.shutdown().await;
            // Dropped here — the child sees EOF and stops waiting.
        }

        // Not awaited: the handle is the reply (spec §3.6). Dropping a tokio
        // Child with kill_on_drop(false) leaves the process running.
        drop(child);

        Ok(dispatched_reply(&run_id, &fleet, &log_path, stale_timeout)
            .to_string()
            .into())
    }
}

/// The handle every dispatch returns, local spawn or daemon request alike.
fn dispatched_reply(
    run_id: &str,
    fleet: &str,
    log_path: &std::path::Path,
    stale_timeout: bool,
) -> serde_json::Value {
    let mut reply = serde_json::json!({
        "run_id": run_id,
        "fleet": fleet,
        "status": "dispatched",
        "log": log_path,
        "follow": format!(
            "mur_job_status {run_id} · mur fleet status {fleet} — the result lands in the fleet channel, not in this reply"
        ),
    });
    if stale_timeout {
        reply["note"] = serde_json::json!(
            "timeout_secs is ignored since 2.80 — the run is bounded by the fleet's limits (mur limits <fleet>)"
        );
    }
    reply
}

/// The full `mur` argv for one dispatch. Shared with `mur-daemon`, which
/// launches cross-router fleets (#2116): one builder, so the two launch paths
/// cannot drift. Argv only — never a shell — so goal text cannot inject.
pub fn dispatch_argv(
    fleet: &str,
    goal: Option<&str>,
    job: Option<&str>,
    run_id: &str,
    cwd: Option<&std::path::Path>,
    cwd_inferred: bool,
) -> Vec<String> {
    let mut args = child_args(fleet, goal, job);
    args.push("--run-id".into());
    args.push(run_id.into());
    // `mur deep-research` has no `--cwd`: research has no target tree.
    if fleet != DEEP_RESEARCH
        && let Some(dir) = cwd
    {
        args.push("--cwd".into());
        args.push(dir.display().to_string());
        if cwd_inferred {
            args.push("--cwd-inferred".into());
        }
    }
    args
}

/// The `mur` argv for one dispatch (before `--run-id` / `--cwd`).
///
/// `goal` rides as `--goal=<text>` — one token, so text starting with `-`
/// cannot be read as a flag — and is a run-scoped override: `fleet.yaml`'s
/// standing goal is left alone. `job` alone is a one-shot run; with `goal`
/// it is queued first and the loop drains it.
fn child_args(fleet: &str, goal: Option<&str>, job: Option<&str>) -> Vec<String> {
    let mut args: Vec<String> = match (fleet, goal) {
        (DEEP_RESEARCH, Some(g)) => return vec![DEEP_RESEARCH.into(), g.into()],
        _ => vec!["fleet".into(), "run".into(), fleet.into()],
    };
    if let Some(j) = job {
        args.push(j.into());
    }
    match (goal, job) {
        (Some(g), _) => {
            args.push("--loop".into());
            args.push(format!("--goal={g}"));
        }
        (None, None) => args.push("--loop".into()),
        (None, Some(_)) => {}
    }
    args
}

#[cfg(test)]
mod daemon_request_tests;

#[cfg(test)]
mod unreadable_tests;

#[cfg(test)]
mod tests;

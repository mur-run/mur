//! Built-in `fleet_run` tool: delegated fleet execution.
//!
//! Lets an allowlisted agent (e.g. the concierge) trigger a guarded fleet run
//! — `mur deep-research "<q>"` / `mur fleet run <name> --loop` — WITHOUT
//! holding filesystem write grants on `~/.mur`. The spawned `mur` child
//! inherits this process's kernel sandbox; the narrow carve-ins it needs
//! (`fleets/`, `commander/`, `conversations/` + spawn of the `mur` binary)
//! are added at seal time by `SandboxPolicy::from_entitlements`, gated on the
//! same config allowlist checked here.
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
}

/// Is `agent` allowed to run `fleet` per the global config? Deny-by-default:
/// missing section or empty lists deny everything.
pub fn allowed(cfg: &mur_common::config::FleetRunConfig, agent: &str, fleet: &str) -> bool {
    cfg.agents.iter().any(|a| a == agent) && cfg.fleets.iter().any(|f| f == fleet)
}

/// Does the global config allow `agent` to run ANY fleet? Used at tool
/// registration so unauthorized agents never even see the tool.
pub fn agent_enabled(mur_home: &std::path::Path, agent: &str) -> bool {
    let cfg = mur_common::config::Config::load_or_default(&mur_home.join("config.yaml")).fleet_run;
    cfg.agents.iter().any(|a| a == agent) && !cfg.fleets.is_empty()
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
for progress; the result lands in the fleet's channel, not in this reply. For the \
deep-research fleet pass the research question as `goal`. Only fleets allowlisted in the \
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
                        "description": "Research question / job text. For deep-research this becomes the research goal; for other fleets it runs as a one-shot job."
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
        // Accepted for one release so an older prompt does not break; the run
        // is bounded by the fleet's limits now, not by how long this call may
        // block (spec §3.6).
        let stale_timeout = input.get("timeout_secs").is_some();

        if !mur_common::fleet::valid_fleet_name(&fleet) {
            return Err(ToolError::InvalidInput(format!(
                "invalid fleet name: {fleet}"
            )));
        }

        // Deny-by-default authorization gate (re-checked here even though
        // registration already gates: defense in depth, and a precise error).
        let cfg = mur_common::config::Config::load_or_default(&self.mur_home.join("config.yaml"))
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
        let fleet_yaml = self.mur_home.join("fleets").join(&fleet).join("fleet.yaml");
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

        // The handle: minted here, handed to the child with --run-id, and
        // what the caller polls. One path segment under ~/.mur/runs/.
        let run_id = format!("fleet-{fleet}-{}", uuid::Uuid::now_v7());
        // Argv only — never a shell — so goal text cannot inject.
        let mut args: Vec<String> = match (&fleet[..], &goal) {
            (DEEP_RESEARCH, Some(g)) => vec![DEEP_RESEARCH.into(), g.clone()],
            (_, Some(g)) => vec!["fleet".into(), "run".into(), fleet.clone(), g.clone()],
            (_, None) => vec!["fleet".into(), "run".into(), fleet.clone(), "--loop".into()],
        };
        args.push("--run-id".into());
        args.push(run_id.clone());
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
        let mut child = cmd.spawn().map_err(|e| {
            ToolError::Execution(format!(
                "failed to spawn `{}`: {e} — if the runtime sandbox denied the spawn, \
                 restart the agent so the fleet_run carve-ins apply",
                mur_bin.display()
            ))
        })?;
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
        Ok(reply.to_string().into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_config(home: &std::path::Path, yaml: &str) {
        std::fs::create_dir_all(home).unwrap();
        std::fs::write(home.join("config.yaml"), yaml).unwrap();
    }

    fn write_fleet(home: &std::path::Path, name: &str, budget: f64) {
        let dir = home.join("fleets").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("fleet.yaml"),
            format!(
                "name: {name}\ngoal: g\nchannel_id: fleet-{name}\nmembers: []\nloop:\n  trigger: manual\n  budget_usd: {budget}\n"
            ),
        )
        .unwrap();
    }

    #[test]
    fn allowed_is_deny_by_default() {
        let cfg = mur_common::config::FleetRunConfig::default();
        assert!(!allowed(&cfg, "mur", "deep-research"));
        let cfg = mur_common::config::FleetRunConfig {
            agents: vec!["mur".into()],
            fleets: vec!["deep-research".into()],
        };
        assert!(allowed(&cfg, "mur", "deep-research"));
        assert!(!allowed(&cfg, "dr_worker_1", "deep-research"));
        assert!(!allowed(&cfg, "mur", "other"));
    }

    #[tokio::test]
    async fn denies_unauthorized_agent() {
        let tmp = tempfile::tempdir().unwrap();
        write_config(tmp.path(), "{}");
        let tool = FleetRunTool {
            mur_home: tmp.path().to_path_buf(),
            agent_name: "mur".into(),
            signing: None,
            key_version: 0,
        };
        let err = tool
            .execute(serde_json::json!({"fleet": "deep-research"}))
            .await
            .unwrap_err();
        assert!(
            matches!(err, ToolError::NotAuthorized(_)),
            "a refusal is NotAuthorized, not Execution: {err}"
        );
        assert!(err.to_string().contains("not authorized"), "{err}");
    }

    #[tokio::test]
    async fn refuses_a_fleet_that_lists_the_triggering_agent() {
        let tmp = tempfile::tempdir().unwrap();
        write_config(
            tmp.path(),
            "fleet_run:\n  agents: [mur]\n  fleets: [selfy]\n",
        );
        let dir = tmp.path().join("fleets").join("selfy");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("fleet.yaml"),
            "name: selfy\ngoal: g\nchannel_id: fleet-selfy\nmembers: [qa, Mur]\nloop:\n  trigger: manual\n  budget_usd: 1.0\n",
        )
        .unwrap();
        let tool = FleetRunTool {
            mur_home: tmp.path().to_path_buf(),
            agent_name: "mur".into(),
            signing: None,
            key_version: 0,
        };
        let err = tool
            .execute(serde_json::json!({"fleet": "selfy"}))
            .await
            .unwrap_err();
        // Matched case-insensitively, like every other agent-name lookup.
        assert!(err.to_string().contains("as a member"), "{err}");
    }

    #[tokio::test]
    async fn rejects_invalid_fleet_name() {
        let tmp = tempfile::tempdir().unwrap();
        write_config(tmp.path(), "{}");
        let tool = FleetRunTool {
            mur_home: tmp.path().to_path_buf(),
            agent_name: "mur".into(),
            signing: None,
            key_version: 0,
        };
        let err = tool
            .execute(serde_json::json!({"fleet": "../etc"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("invalid fleet name"), "{err}");
    }

    #[test]
    fn agent_enabled_gates_registration() {
        let tmp = tempfile::tempdir().unwrap();
        write_config(tmp.path(), "{}");
        assert!(!agent_enabled(tmp.path(), "mur"));
        write_config(
            tmp.path(),
            "fleet_run:\n  agents: [mur]\n  fleets: [deep-research]\n",
        );
        assert!(agent_enabled(tmp.path(), "mur"));
        assert!(!agent_enabled(tmp.path(), "dr_worker_1"));
    }

    #[test]
    fn def_schema_requires_fleet() {
        let tool = FleetRunTool {
            mur_home: PathBuf::from("/tmp"),
            agent_name: "mur".into(),
            signing: None,
            key_version: 0,
        };
        let def = tool.def();
        assert_eq!(def.name, FLEET_RUN);
        assert_eq!(def.input_schema["required"][0], "fleet");
    }

    /// §3.6: the tool returns a handle within a second while the fleet keeps
    /// running, and the child got the id it will be polled under. nextest
    /// runs each test in its own process, so MUR_BIN is private to this one.
    #[cfg(unix)]
    #[tokio::test]
    async fn fleet_run_returns_a_handle_and_leaves_the_child_running() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        write_config(
            home,
            "fleet_run:\n  agents: [mur]\n  fleets: [deep-research]\n",
        );
        write_fleet(home, "deep-research", 0.0);
        let argv_log = home.join("argv.txt");
        let fake = home.join("fake-mur");
        std::fs::write(
            &fake,
            format!("#!/bin/sh\necho \"$@\" > {}\nsleep 5\n", argv_log.display()),
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        unsafe { std::env::set_var("MUR_BIN", &fake) };
        let tool = FleetRunTool {
            mur_home: home.to_path_buf(),
            agent_name: "mur".into(),
            signing: None,
            key_version: 0,
        };
        let t0 = std::time::Instant::now();
        let out = tool
            .execute(serde_json::json!({"fleet": "deep-research", "goal": "why is the sky blue"}))
            .await
            .unwrap();
        unsafe { std::env::remove_var("MUR_BIN") };
        assert!(
            t0.elapsed() < std::time::Duration::from_secs(2),
            "returned in {:?}",
            t0.elapsed()
        );
        let v: serde_json::Value = serde_json::from_str(&out.text).expect("json handle");
        assert_eq!(v["status"], "dispatched");
        let run_id = v["run_id"].as_str().unwrap().to_string();
        assert!(run_id.starts_with("fleet-deep-research-"), "{run_id}");
        assert!(v["follow"].as_str().unwrap().contains("mur_job_status"));
        // the child is still alive and was told the id
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        let argv = std::fs::read_to_string(&argv_log).unwrap();
        assert!(argv.contains(&format!("--run-id {run_id}")), "{argv}");
        assert!(
            argv.starts_with("deep-research why is the sky blue"),
            "{argv}"
        );
        assert!(std::path::Path::new(v["log"].as_str().unwrap()).exists());
    }
}

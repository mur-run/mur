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

use super::{ToolError, ToolExecutor};
use crate::llm::ToolDef;

pub const FLEET_RUN: &str = "fleet_run";

/// The one fleet with a dedicated CLI verb rather than `fleet run <name>`.
/// The fleet name and the subcommand are the same word by coincidence, so
/// naming it once keeps a rename of either from silently half-applying.
const DEEP_RESEARCH: &str = "deep-research";

/// Default / ceiling for how long a run may take before the child is killed.
/// Fleet loops are long-lived (multi-iteration research runs take minutes);
/// the ceiling keeps a wedged loop from pinning a tool slot forever.
const DEFAULT_TIMEOUT_SECS: u64 = 1800;
const MAX_TIMEOUT_SECS: u64 = 3600;

/// Cap on returned combined output — the tail is what matters (converged
/// report / guard verdict); the CompressHook handles anything still large.
const MAX_OUTPUT_CHARS: usize = 16_000;

pub struct FleetRunTool {
    pub mur_home: PathBuf,
    /// Canonical (on-disk) name of the agent this runtime hosts.
    pub agent_name: String,
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

fn resolve_timeout_secs(requested: Option<i64>) -> u64 {
    match requested {
        Some(secs) if secs >= 1 => (secs as u64).min(MAX_TIMEOUT_SECS),
        _ => DEFAULT_TIMEOUT_SECS,
    }
}

#[async_trait::async_trait]
impl ToolExecutor for FleetRunTool {
    fn name(&self) -> &str {
        FLEET_RUN
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: FLEET_RUN.into(),
            description: format!(
                "Run a MUR fleet (agent squad) as a guarded, budgeted loop and return its output. \
For the deep-research fleet pass the research question as `goal`. \
Only fleets allowlisted in the user's config can be run; the run stops on \
convergence, iteration cap, deadline, budget, or kill-switch. \
Long-running: default timeout {DEFAULT_TIMEOUT_SECS}s (max {MAX_TIMEOUT_SECS}s)."
            ),
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
                    },
                    "timeout_secs": {
                        "type": "integer",
                        "description": "Seconds before the run is killed"
                    }
                },
                "required": ["fleet"]
            }),
        }
    }

    async fn execute(&self, input: serde_json::Value) -> Result<String, ToolError> {
        let fleet = input
            .get("fleet")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing required field `fleet`".into()))?
            .to_string();
        let goal = input
            .get("goal")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let timeout_secs = resolve_timeout_secs(input.get("timeout_secs").and_then(|v| v.as_i64()));

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
            return Err(ToolError::Execution(format!(
                "fleet_run denied: agent '{}' / fleet '{fleet}' not authorized — the user must \
                 add them to `fleet_run.agents` / `fleet_run.fleets` in ~/.mur/config.yaml \
                 (deny-by-default)",
                self.agent_name
            )));
        }

        // Safety triad parity with unattended auto-run: an agent-triggered run
        // must have an enforced budget. Refuse fleets without one.
        let fleet_yaml = self.mur_home.join("fleets").join(&fleet).join("fleet.yaml");
        let doc = std::fs::read_to_string(&fleet_yaml).map_err(|e| {
            ToolError::Execution(format!("fleet '{fleet}' not found ({e}): {fleet_yaml:?}"))
        })?;
        let parsed: mur_common::fleet::Fleet = serde_yaml_ng::from_str(&doc)
            .map_err(|e| ToolError::Execution(format!("invalid fleet.yaml for '{fleet}': {e}")))?;
        if parsed.loop_cfg.map(|l| l.budget_usd).unwrap_or(0.0) <= 0.0 {
            return Err(ToolError::Execution(format!(
                "fleet '{fleet}' has no budget (`loop.budget_usd`) — agent-triggered runs require \
                 a positive budget; set one with the fleet config or `mur deep-research setup`"
            )));
        }

        // Argv only — never a shell — so goal text cannot inject.
        let args: Vec<String> = match (&fleet[..], &goal) {
            (DEEP_RESEARCH, Some(g)) => vec![DEEP_RESEARCH.into(), g.clone()],
            (_, Some(g)) => vec!["fleet".into(), "run".into(), fleet.clone(), g.clone()],
            (_, None) => vec!["fleet".into(), "run".into(), fleet.clone(), "--loop".into()],
        };

        let mur_bin = std::env::var("MUR_BIN").unwrap_or_else(|_| "mur".into());
        let path_var = std::env::var("PATH").ok();
        let child = Command::new(&mur_bin)
            .args(&args)
            .env("PATH", super::bash::augmented_path(path_var.as_deref()))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                ToolError::Execution(format!(
                    "failed to spawn `{mur_bin}`: {e} — if the runtime sandbox denied the spawn, \
                     restart the agent so the fleet_run carve-ins apply"
                ))
            })?;

        let out = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            child.wait_with_output(),
        )
        .await
        .map_err(|_| {
            ToolError::Execution(format!(
                "fleet run timed out after {timeout_secs}s and was killed; the fleet's own \
                 guards (.last state, kill-switch) remain authoritative — check `mur fleet show {fleet}`"
            ))
        })?
        .map_err(|e| ToolError::Execution(format!("fleet run failed: {e}")))?;

        let mut combined = String::from_utf8_lossy(&out.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&out.stderr);
        if !stderr.trim().is_empty() {
            combined.push_str("\n--- stderr ---\n");
            combined.push_str(&stderr);
        }
        if !out.status.success() {
            combined.push_str(&format!("\n[exit status: {}]", out.status));
        }
        // Keep the tail — convergence verdict / report location print last.
        if combined.chars().count() > MAX_OUTPUT_CHARS {
            let tail: String = combined
                .chars()
                .skip(combined.chars().count() - MAX_OUTPUT_CHARS)
                .collect();
            combined = format!("[output truncated to last {MAX_OUTPUT_CHARS} chars]\n…{tail}");
        }
        Ok(combined)
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
        };
        let err = tool
            .execute(serde_json::json!({"fleet": "deep-research"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not authorized"), "{err}");
    }

    #[tokio::test]
    async fn rejects_invalid_fleet_name() {
        let tmp = tempfile::tempdir().unwrap();
        write_config(tmp.path(), "{}");
        let tool = FleetRunTool {
            mur_home: tmp.path().to_path_buf(),
            agent_name: "mur".into(),
        };
        let err = tool
            .execute(serde_json::json!({"fleet": "../etc"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("invalid fleet name"), "{err}");
    }

    #[tokio::test]
    async fn refuses_fleet_without_budget() {
        let tmp = tempfile::tempdir().unwrap();
        write_config(
            tmp.path(),
            "fleet_run:\n  agents: [mur]\n  fleets: [deep-research]\n",
        );
        write_fleet(tmp.path(), "deep-research", 0.0);
        let tool = FleetRunTool {
            mur_home: tmp.path().to_path_buf(),
            agent_name: "mur".into(),
        };
        let err = tool
            .execute(serde_json::json!({"fleet": "deep-research"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("budget"), "{err}");
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
        };
        let def = tool.def();
        assert_eq!(def.name, FLEET_RUN);
        assert_eq!(def.input_schema["required"][0], "fleet");
    }
}

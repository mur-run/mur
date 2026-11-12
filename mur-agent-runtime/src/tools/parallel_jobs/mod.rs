//! Built-in `parallel_jobs` tool (#2379): a signed request to `mur-daemon`.
//!
//! The MCP tool of the same name runs inside `mur-mcp-server`, which this
//! runtime spawns inside its own sandbox. That process cannot read `keys/`, so
//! the run it starts writes its channel events unsigned and its write-grant
//! gate can never park an answerable `HitlRequest`. This tool replaces it for
//! sandboxed agents: it signs a `JobsDispatchRequest` with THIS agent's key
//! (loaded before the seal), drops it in the daemon's inbox, and returns the
//! same handle the MCP tool did. The daemon, outside every sandbox, verifies
//! the signature, re-applies `parallel_jobs.{requesters,targets}` from the
//! user's config, and launches `mur job run-request`, which reads the router
//! key from disk like a terminal run.
//!
//! There is no `yes`: an agent may not approve its own write grant (spec Q3).
//! The request type has no field for it, so this tool cannot forward one.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use mur_common::config::ParallelJobsConfig;
use mur_common::identity::AgentIdentity;
use mur_track::jobs_dispatch_request::{
    JOBS_DISPATCH_REQUEST_DIR, JOBS_RUN_LOG_FILE, JobsDispatch, JobsDispatchRequest, MAX_JOBS,
    RequestedJob,
};

use super::{ToolError, ToolExecutor, ToolOutput};
use crate::llm::ToolDef;

pub const PARALLEL_JOBS: &str = "parallel_jobs";

/// Jobs in flight at once when the model names no limit. Same default as the
/// MCP tool, so switching an agent over changes nothing it can observe.
const DEFAULT_MAX_CONCURRENCY: u64 = 8;

/// May `agent` see this tool? Exact canonical match, like `fleet_run`.
/// Targets empty means `parallel_jobs` can reach nobody, so there is nothing
/// to offer; requesters empty means "any agent" (spec Q1). The daemon and its
/// child re-check both against the live config, so this is only about which
/// tool the model is shown.
pub fn enabled_in(cfg: &ParallelJobsConfig, agent: &str) -> bool {
    !cfg.targets.is_empty()
        && (cfg.requesters.is_empty() || cfg.requesters.iter().any(|r| r == agent))
}

/// Wire names of MUR's own MCP `parallel_jobs` that the built-in replaces, one
/// per enabled `mur-mcp-server` entry. Matched on the binary's file name: a
/// third-party server that happens to list a `parallel_jobs` is not ours to
/// hide.
pub fn shadowed_mcp_names(servers: &[mur_common::agent::McpServerEntry]) -> Vec<String> {
    servers
        .iter()
        .filter(|s| {
            Path::new(&s.command)
                .file_stem()
                .is_some_and(|n| n == "mur-mcp-server")
        })
        .map(|s| super::naming::wire_name(&super::naming::sanitize_server(&s.name), PARALLEL_JOBS))
        .collect()
}

/// Register the built-in for `tool.agent_name` when the config offers it and
/// no profile rule denies it or the MCP tool it replaces, removing MUR's own MCP `parallel_jobs` so the
/// model sees one tool with one schema. Returns the removed wire names, which
/// the MCP refresh must keep out too. A no-op (empty) otherwise: the MCP tool
/// stays, exactly as before.
pub fn register(
    tool_map: &mut std::collections::HashMap<String, Arc<dyn ToolExecutor>>,
    cfg: &ParallelJobsConfig,
    rules: &[mur_common::agent::ToolRule],
    mcp: &[mur_common::agent::McpServerEntry],
    tool: ParallelJobsTool,
) -> Vec<String> {
    use mur_common::agent::{ToolPolicy, resolve_tool_policy};
    let shadowed = shadowed_mcp_names(mcp);
    // A deny on the MCP tool this replaces (exact, or a `mcp__mur-tools__*`
    // wildcard) was written before the built-in existed and means "this
    // agent may not fan out". Honour it: the replacement must not be a way
    // around a rule the user already has.
    let denied = |name: &str| resolve_tool_policy(rules, name) == ToolPolicy::Deny;
    if !enabled_in(cfg, &tool.agent_name)
        || denied(PARALLEL_JOBS)
        || shadowed.iter().any(|n| denied(n))
    {
        return Vec::new();
    }
    for name in &shadowed {
        tool_map.remove(name);
    }
    tool_map.insert(PARALLEL_JOBS.to_string(), Arc::new(tool));
    shadowed
}

pub struct ParallelJobsTool {
    pub mur_home: PathBuf,
    /// Canonical (on-disk) name of the agent this runtime hosts.
    pub agent_name: String,
    /// Loaded before the sandbox sealed; the only key this process holds.
    pub signing: Option<Arc<AgentIdentity>>,
    pub key_version: u32,
    /// Where the jobs go when the model names no `cwd` (#1607).
    pub session_cwd: Option<crate::tools::fs_policy::SessionCwd>,
}

#[async_trait::async_trait]
impl ToolExecutor for ParallelJobsTool {
    fn name(&self) -> &str {
        PARALLEL_JOBS
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: PARALLEL_JOBS.into(),
            description: "Fan out N distinct jobs to running MUR agents in parallel and return a \
handle at once: {run_id, channel_id, status: dispatched}. Poll mur_job_status <run_id> for \
progress; each job's reply lands in the channel. The MUR daemon launches the run, so it outlives \
this conversation. Before coding fan-out, apply the parallel-code gate: disjoint files (no shared \
registry/lockfile), contracts frozen first, one writer per file. Only agents in the user's \
`parallel_jobs.targets` can be named, and their runtimes must already be running. Pass `cwd` \
(absolute) when the jobs belong to a specific project; each job is told to work there, and each \
agent must be allowed to write there (an approval is parked for the user otherwise — you cannot \
approve it yourself)."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "jobs": {
                        "type": "array",
                        "description": "Jobs to run in parallel. Each: { description: string, agent?: string }.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "description": { "type": "string" },
                                "agent": { "type": "string" }
                            },
                            "required": ["description"]
                        }
                    },
                    "agent": {
                        "type": "string",
                        "description": "Default assignee agent name for jobs that omit their own `agent`."
                    },
                    "max_concurrency": {
                        "type": "integer",
                        "description": "Max jobs in flight at once, 1-32 (default 8)."
                    },
                    "cwd": {
                        "type": "string",
                        "description": "Absolute path of the project the jobs should work in — the TARGET, not wherever you happen to be sitting. Omitted: your session directory is assumed and the jobs are told it was a guess."
                    }
                },
                "required": ["jobs"]
            }),
        }
    }

    async fn execute(&self, input: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let parsed = parse_input(&input, self.session_cwd.as_ref())?;

        // Precise errors before anything is written. Not the gate: the daemon
        // and its child re-derive every grant from config.yaml themselves.
        if let Ok(cfg) = mur_common::config::Config::try_load(&self.mur_home.join("config.yaml")) {
            precheck(&cfg.parallel_jobs, &self.agent_name, &parsed.jobs)?;
        }
        let Some(identity) = self.signing.as_ref() else {
            return Err(ToolError::Execution(
                "parallel_jobs: this agent has no signing key, so the MUR daemon could not verify \
                 the request. Run the jobs from a terminal instead (`mur job`)."
                    .into(),
            ));
        };
        let dispatch_cfg =
            mur_common::config::Config::load_or_default(&self.mur_home.join("config.yaml"))
                .fleet_dispatch;
        // A request nobody sweeps is a run that never starts behind a handle
        // that says "dispatched".
        if !crate::tools::fleet_run::daemon_request::daemon_alive(
            &self.mur_home,
            dispatch_cfg.heartbeat_stale_secs,
            chrono::Utc::now(),
        ) {
            return Err(ToolError::Execution(
                "parallel_jobs is launched by the MUR daemon (it signs the run's approvals; this \
                 agent cannot), and the daemon is not running. Start it with `mur daemon start`, \
                 then dispatch again."
                    .into(),
            ));
        }

        let dispatch = JobsDispatch {
            requested_by: self.agent_name.clone(),
            jobs: parsed.jobs,
            max_concurrency: parsed.max_concurrency,
            cwd: parsed.cwd.map(|d| d.display().to_string()),
            cwd_inferred: parsed.cwd_inferred,
            run_id: format!("run-{}", uuid::Uuid::now_v7()),
            channel_id: uuid::Uuid::now_v7().to_string(),
        };
        dispatch
            .check_shape()
            .map_err(|e| ToolError::InvalidInput(format!("parallel_jobs: {e}")))?;
        let req =
            JobsDispatchRequest::create(dispatch, identity, self.key_version, chrono::Utc::now());
        write_request(&self.mur_home, &req)
            .map_err(|e| ToolError::Execution(format!("write parallel_jobs request: {e}")))?;
        Ok(reply(&self.mur_home, &req, parsed.dropped_yes)
            .to_string()
            .into())
    }
}

/// The model's arguments, validated. `pub(crate)` for the tests.
#[derive(Debug)]
pub(crate) struct Parsed {
    pub jobs: Vec<RequestedJob>,
    pub max_concurrency: u32,
    pub cwd: Option<PathBuf>,
    pub cwd_inferred: bool,
    /// The model passed `yes: true` — said back so it does not assume it.
    pub dropped_yes: bool,
}

pub(crate) fn parse_input(
    input: &serde_json::Value,
    session_cwd: Option<&crate::tools::fs_policy::SessionCwd>,
) -> Result<Parsed, ToolError> {
    let raw = input
        .get("jobs")
        .and_then(|v| v.as_array())
        .ok_or_else(|| ToolError::InvalidInput("missing required field `jobs` (array)".into()))?;
    if raw.is_empty() || raw.len() > MAX_JOBS {
        return Err(ToolError::InvalidInput(format!(
            "`jobs` must have 1..={MAX_JOBS} entries (got {})",
            raw.len()
        )));
    }
    let default_agent = input.get("agent").and_then(|v| v.as_str());
    let mut jobs = Vec::with_capacity(raw.len());
    for (i, j) in raw.iter().enumerate() {
        let description = j
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let agent = j
            .get("agent")
            .and_then(|v| v.as_str())
            .or(default_agent)
            .ok_or_else(|| {
                ToolError::InvalidInput(format!(
                    "job {i} has no `agent` and no default `agent` was given"
                ))
            })?
            .to_string();
        jobs.push(RequestedJob { description, agent });
    }
    let max_concurrency = input
        .get("max_concurrency")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_MAX_CONCURRENCY)
        .clamp(1, MAX_JOBS as u64) as u32;
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
        None => (session_cwd.map(|c| c.current()), true),
    };
    let dropped_yes = input.get("yes").and_then(|v| v.as_bool()).unwrap_or(false);
    Ok(Parsed {
        jobs,
        max_concurrency,
        cwd,
        cwd_inferred,
        dropped_yes,
    })
}

/// The same rules the daemon applies, said early and precisely. Case-folded
/// here because this process cannot list `agents/` to canonicalize; the
/// daemon's exact check is the one that counts.
pub(crate) fn precheck(
    cfg: &ParallelJobsConfig,
    agent: &str,
    jobs: &[RequestedJob],
) -> Result<(), ToolError> {
    if !enabled_in(cfg, agent) && !cfg.targets.is_empty() {
        return Err(ToolError::NotAuthorized(mur_common::authz::not_authorized(
            &format!(
                "parallel_jobs: agent '{agent}' is not in `parallel_jobs.requesters` in \
                 ~/.mur/config.yaml"
            ),
        )));
    }
    for j in jobs {
        if j.agent.eq_ignore_ascii_case(agent) {
            return Err(ToolError::InvalidInput(format!(
                "parallel_jobs: a job may not be assigned to '{agent}', the agent dispatching it"
            )));
        }
        if !cfg.targets.iter().any(|t| t.eq_ignore_ascii_case(&j.agent)) {
            return Err(ToolError::NotAuthorized(mur_common::authz::not_authorized(
                &format!(
                    "parallel_jobs: target '{}' is not in `parallel_jobs.targets` in \
                     ~/.mur/config.yaml",
                    j.agent
                ),
            )));
        }
    }
    Ok(())
}

/// Atomically drop the request: tmp + rename, named by its (unique) run id.
pub(crate) fn write_request(mur_home: &Path, req: &JobsDispatchRequest) -> anyhow::Result<()> {
    let dir = mur_home.join(JOBS_DISPATCH_REQUEST_DIR);
    std::fs::create_dir_all(&dir)?;
    let name = format!("{}.yaml", req.dispatch.run_id);
    let tmp = dir.join(format!(".{name}.tmp"));
    std::fs::write(&tmp, serde_yaml_ng::to_string(req)?)?;
    std::fs::rename(&tmp, dir.join(name))?;
    Ok(())
}

fn reply(mur_home: &Path, req: &JobsDispatchRequest, dropped_yes: bool) -> serde_json::Value {
    let d = &req.dispatch;
    let log = mur_track::jobs_dispatch_request::jobs_dispatch_run_dir(mur_home, &d.run_id)
        .join(JOBS_RUN_LOG_FILE);
    let mut out = serde_json::json!({
        "run_id": d.run_id,
        "channel_id": d.channel_id,
        "status": "dispatched",
        "jobs": d.jobs.len(),
        "via": "daemon",
        "log": log,
        "follow": format!(
            "mur_job_status {} — each job's reply lands in channel {}",
            d.run_id, d.channel_id
        ),
    });
    if dropped_yes {
        out["note"] = serde_json::json!(
            "`yes` was ignored: write-grant approvals go to the user, never to the dispatching agent"
        );
    }
    out
}

#[cfg(test)]
mod tests;

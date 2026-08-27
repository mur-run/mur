//! Ephemeral parallel-jobs fan-out: build an in-memory DAG of rank-0
//! `delegate_to` steps (one per job) and run it through `execute_dag` — no
//! authored workflow/skill file. Generalizes fleet broadcast (`cmd/fleet/run.rs`)
//! to per-job prompts with a free assignee. See
//! `docs/superpowers/specs/2026-06-24-parallel-jobs-dynamic-fanout-design.md`.

use std::collections::HashSet;
use std::path::Path;

use anyhow::{Result, anyhow, bail};
use mur_channel::ChannelService;
use mur_common::config::Config;
use mur_common::pipeline::PipelineOutput;
use mur_common::skill::manifest::{Procedure, ProcedureStep};

use crate::a2a_dial::canonicalize_agent_name;
use crate::executor::dag::{DagExecOptions, execute_dag};

/// A single job: a prompt and the (canonicalized) agent to delegate it to.
pub struct Job {
    pub description: String,
    pub assignee: String,
}

/// One rank-0 `ProcedureStep` per job (all parallel, no deps). Sets BOTH
/// `intent` (the delegate prompt) and `description` (channel/ledger labels)
/// to the job text, and a stable unique `id` for idempotency / crash-resume.
pub fn build_jobs_procedure(jobs: &[Job]) -> Procedure {
    Procedure {
        variables: vec![],
        steps: jobs
            .iter()
            .enumerate()
            .map(|(i, j)| ProcedureStep {
                description: j.description.clone(),
                intent: Some(j.description.clone()),
                delegate_to: Some(j.assignee.clone()),
                id: Some(format!("job-{i}")),
                ..Default::default()
            })
            .collect(),
    }
}

/// Untyped job as it arrives from the MCP tool: a description and an optional
/// explicit assignee. Resolved into a `Job` by `resolve_jobs`.
pub struct RawJob {
    pub description: String,
    pub agent: Option<String>,
}

/// Resolve each `RawJob` to a `Job` with a concrete, canonicalized assignee.
/// Precedence per job: explicit `agent` -> `default_agent` -> error.
/// Rejects empty descriptions. Names are canonicalized so the runtime
/// spoof check passes (case-insensitive on-disk match, else used verbatim).
pub fn resolve_jobs(
    mur_home: &Path,
    raw: &[RawJob],
    default_agent: Option<&str>,
) -> Result<Vec<Job>> {
    if raw.is_empty() {
        bail!("no jobs provided");
    }
    raw.iter()
        .enumerate()
        .map(|(i, j)| {
            if j.description.trim().is_empty() {
                bail!("job {i} has an empty description");
            }
            let assignee = j.agent.as_deref().or(default_agent).ok_or_else(|| {
                anyhow!(
                    "job {i} has no assignee: pass per-job `agent` or a top-level default `agent`"
                )
            })?;
            Ok(Job {
                description: j.description.clone(),
                assignee: canonicalize_agent_name(mur_home, assignee),
            })
        })
        .collect()
}

/// Pure deterministic gate: every job's assignee must be in `allow`.
/// Fail-closed: any miss returns an Err immediately. Called before any
/// channel mint or dial so a prompt-injected concierge cannot widen the
/// target set (OWASP Agentic ASI02/03/04).
fn check_authorization(allow: &HashSet<String>, jobs: &[Job], config_path: &Path) -> Result<()> {
    for j in jobs {
        if !allow.contains(&j.assignee) {
            bail!(
                "target '{}' not authorized for parallel_jobs (deny-by-default) — add it under \
                 `parallel_jobs.targets` in {}, e.g.\n\nparallel_jobs:\n  targets:\n    - {}",
                j.assignee,
                config_path.display(),
                j.assignee
            );
        }
    }
    Ok(())
}

/// Load the allowlist from config, canonicalize each entry, then verify all
/// jobs' targets are in the allowlist. Deterministic, pre-action, fail-closed.
/// Mirrors the `verified_active_fleet` pattern (any miss is a denial, never
/// fail-open).
fn authorize_targets(mur_home: &Path, jobs: &[Job]) -> Result<()> {
    let config_path = mur_home.join("config.yaml");
    let cfg = Config::load_or_default(&config_path);
    let allow: HashSet<String> = cfg
        .parallel_jobs
        .targets
        .iter()
        .map(|t| canonicalize_agent_name(mur_home, t))
        .collect();
    check_authorization(&allow, jobs, &config_path)
}

/// Run N jobs as one ephemeral, channel-recorded DAG. Mints a throwaway
/// workflow channel, fans the jobs out (bounded by `max_concurrency`), and
/// returns `(channel_id, output)`. Per-job replies are persisted on the
/// channel; the caller reads them back via `channel_id`. `yes` is passed
/// straight through — `false` keeps risk-tiered steps fail-closed at the HITL gate.
pub async fn run_parallel_jobs(
    mur_home: &Path,
    jobs: &[Job],
    max_concurrency: Option<usize>,
    yes: bool,
) -> Result<(String, PipelineOutput)> {
    authorize_targets(mur_home, jobs)?;
    let proc = build_jobs_procedure(jobs);
    let svc = ChannelService::open(mur_home)?;
    let channel_id = svc.create_for_workflow("parallel-jobs")?.id;
    let opts = DagExecOptions {
        yes,
        trigger: "agent",
        channel_id: Some(channel_id.clone()),
        run_id: format!("run-{}", uuid::Uuid::now_v7()),
        run_kind: Some(crate::run_status::RunKind::Job),
        run_label: format!("{} parallel job(s)", jobs.len()),
        max_concurrency,
        ..Default::default()
    };
    // `job:` keeps the run ledger out of the skill store — this fan-out is
    // ephemeral and owns no skill.yaml. See `skill::event_log::event_log_path`.
    let out = execute_dag(mur_home, "job:parallel-jobs", &proc, &opts)
        .await
        .map_err(|e| anyhow::anyhow!("parallel_jobs run on channel {channel_id} failed: {e}"))?;
    Ok((channel_id, out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_jobs_procedure_one_rank0_step_per_job() {
        let jobs = vec![
            Job {
                description: "add caching to fetch".into(),
                assignee: "rustsmith".into(),
            },
            Job {
                description: "write the README".into(),
                assignee: "frontend".into(),
            },
        ];
        let p = build_jobs_procedure(&jobs);
        assert_eq!(p.steps.len(), 2);
        // delegate target per job
        assert_eq!(p.steps[0].delegate_to.as_deref(), Some("rustsmith"));
        assert_eq!(p.steps[1].delegate_to.as_deref(), Some("frontend"));
        // BOTH intent (prompt) and description (labels) carry the job text
        assert_eq!(p.steps[0].intent.as_deref(), Some("add caching to fetch"));
        assert_eq!(p.steps[0].description, "add caching to fetch");
        // stable, unique ids
        assert_eq!(p.steps[0].id.as_deref(), Some("job-0"));
        assert_eq!(p.steps[1].id.as_deref(), Some("job-1"));
        // all rank-0 (no dependencies => all parallel)
        assert!(p.steps.iter().all(|s| s.depends_on.is_empty()));
    }

    #[test]
    fn resolve_jobs_precedence_and_validation() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();

        // per-job agent wins over the default
        let raw = vec![RawJob {
            description: "A".into(),
            agent: Some("rustsmith".into()),
        }];
        let jobs = resolve_jobs(home, &raw, Some("frontend")).unwrap();
        assert_eq!(jobs[0].assignee, "rustsmith");

        // falls back to the default agent when a job omits its own
        let raw = vec![RawJob {
            description: "B".into(),
            agent: None,
        }];
        let jobs = resolve_jobs(home, &raw, Some("frontend")).unwrap();
        assert_eq!(jobs[0].assignee, "frontend");

        // error when neither a per-job nor a default agent is set
        let raw = vec![RawJob {
            description: "C".into(),
            agent: None,
        }];
        assert!(resolve_jobs(home, &raw, None).is_err());

        // error on an empty description
        let raw = vec![RawJob {
            description: "  ".into(),
            agent: Some("rustsmith".into()),
        }];
        assert!(resolve_jobs(home, &raw, None).is_err());
    }

    // ── check_authorization unit tests ───────────────────────────────────────

    #[test]
    fn check_authorization_is_deny_by_default() {
        // Empty allowlist => every target is rejected.
        let allow: HashSet<String> = HashSet::new();
        let jobs = vec![Job {
            description: "a".into(),
            assignee: "rustsmith".into(),
        }];
        assert!(
            check_authorization(&allow, &jobs, Path::new("config.yaml")).is_err(),
            "empty allowlist must reject any target"
        );
    }

    #[test]
    fn check_authorization_permits_listed_target() {
        let allow: HashSet<String> = ["rustsmith".to_string()].into();
        let jobs = vec![Job {
            description: "a".into(),
            assignee: "rustsmith".into(),
        }];
        assert!(
            check_authorization(&allow, &jobs, Path::new("config.yaml")).is_ok(),
            "allowlisted target must be permitted"
        );
    }

    #[test]
    fn check_authorization_rejects_unlisted_target() {
        let allow: HashSet<String> = ["rustsmith".to_string()].into();
        let jobs = vec![Job {
            description: "a".into(),
            assignee: "unknown-agent".into(),
        }];
        let err = check_authorization(&allow, &jobs, Path::new("config.yaml")).unwrap_err();
        assert!(
            err.to_string().contains("not authorized"),
            "error must mention authorization: {err}"
        );
    }

    #[test]
    fn check_authorization_rejects_first_unlisted_in_mixed_list() {
        // First job is allowed, second is not — gate must fail on the unlisted one.
        let allow: HashSet<String> = ["rustsmith".to_string()].into();
        let jobs = vec![
            Job {
                description: "a".into(),
                assignee: "rustsmith".into(),
            },
            Job {
                description: "b".into(),
                assignee: "intruder".into(),
            },
        ];
        assert!(
            check_authorization(&allow, &jobs, Path::new("config.yaml")).is_err(),
            "must reject when any target is not in the allowlist"
        );
    }

    // ── run_parallel_jobs integration: gate blocks before channel mint ───────

    #[tokio::test]
    async fn run_parallel_jobs_blocked_by_empty_allowlist() {
        // No config.yaml => empty allowlist => gate rejects before any channel is minted.
        let tmp = tempfile::TempDir::new().unwrap();
        let jobs = vec![Job {
            description: "do A".into(),
            assignee: "nonexistent-agent-xyz".into(),
        }];
        let result = run_parallel_jobs(tmp.path(), &jobs, Some(2), false).await;
        assert!(
            result.is_err(),
            "empty allowlist must block run_parallel_jobs"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("not authorized"),
            "error must mention authorization: {err}"
        );
        // No channel should have been minted.
        let svc = mur_channel::ChannelService::open(tmp.path()).unwrap();
        let channels = svc.list(100).unwrap_or_default();
        assert!(
            channels.is_empty(),
            "no channel must be minted when the gate rejects"
        );
    }

    #[tokio::test]
    async fn run_parallel_jobs_mints_channel_even_when_delegate_unreachable() {
        // Write an allowlisting config.yaml so the gate passes, then verify the
        // channel is minted even though the delegate is unreachable at runtime.
        // (No runtime is running, so the delegate dial fails fast (RequireRunning).
        // run_parallel_jobs must still mint the channel and return Ok — the
        // executor turns a failed delegate into a failed step, not an Err.)
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("config.yaml"),
            "parallel_jobs:\n  targets:\n    - nonexistent-agent-xyz\n",
        )
        .unwrap();
        let jobs = vec![Job {
            description: "do A".into(),
            assignee: "nonexistent-agent-xyz".into(),
        }];
        let (channel_id, _out) = run_parallel_jobs(tmp.path(), &jobs, Some(2), false)
            .await
            .expect("must not error when the delegate is unreachable");
        assert!(!channel_id.is_empty(), "a channel should have been minted");
        // The minted channel is persisted and loadable.
        let svc = mur_channel::ChannelService::open(tmp.path()).unwrap();
        assert!(svc.load_events(&channel_id).is_ok());
    }
}

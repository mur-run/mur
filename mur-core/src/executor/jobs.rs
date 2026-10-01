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
pub use crate::executor::delegation::cwd::RunCwd;

/// A single job: a prompt and the (canonicalized) agent to delegate it to.
pub struct Job {
    pub description: String,
    pub assignee: String,
}

/// One rank-0 `ProcedureStep` per job (all parallel, no deps). Sets BOTH
/// `intent` (the delegate prompt) and `description` (channel/ledger labels)
/// to the job text, and a stable unique `id` for idempotency / crash-resume.
/// `routing` — the note naming the target directory — is appended to the
/// prompt only, never to the label: a member that is told nothing about where
/// the work is builds in whatever directory it happens to sit in (#1607).
pub fn build_jobs_procedure(jobs: &[Job], routing: Option<&str>) -> Procedure {
    Procedure {
        variables: vec![],
        steps: jobs
            .iter()
            .enumerate()
            .map(|(i, j)| ProcedureStep {
                description: j.description.clone(),
                intent: Some(match routing {
                    Some(note) => format!("{}{note}", j.description),
                    None => j.description.clone(),
                }),
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
                "{}",
                mur_common::authz::not_authorized(&format!(
                    "target '{}' for parallel_jobs (deny-by-default) — add it under \
                     `parallel_jobs.targets` in {}, e.g.\n\nparallel_jobs:\n  targets:\n    - {}",
                    j.assignee,
                    config_path.display(),
                    j.assignee
                ))
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
/// A fan-out that has been started and can be polled.
pub struct Dispatched {
    pub run_id: String,
    pub channel_id: String,
    pub handle: tokio::task::JoinHandle<Result<PipelineOutput>>,
}

/// Start the fan-out and return its handle at once (spec 2026-09-12
/// execution-limits §3.6): the caller polls `mur_job_status`. The run
/// executes on the CURRENT tokio runtime — inside `mur-mcp-server` that is
/// the server's own lifetime, which the tool description says out loud.
/// `cwd` is where the jobs are routed; a bad one fails here, before any
/// channel is minted.
pub fn dispatch_parallel_jobs(
    mur_home: &Path,
    jobs: &[Job],
    max_concurrency: Option<usize>,
    yes: bool,
    cwd: &RunCwd,
) -> Result<Dispatched> {
    authorize_targets(mur_home, jobs)?;
    let routing = cwd.routing_note()?;
    let proc = build_jobs_procedure(jobs, Some(&routing));
    let svc = ChannelService::open(mur_home)?;
    let channel_id = svc.create_for_workflow("parallel-jobs")?.id;
    let run_id = format!("run-{}", uuid::Uuid::now_v7());
    let home = mur_home.to_path_buf();
    let (rid, cid) = (run_id.clone(), channel_id.clone());
    let label = format!("{} parallel job(s)", jobs.len());
    let handle = tokio::spawn(async move {
        let opts = DagExecOptions {
            yes,
            trigger: "agent",
            channel_id: Some(cid.clone()),
            run_id: rid,
            run_kind: Some(crate::run_status::RunKind::Job),
            run_label: label,
            max_concurrency,
            ..Default::default()
        };
        // `job:` keeps the run ledger out of the skill store — this fan-out is
        // ephemeral and owns no skill.yaml. See `skill::event_log::event_log_path`.
        execute_dag(&home, "job:parallel-jobs", &proc, &opts)
            .await
            .map_err(|e| anyhow::anyhow!("parallel_jobs run on channel {cid} failed: {e}"))
    });
    Ok(Dispatched {
        run_id,
        channel_id,
        handle,
    })
}

/// Dispatch and wait — for an in-process caller that wants the output.
pub async fn run_parallel_jobs(
    mur_home: &Path,
    jobs: &[Job],
    max_concurrency: Option<usize>,
    yes: bool,
    cwd: &RunCwd,
) -> Result<(String, PipelineOutput)> {
    let d = dispatch_parallel_jobs(mur_home, jobs, max_concurrency, yes, cwd)?;
    let out = d
        .handle
        .await
        .map_err(|e| anyhow::anyhow!("parallel_jobs task panicked: {e}"))??;
    Ok((d.channel_id, out))
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
        let p = build_jobs_procedure(&jobs, None);
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

    /// #1607: the routing note reaches the member (intent) but never the
    /// channel/ledger label (description).
    #[test]
    fn build_jobs_procedure_routes_prompt_not_label() {
        let jobs = vec![Job {
            description: "fix the build".into(),
            assignee: "coder".into(),
        }];
        let note = "\n\nIMPORTANT: the directory you are working in is `/proj`.";
        let p = build_jobs_procedure(&jobs, Some(note));
        assert_eq!(
            p.steps[0].intent.as_deref(),
            Some("fix the build\n\nIMPORTANT: the directory you are working in is `/proj`.")
        );
        assert_eq!(p.steps[0].description, "fix the build");
    }

    /// A bad `cwd` is refused before any channel exists — nothing to clean up.
    #[test]
    fn dispatch_rejects_relative_cwd_before_minting() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("config.yaml"),
            "parallel_jobs:\n  targets: [ghost]\n",
        )
        .unwrap();
        let jobs = vec![Job {
            description: "a".into(),
            assignee: "ghost".into(),
        }];
        let rt = tokio::runtime::Runtime::new().unwrap();
        let _g = rt.enter();
        let err = match dispatch_parallel_jobs(
            tmp.path(),
            &jobs,
            Some(1),
            false,
            &RunCwd::from_tool_args(Some("rel/dir"), false),
        ) {
            Ok(_) => panic!("relative cwd must be refused"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("absolute"), "{err}");
        let svc = mur_channel::ChannelService::open(tmp.path()).unwrap();
        assert!(
            svc.list(100).unwrap_or_default().is_empty(),
            "no channel may be minted for a refused dispatch"
        );
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
        let result = run_parallel_jobs(tmp.path(), &jobs, Some(2), false, &RunCwd::default()).await;
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
        let (channel_id, _out) =
            run_parallel_jobs(tmp.path(), &jobs, Some(2), false, &RunCwd::default())
                .await
                .expect("must not error when the delegate is unreachable");
        assert!(!channel_id.is_empty(), "a channel should have been minted");
        // The minted channel is persisted and loadable.
        let svc = mur_channel::ChannelService::open(tmp.path()).unwrap();
        assert!(svc.load_events(&channel_id).is_ok());
    }

    /// §7: dispatch returns at once with an id; the run is recorded under it
    /// and finishes on its own. The target is not running, so the run fails
    /// fast — what is under test is the shape, not the delegation.
    #[tokio::test]
    async fn dispatch_returns_before_the_run_finishes_and_records_it() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();
        std::fs::write(
            home.join("config.yaml"),
            "parallel_jobs:\n  targets:\n    - ghost\n",
        )
        .unwrap();
        let jobs = vec![Job {
            description: "do x".into(),
            assignee: "ghost".into(),
        }];
        let d = dispatch_parallel_jobs(home, &jobs, Some(1), false, &RunCwd::default()).unwrap();
        // No wall-clock bound here, deliberately. The previous
        // `elapsed() < 2s` measured what `dispatch_parallel_jobs` does
        // *synchronously* before it spawns — authorize, open the channel
        // store, write a channel row — which is SQLite I/O and took over six
        // seconds on the Windows runner, failing on `main` for changes that
        // never touched this crate.
        //
        // It also could not test what it claimed. "Returns before the run
        // finishes" is guaranteed structurally: the run is on a
        // `tokio::spawn`ed task and reaches the caller as `handle`, so there
        // is no version of this code that returns a `Dispatched` *after*
        // awaiting it. A timer cannot tell that apart from a fast runner.
        assert!(d.run_id.starts_with("run-"));
        // The handle is still ours to await — which is the property, stated
        // as the API rather than as a stopwatch.
        let _ = d.handle.await;
        let rec = crate::run_status::store::load(home, &d.run_id)
            .unwrap()
            .expect("recorded by execute_dag");
        assert!(rec.state.is_terminal(), "{:?}", rec.state);
    }
}

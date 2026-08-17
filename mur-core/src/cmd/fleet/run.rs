//! `mur fleet run` — one iteration: fan the goal out to each member over the
//! shared channel via the existing DAG executor (delegation), then print replies.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use mur_common::channel::{ChannelActor, EventKind};
use mur_common::fleet::{Job, JobStatus};
use mur_common::parallel::{ParallelConfig, ParallelMode};
use mur_common::skill::manifest::{Procedure, ProcedureStep};

use crate::parallel::partition::{
    planner::{plan_partition, validate_coverage},
    prompt::region_prompt,
};
use crate::parallel::semantic::{SupportedLanguage, extract_units};
use crate::parallel::track::{TrackSet, worktree};

use super::store;

/// Map a fleet run's **channel outcome** to a terminal [`JobStatus`] (#10).
///
/// Reads the last `StateChange` event emitted by THIS run (`seq > since`) and
/// maps its `to` state — this is the authoritative source of a job's terminal
/// status, replacing the old behaviour of trusting the yaml's declared
/// `status:`. Returns `None` when the run recorded no terminal StateChange
/// (e.g. an empty DAG, a run with no channel routing, or a crash before the
/// executor's `emit_final`), in which case the caller falls back to the raw
/// `exec_result`.
///
/// Mapping (channel `ChannelState` → `JobStatus`):
/// - `completed` → `Done`
/// - `failed`    → `Failed`
/// - `canceled` / `rejected` → `Canceled`
///
/// Non-terminal states (`working`, `submitted`, `input-required`, `stale`) are
/// ignored: they are never a run's *final* word, so we keep scanning and, if
/// nothing terminal is found, return `None`.
fn job_status_from_channel(
    events: &[mur_common::channel::ChannelEvent],
    since: u64,
) -> Option<JobStatus> {
    events
        .iter()
        .filter(|e| e.seq > since && e.kind == EventKind::StateChange)
        .filter_map(|e| e.payload.get("to").and_then(|v| v.as_str()))
        .rev()
        .find_map(|to| match to {
            "completed" => Some(JobStatus::Done),
            "failed" => Some(JobStatus::Failed),
            "canceled" | "rejected" => Some(JobStatus::Canceled),
            _ => None,
        })
}

/// Phase 1 "plan": one parallel delegate-step per member (or per track if parallel config present),
/// each handed the goal (with approach injection for tracks).
/// (Phase 2 replaces this with a router-produced DAG.)
pub fn build_fleet_procedure(
    goal: &str,
    members: &[String],
    parallel: Option<&ParallelConfig>,
) -> Result<Procedure> {
    // Partition mode: read the target file and build per-region steps.
    if let Some(cfg) = parallel
        && cfg.mode == ParallelMode::Partition
        && let Some(part) = &cfg.partition
    {
        let source = std::fs::read(&part.target_file)
            .with_context(|| format!("read partition target {}", part.target_file))?;
        return build_partition_procedure(goal, &part.target_file, &source, members, &cfg.tracks);
    }

    let steps = if let Some(cfg) = parallel {
        if members.is_empty() {
            bail!("parallel fleet requires at least one member");
        }
        // Parallel tracks: one step per track with approach injection into goal
        cfg.tracks
            .iter()
            .enumerate()
            .map(|(i, track_cfg)| {
                let injected_goal = format!("{goal}\n\nApproach: {}", track_cfg.approach);
                let member = members
                    .get(i % members.len())
                    .cloned()
                    .expect("members guard ensures non-empty");
                ProcedureStep {
                    description: format!("{}: {injected_goal}", track_cfg.name),
                    intent: Some(injected_goal),
                    delegate_to: Some(member),
                    id: Some(track_cfg.name.clone()),
                    ..Default::default()
                }
            })
            .collect()
    } else {
        // Regular fleet: one step per member with same goal
        members
            .iter()
            .map(|m| ProcedureStep {
                description: format!("{m}: {goal}"),
                intent: Some(goal.to_string()),
                delegate_to: Some(m.clone()),
                id: Some(m.clone()),
                ..Default::default()
            })
            .collect()
    };
    Ok(Procedure {
        variables: vec![],
        steps,
    })
}

/// Build a partition-mode procedure: one step per track, each constrained to its region.
pub fn build_partition_procedure(
    goal: &str,
    target_file: &str,
    source: &[u8],
    members: &[String],
    tracks: &[mur_common::parallel::TrackConfig],
) -> Result<Procedure> {
    if members.is_empty() {
        bail!("partition fleet requires at least one member");
    }
    let units = extract_units(source, SupportedLanguage::Rust)?;
    if units.is_empty() {
        bail!("no semantic units found in {target_file}");
    }
    let track_names: Vec<String> = tracks.iter().map(|t| t.name.clone()).collect();
    let plan = plan_partition(&units, &track_names)?;
    validate_coverage(&plan, &units)?;

    let steps = plan
        .assignments
        .iter()
        .enumerate()
        .map(|(i, assignment)| {
            let constraint = region_prompt(target_file, &units, assignment);
            let injected = format!("{goal}\n\n{constraint}");
            let member = members
                .get(i % members.len())
                .cloned()
                .expect("members guard ensures non-empty");
            ProcedureStep {
                description: format!("{}: {target_file} region", assignment.track_name),
                intent: Some(injected),
                delegate_to: Some(member),
                id: Some(assignment.track_name.clone()),
                ..Default::default()
            }
        })
        .collect();
    Ok(Procedure {
        variables: vec![],
        steps,
    })
}

/// Pure goal resolver: explicit arg > oldest queued job > standing goal.
/// Marks the chosen job Running (if a job was selected) before returning.
/// Returns (resolved_goal, active_job_or_none).
pub fn resolve_run_goal(
    mur_home: &Path,
    name: &str,
    job_arg: Option<String>,
    standing_goal: &str,
) -> Result<(String, Option<Job>)> {
    let mut active: Option<Job> = if let Some(text) = job_arg {
        // Explicit arg: enqueue and persist (one-shot, marked Done on finish).
        let job = super::jobs::enqueue_job(mur_home, name, &text, "cli")?;
        Some(job)
    } else {
        super::jobs::next_queued(mur_home, name)?
    };
    let goal = active
        .as_ref()
        .map(|j| j.text.clone())
        .unwrap_or_else(|| standing_goal.to_string());
    if let Some(job) = active.as_mut() {
        job.status = JobStatus::Running;
        job.started_at = Some(chrono::Utc::now().to_rfc3339());
        super::jobs::save_job(mur_home, name, job)?;
    }
    Ok((goal, active))
}

/// Tier 1 worktree execution is gated (experimental, default OFF). When set,
/// a fleet that has a `parallel:` block fans each track into its own git
/// worktree instead of broadcasting to members in the shared checkout.
const EXEC_FLAG_ENV: &str = "MUR_PARALLEL_EXEC";

fn parallel_exec_enabled(force: bool) -> bool {
    force || std::env::var(EXEC_FLAG_ENV).as_deref() == Ok("1")
}

/// Main repo root (where `.worktrees/` lives), discovered from the invoking cwd.
fn discover_repo_root() -> Result<PathBuf> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .context("git rev-parse --show-toplevel")?;
    if !out.status.success() {
        bail!("parallel execution must run inside a git repository");
    }
    Ok(PathBuf::from(
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
    ))
}

/// Concurrency cap for the fan-out — closes the unbounded-spawn gap documented
/// on `DagExecOptions::max_concurrency` (N worktree agents must not cascade past
/// API rate limits). At most `cores-2`, never below 1, never above the step count.
fn fanout_cap(n_steps: usize) -> usize {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    n_steps.min(cores.saturating_sub(2).max(1)).max(1)
}

/// Prompt-route each track's delegate step to work + commit INSIDE its own
/// worktree, via the bash tool's existing per-call `cwd` parameter (Tier 1:
/// no runtime change, best-effort isolation). Steps match tracks by `id`.
fn inject_worktree_routing(procedure: &mut Procedure, tracks: &TrackSet) {
    for step in &mut procedure.steps {
        let Some(id) = step.id.clone() else { continue };
        let Some(track) = tracks.tracks.iter().find(|t| t.config.name == id) else {
            continue;
        };
        let wt = track.worktree_path.display();
        let routing = format!(
            "\n\nIMPORTANT — you are in an ISOLATED worktree; stay inside it:\n\
             - Working directory is `{wt}`. Pass cwd=`{wt}` on EVERY bash/shell call.\n\
             - Edit only files under `{wt}` (absolute paths). Never touch files outside it.\n\
             - When finished, commit so your result can be merged:\n\
             \x20   cd {wt} && git add -A && git commit -m \"{id}: done\""
        );
        match &mut step.intent {
            Some(intent) => intent.push_str(&routing),
            None => step.intent = Some(routing),
        }
    }
}

/// State captured when a gated parallel run creates its worktrees — used after
/// execution for the collision guard and to print reconcile steps.
struct ParallelRun {
    tracks: TrackSet,
    repo_root: PathBuf,
    main_dirty_before: std::collections::HashSet<String>,
}

/// `git status --porcelain` lines for `repo`'s MAIN worktree. Linked worktrees
/// under `.worktrees/` are separate working trees and are NOT reported here, so
/// a diff of this set across the run isolates strays into the main checkout.
fn git_porcelain(repo: &Path) -> std::collections::HashSet<String> {
    std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["status", "--porcelain"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(|l| l.to_string())
                .collect()
        })
        .unwrap_or_default()
}

pub async fn cmd_fleet_run(
    mur_home: &Path,
    name: &str,
    job_arg: Option<String>,
    force_worktree: bool,
) -> Result<()> {
    let fleet = store::load_fleet(mur_home, name)?;
    if fleet.members.is_empty() {
        bail!("fleet '{name}' has no members");
    }

    // Best-effort program-deps preflight — informational only, never blocks
    // the run. A load/aggregate error is swallowed.
    let _ = (|| -> Result<()> {
        let deps = crate::cmd::deps::aggregate_fleet(mur_home, name)?;
        let report = crate::cmd::deps::doctor::build_report(&deps, mur_home);
        if crate::cmd::deps::doctor::missing_count(&report) > 0 {
            eprintln!(
                "warning: fleet '{name}' has missing program dependencies — run `mur fleet doctor {name}` for details or `mur fleet install-deps {name}` to install them."
            );
        }
        Ok(())
    })();

    if super::control::is_stopped(mur_home, name) {
        bail!(
            "fleet '{name}' is stopped (kill-switch). Run `mur fleet start {name}` to re-enable."
        );
    }
    let (goal, mut active_job) = resolve_run_goal(mur_home, name, job_arg, &fleet.goal)?;
    if goal.is_empty() {
        bail!(
            "fleet '{name}' has no goal and no queued job; pass one: mur fleet run {name} \"<job>\""
        );
    }
    let svc = mur_channel::ChannelService::open(mur_home)?;
    let events = svc.load_events(&fleet.channel_id)?;
    // Cursor BEFORE this run so the reply tail prints only THIS run's events.
    let since = events.last().map(|e| e.seq).unwrap_or(0);
    // Plan with the resolved goal (overrides fleet.goal for this run).
    let planning_fleet = mur_common::fleet::Fleet {
        goal: goal.clone(),
        ..fleet.clone()
    };
    // Tier 1 parallel execution (gated, experimental): create one git worktree
    // per track and route each delegate into it. Otherwise: router plan, else
    // broadcast-to-all. The router does not model worktrees, so the parallel
    // path deliberately bypasses it.
    let exec_parallel = parallel_exec_enabled(force_worktree) && fleet.parallel.is_some();
    if force_worktree && !exec_parallel {
        eprintln!(
            "warning: --worktree has no effect -- fleet '{name}' has no parallel: config block, so Tier 1 worktree isolation cannot activate; falling back to normal delegation"
        );
    }
    let (proc, parallel_run) = if exec_parallel {
        let cfg = fleet.parallel.as_ref().expect("guarded by exec_parallel");
        let repo_root = discover_repo_root()?;
        let fleet_dir = mur_home.join("fleets").join(name);
        // Clean slate: tear down any leftover worktrees from a prior run of THIS fleet.
        if let Ok(prev) = TrackSet::load(&fleet_dir) {
            worktree::destroy_tracks(&prev, &repo_root);
        }
        let main_dirty_before = git_porcelain(&repo_root);
        let ts = worktree::create_tracks(cfg, &repo_root).context("create per-track worktrees")?;
        ts.save(&fleet_dir)?;
        let mut p = build_fleet_procedure(&goal, &fleet.members, Some(cfg))?;
        inject_worktree_routing(&mut p, &ts);
        println!(
            "[parallel] {} worktree(s) under {}/.worktrees — routing one agent each",
            ts.tracks.len(),
            repo_root.display()
        );
        (
            p,
            Some(ParallelRun {
                tracks: ts,
                repo_root,
                main_dirty_before,
            }),
        )
    } else {
        // Regular (non-worktree) delegation: the member agent otherwise gets ONLY the
        // bare goal and has no idea which repo/dir to work in. Best-effort: discover the
        // repo root and append a routing note. Not being in a git repo is not fatal here.
        let mut routed_goal = goal.clone();
        if let Ok(repo_root) = discover_repo_root() {
            let repo = repo_root.display();
            routed_goal.push_str(&format!(
                "\n\nIMPORTANT: the repository you are working on is at `{repo}`. cd there (or pass cwd=`{repo}` on every bash/tool call) before doing anything else."
            ));
        }
        let routed_fleet = mur_common::fleet::Fleet {
            goal: routed_goal.clone(),
            ..planning_fleet.clone()
        };
        // Router plans which members do what (with deps); falls back to broadcast-to-all.
        let p = super::plan::plan_via_router(mur_home, &routed_fleet, &routed_goal, &events)
            .unwrap_or_else(|| {
                build_fleet_procedure(&routed_goal, &fleet.members, fleet.parallel.as_ref())
                    .expect("members validated by caller guard")
            });
        (p, None)
    };
    let run_id = format!("run-{}", uuid::Uuid::now_v7());
    let opts = crate::executor::dag::DagExecOptions {
        // Fail-closed default: do NOT blanket-approve. Fleet delegation steps carry
        // no risk tier today (member runtimes gate their own tools), but a future
        // router-emitted DAG with risk steps must fail-closed, never auto-approve
        // unattended. See the best-practice audit (OWASP Agentic ASI06).
        yes: false,
        channel_id: Some(fleet.channel_id.clone()),
        run_id: run_id.clone(),
        run_kind: Some(crate::run_status::RunKind::Fleet),
        run_label: format!("fleet {}", fleet.name),
        // Cap fan-out so N worktree agents don't cascade past API rate limits.
        max_concurrency: exec_parallel.then(|| fanout_cap(proc.steps.len())),
        ..Default::default()
    };
    // skill_name here is a readable run-history label; channel routing still uses opts.channel_id.
    let exec_result =
        crate::executor::dag::execute_dag(mur_home, &format!("fleet:{}", fleet.name), &proc, &opts)
            .await;
    // Stamp job terminal before surfacing any error.
    //
    // #10 truth fix: the job's terminal status MUST reflect the RUN's actual
    // outcome, not the yaml's declared `status:`. The authoritative source is
    // the channel's last `StateChange` event emitted by THIS run (seq > since):
    // the DAG executor transitions the channel to Completed/Failed at the end.
    // A DAG can return `Ok` while the channel ended `failed` (a member step
    // failed but was tolerated) — that is exactly the decoupling that lied to
    // the user ("done" while the channel said working→failed). We read the
    // channel state-change back and let it decide; `exec_result` only supplies
    // the error text and is the fallback when no StateChange was recorded.
    if let Some(job) = active_job.as_mut() {
        job.run_id = Some(run_id);
        job.finished_at = Some(chrono::Utc::now().to_rfc3339());

        // Authoritative: last StateChange `to` from this run's events.
        let channel_terminal = svc
            .load_events(&fleet.channel_id)
            .ok()
            .and_then(|evs| job_status_from_channel(&evs, since));

        match (&channel_terminal, &exec_result) {
            // Channel spoke: it is the source of truth for the terminal state.
            (Some(JobStatus::Done), _) => {
                job.status = JobStatus::Done;
                job.result = exec_result
                    .as_ref()
                    .ok()
                    .and_then(|o| o.output_text.clone())
                    .filter(|t| !t.is_empty());
            }
            (Some(JobStatus::Failed), _) => {
                job.status = JobStatus::Failed;
                // Prefer the concrete exec error; else say the channel failed.
                job.error = Some(match &exec_result {
                    Err(e) => format!("{e:#}"),
                    Ok(_) => "run ended in channel state `failed`".to_string(),
                });
            }
            (Some(JobStatus::Canceled), _) => {
                job.status = JobStatus::Canceled;
                job.error = Some("run was canceled/rejected".to_string());
            }
            // Channel said nothing terminal (empty DAG, no channel routing, or a
            // hard crash before emit): fall back to the exec_result.
            (_, Ok(out)) => {
                job.status = JobStatus::Done;
                job.result = out.output_text.clone().filter(|t| !t.is_empty());
            }
            (_, Err(e)) => {
                job.status = JobStatus::Failed;
                job.error = Some(format!("{e:#}"));
            }
        }
        super::jobs::save_job(mur_home, name, job)?;
    }
    let out = exec_result?;
    if let Some(t) = out.output_text.filter(|t| !t.is_empty()) {
        println!("{t}");
    }

    // Tail agent-authored replies from THIS run (seq > pre-run cursor), peer-writes-own.
    // Note: prints payload["text"]; confirm the exact reply payload shape in the live Harness test.
    for ev in svc
        .load_events(&fleet.channel_id)?
        .into_iter()
        .filter(|e| e.seq > since)
    {
        if let (ChannelActor::Agent { id }, Some(text)) = (
            &ev.actor,
            ev.payload
                .get("text")
                .and_then(|v| v.as_str())
                .filter(|t| !t.is_empty()),
        ) {
            println!("[{id}] {text}");
        }
    }

    // Tier 1: leave worktrees + tracks.json intact for the existing reconcilers
    // (auto-destroy would discard the agents' committed work). Print next steps.
    if let Some(run) = &parallel_run {
        // Collision guard (best-effort): new porcelain entries in the MAIN checkout
        // mean an agent ignored its cwd routing. This stray rate is exactly the
        // signal that would justify Tier 2 (runtime-enforced cwd).
        let after = git_porcelain(&run.repo_root);
        let strayed: Vec<&String> = after.difference(&run.main_dirty_before).collect();
        if !strayed.is_empty() {
            println!(
                "\n[parallel] ⚠ {} path(s) changed in the MAIN checkout — an agent left its worktree:",
                strayed.len()
            );
            for s in strayed.iter().take(10) {
                println!("    {s}");
            }
            println!("  Tier 1 routing is best-effort; strays justify Tier 2 enforced cwd.");
        }

        let is_partition = fleet
            .parallel
            .as_ref()
            .map(|c| c.mode == ParallelMode::Partition)
            .unwrap_or(false);
        println!(
            "\n[parallel] {} track(s) done in their worktrees. Reconcile with:",
            run.tracks.tracks.len()
        );
        if is_partition {
            println!("  mur fleet merge {name}");
        } else {
            println!("  mur fleet compare {name}");
            println!("  MUR_PARALLEL_CONCURRENT=1 mur fleet merge-concurrent {name} --stats");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_flag_gates_parallel_execution() {
        unsafe { std::env::remove_var(EXEC_FLAG_ENV) };
        assert!(!parallel_exec_enabled(false));
        unsafe { std::env::set_var(EXEC_FLAG_ENV, "1") };
        assert!(parallel_exec_enabled(false));
        unsafe { std::env::remove_var(EXEC_FLAG_ENV) };
    }

    #[test]
    fn force_worktree_bypasses_env_var() {
        unsafe { std::env::remove_var(EXEC_FLAG_ENV) };
        assert!(
            parallel_exec_enabled(true),
            "an explicit force=true must enable isolation even with the env var unset"
        );
        assert!(!parallel_exec_enabled(false));
    }

    #[test]
    fn fanout_cap_is_bounded_and_nonzero() {
        assert!(fanout_cap(0) >= 1, "never zero");
        assert_eq!(fanout_cap(1), 1, "never exceeds the step count");
        assert!(fanout_cap(64) >= 1);
    }

    #[test]
    fn worktree_routing_injected_per_track_by_id() {
        use crate::parallel::track::{Track, TrackSet};
        use mur_common::parallel::TrackConfig;
        let mk = |id: &str, intent: &str| ProcedureStep {
            id: Some(id.into()),
            intent: Some(intent.into()),
            ..Default::default()
        };
        let mut proc = Procedure {
            variables: vec![],
            steps: vec![
                mk("track-a", "do A"),
                mk("track-b", "do B"),
                mk("other", "do C"),
            ],
        };
        let track = |name: &str, path: &str| Track {
            config: TrackConfig {
                name: name.into(),
                approach: String::new(),
                model: None,
            },
            worktree_path: path.into(),
        };
        let ts = TrackSet {
            tracks: vec![track("track-a", "/wt/a"), track("track-b", "/wt/b")],
        };

        inject_worktree_routing(&mut proc, &ts);

        let a = proc.steps[0].intent.as_ref().unwrap();
        assert!(a.starts_with("do A"), "original intent preserved");
        assert!(
            a.contains("/wt/a") && a.contains("cwd"),
            "routed to its worktree via cwd"
        );
        assert!(proc.steps[1].intent.as_ref().unwrap().contains("/wt/b"));
        // A step with no matching track is left untouched.
        assert_eq!(proc.steps[2].intent.as_deref(), Some("do C"));
    }

    #[test]
    fn build_fleet_procedure_one_delegate_step_per_member() {
        let p =
            build_fleet_procedure("ship it", &["pm".to_string(), "qa".to_string()], None).unwrap();
        assert_eq!(p.steps.len(), 2);
        assert_eq!(p.steps[0].delegate_to.as_deref(), Some("pm"));
        assert_eq!(p.steps[1].delegate_to.as_deref(), Some("qa"));
        assert_eq!(p.steps[0].intent.as_deref(), Some("ship it"));
        assert!(p.steps[0].depends_on.is_empty()); // parallel rank 0
    }

    #[test]
    fn parallel_goal_injects_approach() {
        let cfg = ParallelConfig {
            mode: mur_common::parallel::ParallelMode::Speculative,
            tracks: vec![
                mur_common::parallel::TrackConfig {
                    name: "track1".to_string(),
                    approach: "functional style".to_string(),
                    model: None,
                },
                mur_common::parallel::TrackConfig {
                    name: "track2".to_string(),
                    approach: "imperative style".to_string(),
                    model: None,
                },
            ],
            judge: mur_common::parallel::JudgeConfig {
                model: "test-model".to_string(),
                rubric: mur_common::parallel::Rubric::default(),
            },
            pre_filter: vec![],
            partition: None,
        };
        let p = build_fleet_procedure("ship it", &["pm".to_string(), "qa".to_string()], Some(&cfg))
            .unwrap();
        assert_eq!(p.steps.len(), 2);
        // First track should have functional style approach injected
        assert_eq!(p.steps[0].id.as_deref(), Some("track1"));
        let intent0 = p.steps[0].intent.as_deref().unwrap();
        assert!(intent0.contains("ship it"));
        assert!(intent0.contains("functional style"));
        assert!(intent0.contains("Approach:"));
        // Second track should have imperative style approach injected
        assert_eq!(p.steps[1].id.as_deref(), Some("track2"));
        let intent1 = p.steps[1].intent.as_deref().unwrap();
        assert!(intent1.contains("ship it"));
        assert!(intent1.contains("imperative style"));
        assert!(intent1.contains("Approach:"));
    }

    #[test]
    fn resolve_goal_prefers_arg_then_queued_then_standing() {
        // pure resolution helper — no execution
        use mur_common::fleet::JobStatus;
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        super::super::create::cmd_fleet_create(
            home,
            "dev",
            vec!["pm".into()],
            None,
            Some("standing".into()),
            None,
        )
        .unwrap();

        // no arg, empty queue → standing goal
        let (goal, job) = resolve_run_goal(home, "dev", None, "standing").unwrap();
        assert_eq!(goal, "standing");
        assert!(job.is_none());

        // queued job → job's text, marked running
        super::super::jobs::enqueue_job(home, "dev", "queued-work", "cli").unwrap();
        let (goal, job) = resolve_run_goal(home, "dev", None, "standing").unwrap();
        assert_eq!(goal, "queued-work");
        assert_eq!(job.as_ref().unwrap().status, JobStatus::Running);

        // explicit arg jumps ahead of queue and is persisted
        let (goal, job) = resolve_run_goal(home, "dev", Some("urgent".into()), "standing").unwrap();
        assert_eq!(goal, "urgent");
        assert_eq!(job.as_ref().unwrap().status, JobStatus::Running);
        // arg job must be persisted (source=="cli")
        let all_jobs = super::super::jobs::list_jobs(home, "dev").unwrap();
        assert!(
            all_jobs.iter().any(|j| j.text == "urgent"),
            "arg job should be persisted"
        );
    }

    #[test]
    fn partition_procedure_constrains_each_member_to_its_region() {
        use mur_common::parallel::TrackConfig;
        let source = b"fn alpha() -> i32 { 0 }\nfn beta() -> i32 { 0 }\n";
        let tracks = vec![
            TrackConfig {
                name: "track-0".into(),
                approach: String::new(),
                model: None,
            },
            TrackConfig {
                name: "track-1".into(),
                approach: String::new(),
                model: None,
            },
        ];
        let members = vec!["pm".to_string(), "qa".to_string()];
        let p =
            build_partition_procedure("build widget", "src/widget.rs", source, &members, &tracks)
                .unwrap();
        assert_eq!(p.steps.len(), 2);
        // Each step delegates to a member and mentions the file
        let intents: Vec<&str> = p.steps.iter().filter_map(|s| s.intent.as_deref()).collect();
        assert_eq!(intents.len(), 2);
        assert!(intents.iter().all(|i| i.contains("src/widget.rs")));
        // Steps reference different units
        assert_ne!(intents[0], intents[1]);
    }
}

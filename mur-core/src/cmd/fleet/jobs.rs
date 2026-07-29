//! Fleet job queue — small YAML records under `~/.mur/fleets/<name>/jobs/`.
//! A job is a unit of work handed to a fleet by command; it becomes the goal
//! for one run. FIFO ordering is the UUIDv7 filename sort (no index file).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use mur_common::channel::{ChannelEvent, EventKind};
use mur_common::fleet::{Job, JobStatus, valid_fleet_name};

use super::store;

/// A `running` job with no live run is a zombie (#10: `019f69d8` sat `running`
/// for four days). If neither the channel nor anything else ever terminates it,
/// reconcile it to `failed` this long after `started_at` so it stops lying.
const RUNNING_GRACE_SECS: i64 = 6 * 60 * 60; // 6h

pub(crate) fn jobs_dir(mur_home: &Path, fleet: &str) -> PathBuf {
    store::fleet_dir(mur_home, fleet).join("jobs")
}

fn job_path(mur_home: &Path, fleet: &str, id: &str) -> PathBuf {
    jobs_dir(mur_home, fleet).join(format!("{id}.yaml"))
}

/// Write a new queued job and return it.
pub fn enqueue_job(mur_home: &Path, fleet: &str, text: &str, source: &str) -> Result<Job> {
    let job = Job {
        id: uuid::Uuid::now_v7().to_string(),
        text: text.to_string(),
        source: source.to_string(),
        status: JobStatus::Queued,
        created_at: chrono::Utc::now().to_rfc3339(),
        started_at: None,
        finished_at: None,
        run_id: None,
        result: None,
        error: None,
    };
    save_job(mur_home, fleet, &job)?;
    Ok(job)
}

/// Atomic write (temp + rename), matching `store::save_fleet`.
pub fn save_job(mur_home: &Path, fleet: &str, job: &Job) -> Result<()> {
    if !valid_fleet_name(fleet) {
        bail!("invalid fleet name '{fleet}': use lowercase letters, digits, '-' or '_'");
    }
    let dir = jobs_dir(mur_home, fleet);
    std::fs::create_dir_all(&dir).with_context(|| format!("create jobs dir {}", dir.display()))?;
    let path = job_path(mur_home, fleet, &job.id);
    let yaml = serde_yaml::to_string(job).context("serialize job")?;
    let tmp = path.with_extension("yaml.tmp");
    std::fs::write(&tmp, yaml).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("rename to {}", path.display()))?;
    Ok(())
}

/// The channel's terminal outcome for a run, read from its event log (#10):
/// the last `StateChange` event whose `to` is terminal, mapped to a job status.
/// `None` if the channel has no terminal StateChange yet (still working).
pub(crate) fn channel_terminal_status(events: &[ChannelEvent]) -> Option<JobStatus> {
    events
        .iter()
        .filter(|e| e.kind == EventKind::StateChange)
        .filter_map(|e| e.payload.get("to").and_then(|v| v.as_str()))
        .rev()
        .find_map(|to| match to {
            "completed" => Some(JobStatus::Done),
            "failed" => Some(JobStatus::Failed),
            "canceled" | "rejected" => Some(JobStatus::Canceled),
            _ => None,
        })
}

/// Decide the reconciled terminal status of a **running** job (#10), pure so it
/// is testable without touching the filesystem. Returns `Some((status, reason))`
/// when the job should be moved to a terminal state, `None` to leave it running.
///
/// Priority:
///  1. **Channel truth** — if the job's run drove a channel to a terminal
///     StateChange, adopt it (this is what `mur fleet run` also does, so a job
///     whose run crashed *after* the channel finished but *before* it stamped
///     the yaml still ends up correct).
///  2. **Orphan staleness** — a job still `running` `RUNNING_GRACE_SECS` after
///     `started_at`, with no terminal channel signal, has no live run: its
///     process died (the four-day `019f69d8`). Mark `failed(orphaned)`.
pub(crate) fn reconcile_running(
    job: &Job,
    channel: Option<JobStatus>,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<(JobStatus, String)> {
    if job.status != JobStatus::Running {
        return None;
    }
    if let Some(term) = channel {
        // Only adopt channel truth for a job that actually drove a run.
        if job.run_id.is_some() {
            let reason = match term {
                JobStatus::Failed => "run ended in channel state `failed`",
                JobStatus::Canceled => "run was canceled/rejected in channel",
                _ => "channel run completed",
            };
            return Some((term, reason.to_string()));
        }
    }
    // No terminal channel signal → orphan check against the grace window.
    let started = job
        .started_at
        .as_deref()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc));
    if let Some(started) = started
        && (now - started).num_seconds() >= RUNNING_GRACE_SECS
    {
        return Some((
            JobStatus::Failed,
            format!(
                "orphaned: still `running` with no live run {}h after start (state machine reconcile)",
                RUNNING_GRACE_SECS / 3600
            ),
        ));
    }
    None
}

/// Sweep a fleet's jobs and converge any zombie `running` records to their true
/// terminal state (#10). Called lazily by [`list_jobs`], so `mur fleet show`
/// and `mur fleet jobs` always report truth, and orphaned runs (crash/kill)
/// self-heal on the next read. Best-effort: channel-read/save failures leave the
/// job untouched rather than aborting the listing.
fn reconcile_jobs(mur_home: &Path, fleet: &str, jobs: &mut [Job]) {
    let running = jobs.iter().any(|j| j.status == JobStatus::Running);
    if !running {
        return;
    }
    // Resolve the fleet's channel once; skip channel-truth if unavailable.
    let channel_status = store::load_fleet(mur_home, fleet).ok().and_then(|f| {
        mur_channel::ChannelService::open(mur_home)
            .and_then(|svc| svc.load_events(&f.channel_id))
            .ok()
            .and_then(|evs| channel_terminal_status(&evs))
    });
    let now = chrono::Utc::now();
    for job in jobs.iter_mut() {
        if let Some((status, reason)) = reconcile_running(job, channel_status, now) {
            job.status = status;
            if status == JobStatus::Failed || status == JobStatus::Canceled {
                job.error.get_or_insert(reason);
            }
            if job.finished_at.is_none() {
                job.finished_at = Some(now.to_rfc3339());
            }
            let _ = save_job(mur_home, fleet, job);
        }
    }
}

/// Jobs straight from the store, oldest-first, with no channel reconciliation.
/// `list_jobs` adds reconciliation on top; callers that already hold the
/// channel's events (the murmur fleet rail) use this to avoid re-parsing it.
pub(crate) fn list_jobs_raw(mur_home: &Path, fleet: &str) -> Result<Vec<Job>> {
    if !valid_fleet_name(fleet) {
        bail!("invalid fleet name '{fleet}': use lowercase letters, digits, '-' or '_'");
    }
    let dir = jobs_dir(mur_home, fleet);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut jobs = Vec::new();
    for entry in
        std::fs::read_dir(&dir).with_context(|| format!("read jobs dir {}", dir.display()))?
    {
        let entry = entry.context("read dir entry")?;
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "yaml") {
            let yaml = std::fs::read_to_string(&path)
                .with_context(|| format!("read {}", path.display()))?;
            let job: Job = serde_yaml::from_str(&yaml).context("deserialize job")?;
            jobs.push(job);
        }
    }
    // Sort by id (UUIDv7 = time order)
    jobs.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(jobs)
}

/// All jobs sorted oldest-first (UUIDv7 lexical sort == time order).
///
/// Reconciles zombie `running` jobs against the channel/orphan rules (#10)
/// before returning, so every caller (`fleet show`, `fleet jobs`) sees truth.
pub fn list_jobs(mur_home: &Path, fleet: &str) -> Result<Vec<Job>> {
    let mut jobs = list_jobs_raw(mur_home, fleet)?;
    // #10: converge zombie `running` records to their true terminal state.
    reconcile_jobs(mur_home, fleet, &mut jobs);
    Ok(jobs)
}

/// Next queued job (oldest first), or None.
pub fn next_queued(mur_home: &Path, fleet: &str) -> Result<Option<Job>> {
    let jobs = list_jobs(mur_home, fleet)?;
    Ok(jobs.into_iter().find(|j| j.status == JobStatus::Queued))
}

/// `mur fleet send <name> "<job>"` — enqueue a job (asynchronous).
pub fn cmd_fleet_send(mur_home: &Path, fleet: &str, text: &str) -> Result<()> {
    let _ = store::load_fleet(mur_home, fleet)?; // validates name + existence
    let job = enqueue_job(mur_home, fleet, text, "cli")?;
    println!(
        "Queued job {} for fleet '{fleet}'. Drain it with `mur fleet run {fleet}` (or the daemon).",
        job.id
    );
    Ok(())
}

/// Which jobs `mur fleet jobs` shows: all when `all`, else only non-terminal
/// (queued/running). Pure so the filter is testable without capturing stdout.
fn visible_jobs(jobs: &[Job], all: bool) -> Vec<&Job> {
    jobs.iter()
        .filter(|j| all || !j.status.is_terminal())
        .collect()
}

/// `mur fleet jobs <name> [--all]` — list jobs and their status.
pub fn cmd_fleet_jobs(mur_home: &Path, fleet: &str, all: bool) -> Result<()> {
    let _ = store::load_fleet(mur_home, fleet)?;
    let jobs = list_jobs(mur_home, fleet)?;
    let shown = visible_jobs(&jobs, all);
    if shown.is_empty() {
        println!("No jobs. Queue one: mur fleet send {fleet} \"<job>\"");
        return Ok(());
    }
    for j in shown {
        let short = &j.id[..j.id.len().min(8)];
        let status = j.status.as_str();
        let note = j.error.as_deref().or(j.result.as_deref()).unwrap_or("");
        let text = if j.text.chars().count() > 50 {
            format!("{}…", j.text.chars().take(49).collect::<String>())
        } else {
            j.text.clone()
        };
        println!("{short}  {status:<9}  {text}  {note}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::fleet::JobStatus;

    #[test]
    fn visible_jobs_hides_terminal_unless_all() {
        let mk = |status| Job {
            id: "x".into(),
            text: "t".into(),
            source: "cli".into(),
            status,
            created_at: "now".into(),
            started_at: None,
            finished_at: None,
            run_id: None,
            result: None,
            error: None,
        };
        let jobs = vec![
            mk(JobStatus::Queued),
            mk(JobStatus::Running),
            mk(JobStatus::Done),
            mk(JobStatus::Failed),
        ];
        // default (all=false): only non-terminal jobs are shown
        let shown = visible_jobs(&jobs, false);
        assert_eq!(shown.len(), 2);
        assert!(shown.iter().all(|j| !j.status.is_terminal()));
        // --all: every job, including terminal
        assert_eq!(visible_jobs(&jobs, true).len(), 4);
    }

    #[test]
    fn enqueue_list_next_fifo_and_update() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let a = enqueue_job(home, "dev", "first", "cli").unwrap();
        let b = enqueue_job(home, "dev", "second", "cli").unwrap();
        assert_ne!(a.id, b.id);

        let all = list_jobs(home, "dev").unwrap();
        assert_eq!(all.len(), 2);
        // uuid v7 => creation order
        assert_eq!(all[0].text, "first");
        assert_eq!(all[1].text, "second");

        // oldest queued is `a`
        assert_eq!(next_queued(home, "dev").unwrap().unwrap().id, a.id);

        // mark `a` running => next_queued skips it
        let mut a2 = a.clone();
        a2.status = JobStatus::Running;
        save_job(home, "dev", &a2).unwrap();
        assert_eq!(next_queued(home, "dev").unwrap().unwrap().id, b.id);
    }

    #[test]
    fn invalid_fleet_name_refused() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(enqueue_job(tmp.path(), "../evil", "x", "cli").is_err());
    }

    #[test]
    fn list_empty_when_no_jobs_dir() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(list_jobs(tmp.path(), "dev").unwrap().is_empty());
    }

    #[test]
    fn send_requires_existing_fleet_then_enqueues() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        // no fleet yet → error
        assert!(cmd_fleet_send(home, "dev", "do it").is_err());
        super::super::create::cmd_fleet_create(home, "dev", vec!["pm".into()], None, None, None)
            .unwrap();
        cmd_fleet_send(home, "dev", "do it").unwrap();
        let jobs = list_jobs(home, "dev").unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].text, "do it");
        assert_eq!(jobs[0].source, "cli");
        // jobs view runs without panicking on existing fleet
        cmd_fleet_jobs(home, "dev", true).unwrap();
    }
}

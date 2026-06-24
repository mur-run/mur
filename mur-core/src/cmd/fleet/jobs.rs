//! Fleet job queue — small YAML records under `~/.mur/fleets/<name>/jobs/`.
//! A job is a unit of work handed to a fleet by command; it becomes the goal
//! for one run. FIFO ordering is the UUIDv7 filename sort (no index file).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use mur_common::fleet::{Job, JobStatus, valid_fleet_name};

use super::store;

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

/// All jobs sorted oldest-first (UUIDv7 lexical sort == time order).
pub fn list_jobs(mur_home: &Path, fleet: &str) -> Result<Vec<Job>> {
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

/// `mur fleet jobs <name> [--all]` — list jobs and their status.
pub fn cmd_fleet_jobs(mur_home: &Path, fleet: &str, all: bool) -> Result<()> {
    let _ = store::load_fleet(mur_home, fleet)?;
    let jobs = list_jobs(mur_home, fleet)?;
    let shown: Vec<&Job> = jobs
        .iter()
        .filter(|j| all || !j.status.is_terminal())
        .collect();
    if shown.is_empty() {
        println!("No jobs. Queue one: mur fleet send {fleet} \"<job>\"");
        return Ok(());
    }
    for j in shown {
        let short = &j.id[..j.id.len().min(8)];
        let status = format!("{:?}", j.status).to_lowercase();
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
        super::super::create::cmd_fleet_create(home, "dev", vec!["pm".into()], None, None).unwrap();
        cmd_fleet_send(home, "dev", "do it").unwrap();
        let jobs = list_jobs(home, "dev").unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].text, "do it");
        assert_eq!(jobs[0].source, "cli");
        // jobs view runs without panicking on existing fleet
        cmd_fleet_jobs(home, "dev", true).unwrap();
    }
}

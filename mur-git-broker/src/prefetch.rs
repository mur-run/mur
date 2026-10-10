//! Budgeted prefetch (F12 item 4, F29): fetch only what the push's ancestry proof needs, under
//! wall-clock, byte and disk budgets. A breach is `PrefetchRejected`; the caller destroys the
//! repo, writes no pending row and audits.
use crate::{
    action::ActionDocument,
    constants::{PREFETCH_BASE_REF_PREFIX, PREFETCH_OLD_REF, PREFETCH_WATCH_INTERVAL_MS},
    error::BrokerError,
    git::{GitError, GitRunner, run_watched},
    policy::{BrokerLimits, RemotePolicy},
    repo::PrivateRepo,
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrefetchBudget {
    pub wall: Duration,
    pub bytes: u64,
    pub disk_bytes: u64,
}
impl From<&BrokerLimits> for PrefetchBudget {
    fn from(l: &BrokerLimits) -> Self {
        Self {
            wall: Duration::from_secs(l.max_prefetch_wall_secs),
            bytes: l.max_prefetch_bytes,
            disk_bytes: l.max_prefetch_disk_bytes,
        }
    }
}

/// Reads refs from the remote into the private repo. A trait so tests (and the push executor's
/// own fetch-free paths) never need a network.
pub trait RemoteReader: Send + Sync {
    fn fetch(
        &self,
        repo: &PrivateRepo,
        specs: &[String],
        budget: &PrefetchBudget,
    ) -> Result<(), BrokerError>;
}

/// Caps how many prefetches run at once; a request over the cap is rejected, not queued.
#[derive(Clone)]
pub struct PrefetchGate {
    max: usize,
    active: Arc<Mutex<usize>>,
}
pub struct PrefetchPermit {
    active: Arc<Mutex<usize>>,
}
impl PrefetchGate {
    pub fn new(max_concurrent: usize) -> Self {
        Self {
            max: max_concurrent,
            active: Arc::new(Mutex::new(0)),
        }
    }
    pub fn acquire(&self) -> Result<PrefetchPermit, BrokerError> {
        let mut n = self.active.lock().unwrap_or_else(|p| p.into_inner());
        if *n >= self.max {
            return Err(BrokerError::PrefetchRejected(
                "concurrent prefetch limit".into(),
            ));
        }
        *n += 1;
        Ok(PrefetchPermit {
            active: Arc::clone(&self.active),
        })
    }
}
impl Drop for PrefetchPermit {
    fn drop(&mut self) {
        let mut n = self.active.lock().unwrap_or_else(|p| p.into_inner());
        *n = n.saturating_sub(1);
    }
}

/// The exact refspecs to fetch: never a glob, `--all` or `--mirror`.
/// Update: the target ref only. Creation: only the policy's creation base refs.
pub fn prefetch_specs(action: &ActionDocument, policy: &RemotePolicy) -> Vec<String> {
    if !action.is_creation() {
        return vec![format!("+{}:{PREFETCH_OLD_REF}", action.updates[0].r#ref)];
    }
    policy
        .creation_base_refs
        .iter()
        .map(|r| {
            let short = r
                .strip_prefix("refs/heads/")
                .or_else(|| r.strip_prefix("refs/"))
                .unwrap_or(r);
            format!("+{r}:{PREFETCH_BASE_REF_PREFIX}{short}")
        })
        .collect()
}

pub struct GitFetchReader {
    pub remote_url: String,
    pub git_bin: PathBuf,
}
impl RemoteReader for GitFetchReader {
    fn fetch(
        &self,
        repo: &PrivateRepo,
        specs: &[String],
        budget: &PrefetchBudget,
    ) -> Result<(), BrokerError> {
        let runner = GitRunner::new(self.git_bin.clone(), repo.path().to_path_buf());
        // No `--filter`: partial clone is rejected by the repo inspection. `--` keeps a hostile
        // URL from being read as an option.
        let mut args = vec!["fetch", "--no-tags", "--no-write-fetch-head", "--"];
        args.push(&self.remote_url);
        args.extend(specs.iter().map(String::as_str));
        let objects = repo.path().join("objects");
        let mut over_budget = || dir_bytes(&objects) > budget.bytes;
        let out = run_watched(
            runner.command(&args),
            budget.wall,
            Duration::from_millis(PREFETCH_WATCH_INTERVAL_MS),
            &mut over_budget,
        )
        .map_err(|e| match e {
            GitError::Tripped => BrokerError::PrefetchRejected("byte budget".into()),
            GitError::Timeout => BrokerError::PrefetchRejected("wall-clock budget".into()),
            GitError::Signal => BrokerError::PrefetchRejected("fetch killed".into()),
            GitError::Spawn(m) => BrokerError::Storage(m),
        })?;
        if out.code != 0 {
            // stderr can carry remote-controlled text; keep it out of the error.
            return Err(BrokerError::PrefetchRejected(format!(
                "fetch exited {}",
                out.code
            )));
        }
        // A fast fetch can finish between two watcher ticks, so enforce the budget once more.
        if dir_bytes(&objects) > budget.bytes {
            return Err(BrokerError::PrefetchRejected("byte budget".into()));
        }
        Ok(())
    }
}

/// Run one prefetch: nothing to fetch means the reader is never called; otherwise take a permit,
/// fetch under budget, then check the repo's total size against the disk budget.
pub fn run_prefetch(
    gate: &PrefetchGate,
    reader: &dyn RemoteReader,
    repo: &PrivateRepo,
    action: &ActionDocument,
    policy: &RemotePolicy,
    limits: &BrokerLimits,
) -> Result<(), BrokerError> {
    let specs = prefetch_specs(action, policy);
    if specs.is_empty() {
        return Ok(());
    }
    let _permit = gate.acquire()?;
    let budget = PrefetchBudget::from(limits);
    reader.fetch(repo, &specs, &budget)?;
    if dir_bytes(repo.path()) > budget.disk_bytes {
        return Err(BrokerError::PrefetchRejected("disk budget".into()));
    }
    Ok(())
}

/// Total size of the regular files under `p`. Symlinks are not followed; entries that vanish
/// mid-walk (git renames temp packs) count as zero.
fn dir_bytes(p: &Path) -> u64 {
    let Ok(meta) = fs::symlink_metadata(p) else {
        return 0;
    };
    if meta.is_dir() {
        fs::read_dir(p)
            .map(|d| d.flatten().map(|e| dir_bytes(&e.path())).sum())
            .unwrap_or(0)
    } else if meta.is_file() {
        meta.len()
    } else {
        0
    }
}

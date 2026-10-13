//! Inbox intake for the git push broker.
//!
//! For each agent home, every request file in `inbox/git-push/` is verified
//! against that agent's on-disk pubkey (the agent id comes from the verified
//! path, never the payload), then answered with a status. A request that passes
//! every check and whose (agent, repo, remote) is enrolled goes to
//! `Broker::submit_request` with the epoch and repo identity the *current*
//! enrollment records carry; anything else is refused with a code and leaves no
//! row. The records are read per request, never cached, so a revoke applies to
//! the next request without a restart. Unreadable records refuse the request
//! (`enrollment_unavailable`); they never read as "nothing enrolled" for work
//! that is already pending.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use mur_common::config::Config;
use mur_common::git_push::{enrollments_path, inbox_dir, private_dir, status_dir};
use mur_common::identity::AgentIdentity;
use mur_git_broker::approval::SystemClock;
use mur_git_broker::binding::{EnrollmentSource, FileEnrollments};
use mur_git_broker::broker::{Broker, BrokerConfig};
use mur_git_broker::error::BrokerError;
use mur_git_broker::import::RlimitSpawn;
use mur_git_broker::policy::BrokerLimits;
use mur_git_broker::prefetch::GitFetchReader;
use mur_git_broker::push::NoAuth;
use mur_git_broker::request_file::{
    REQUEST_EXT, RequestFile, pack_path, validate_id, verify_request,
};

/// The agent is not in `git_push.agents`.
pub const CODE_NOT_ALLOWLISTED: &str = "agent_not_allowlisted";
/// Verified and allowlisted, but no remote is enrolled for it yet.
pub const CODE_NOT_ENROLLED: &str = "not_enrolled";
const STATE_REJECTED: &str = "rejected";
const STATUS_EXT: &str = "yaml";
const TMP_EXT: &str = "tmp";
const AGENTS_DIR: &str = "agents";
const STATE_PENDING_APPROVAL: &str = "pending_approval";
/// Broker-private state, under [`private_dir`].
pub(super) const PENDING_DB: &str = "pending.sqlite";
const WORK_DIR: &str = "work";
const PRIVATE_DIR_MODE: u32 = 0o700;
const GIT_BIN_NAME: &str = "git";
/// The pack's size is checked before it is hashed, so an oversized file is never read whole.
const CODE_PACK_REJECTED: &str = "invalid_request";
/// How often the inboxes are swept. Intake only reads small files.
pub const SWEEP_INTERVAL_SECS: u64 = 5;

/// The broker, opened on first use and then kept: opening settles half-done rows
/// (`PendingStore::recover`), which must happen once per process, not once per sweep.
#[derive(Default)]
pub struct BrokerSlot(Mutex<Option<Arc<Broker>>>);

impl BrokerSlot {
    pub fn get(&self, mur_home: &Path) -> Result<Arc<Broker>, BrokerError> {
        let mut g = self.0.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(b) = &*g {
            return Ok(b.clone());
        }
        let b = Arc::new(open_broker(mur_home)?);
        *g = Some(b.clone());
        Ok(b)
    }
}

fn storage(e: impl std::fmt::Display) -> BrokerError {
    BrokerError::Storage(e.to_string())
}

fn find_git() -> Result<PathBuf, BrokerError> {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .map(|d| d.join(GIT_BIN_NAME))
        .find(|c| c.is_file())
        .ok_or_else(|| storage("git not found"))
}

/// A broker whose every decision re-reads `enrollments.yaml`. Remotes are `file://` only here,
/// so no credentials are applied.
pub fn open_broker(mur_home: &Path) -> Result<Broker, BrokerError> {
    use std::os::unix::fs::DirBuilderExt;
    let private = private_dir(mur_home);
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(PRIVATE_DIR_MODE)
        .create(&private)
        .map_err(storage)?;
    let git_bin = find_git()?;
    let reader_git = git_bin.clone();
    Broker::new(BrokerConfig {
        store_path: private.join(PENDING_DB),
        work_root: private.join(WORK_DIR),
        limits: BrokerLimits::default(),
        git_bin,
        enrollments: Box::new(FileEnrollments::new(enrollments_path(mur_home))),
        reader: Box::new(move |url: &str| {
            Box::new(GitFetchReader {
                remote_url: url.to_string(),
                git_bin: reader_git.clone(),
            })
        }),
        spawn: Box::new(RlimitSpawn),
        auth: Box::new(NoAuth),
        clock: Arc::new(SystemClock),
    })
}

/// The daemon loop. Each pass runs on the blocking pool: it is file I/O.
pub fn spawn(mur_home: std::path::PathBuf) {
    tokio::spawn(async move {
        let period = std::time::Duration::from_secs(SWEEP_INTERVAL_SECS);
        let slot = Arc::new(BrokerSlot::default());
        loop {
            let home = mur_home.clone();
            let slot = slot.clone();
            // Intake first, so a request that just became pending is asked about in the same pass.
            let pass = move || {
                sweep_with(&home, &slot);
                super::approvals::approval_pass(&home, &slot);
            };
            if let Err(e) = tokio::task::spawn_blocking(pass).await {
                tracing::warn!(error = %e, "git push sweep panicked");
            }
            tokio::time::sleep(period).await;
        }
    });
}

#[derive(serde::Serialize)]
struct Status<'a> {
    state: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    action_hash: Option<&'a str>,
}

/// One pass with a throwaway broker slot. Tests and one-shot callers use this; the daemon keeps
/// one slot for its lifetime via [`spawn`].
pub fn sweep_inboxes(mur_home: &Path) {
    sweep_with(mur_home, &BrokerSlot::default());
}

/// One pass over every agent inbox under `mur_home`. Errors are logged per
/// file; one bad file never stops the sweep. With `git_push.enabled` off it
/// touches nothing, re-read each pass so the switch needs no restart.
pub fn sweep_with(mur_home: &Path, slot: &BrokerSlot) {
    let cfg = Config::load_or_default(&mur_home.join("config.yaml"));
    if !cfg.git_push.enabled {
        return;
    }
    let Ok(agents) = std::fs::read_dir(mur_home.join(AGENTS_DIR)) else {
        return;
    };
    for agent_home in agents.flatten().map(|e| e.path()) {
        let Some(agent) = agent_home.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Ok(files) = std::fs::read_dir(inbox_dir(&agent_home)) else {
            continue;
        };
        for path in files.flatten().map(|e| e.path()) {
            if path.extension().is_none_or(|e| e != REQUEST_EXT) {
                continue;
            }
            intake_one(mur_home, &cfg, slot, &agent_home, agent, &path);
        }
    }
}

fn intake_one(
    mur_home: &Path,
    cfg: &Config,
    slot: &BrokerSlot,
    agent_home: &Path,
    agent: &str,
    path: &Path,
) {
    let verified = AgentIdentity::load_pubkey(agent_home)
        .map_err(|e| e.to_string())
        .and_then(|pk| verify_request(path, agent, &pk).map_err(|e| e.to_string()));
    // The pack's name comes from the file name, which the agent chose, so it must be a valid id
    // before it is joined onto a path.
    let pack = match (path.parent(), path.file_stem().and_then(|s| s.to_str())) {
        (Some(dir), Some(stem)) if validate_id(stem, "request_id").is_ok() => {
            Some(pack_path(dir, stem))
        }
        _ => None,
    };
    let outcome = match verified {
        Ok(req) => Some((
            decide(mur_home, cfg, slot, agent, &req, pack.as_deref()),
            req,
        )),
        Err(e) => {
            tracing::warn!(agent, request = %path.display(), error = %e, "git push request refused");
            None
        }
    };
    // Single-shot either way, as in `snapshot_requests`: consume, then log. The pack goes with
    // it, refused or not; the broker has copied what it needs by the time `decide` returns.
    let _ = std::fs::remove_file(path);
    if let Some(p) = &pack {
        let _ = std::fs::remove_file(p);
    }
    let Some((result, req)) = outcome else { return };
    let published = match &result {
        Ok(hash) => {
            tracing::info!(agent, request_id = %req.request_id, "git push request pending approval");
            publish(
                mur_home,
                agent,
                &req.request_id,
                STATE_PENDING_APPROVAL,
                None,
                Some(hash),
            )
        }
        Err(code) => {
            tracing::info!(agent, request_id = %req.request_id, code, "git push request rejected");
            publish(
                mur_home,
                agent,
                &req.request_id,
                STATE_REJECTED,
                Some(code),
                None,
            )
        }
    };
    if let Err(e) = published {
        tracing::warn!(agent, request_id = %req.request_id, error = %e, "git push status not written");
    }
}

/// `Ok(action_hash)` once the broker holds the request pending; otherwise the code to publish.
fn decide(
    mur_home: &Path,
    cfg: &Config,
    slot: &BrokerSlot,
    agent: &str,
    req: &RequestFile,
    pack: Option<&Path>,
) -> Result<String, String> {
    if !cfg.git_push.agents.iter().any(|a| a == agent) {
        return Err(CODE_NOT_ALLOWLISTED.into());
    }
    // Asked per request. A missing file or no matching record never opens the broker at all.
    let records = FileEnrollments::new(enrollments_path(mur_home))
        .current()
        .map_err(|e| e.code().to_string())?;
    let enrolled = records
        .as_ref()
        .is_some_and(|e| e.binding(agent, &req.repo_id, &req.remote_id).is_some());
    if !enrolled {
        return Err(CODE_NOT_ENROLLED.into());
    }
    let pack = pack.ok_or_else(|| CODE_PACK_REJECTED.to_string())?;
    let within_cap = std::fs::metadata(pack)
        .is_ok_and(|m| m.is_file() && m.len() <= BrokerLimits::default().max_pack_bytes);
    if !within_cap || !req.verify_pack(pack) {
        return Err(CODE_PACK_REJECTED.into());
    }
    let broker = slot.get(mur_home).map_err(|e| e.code().to_string())?;
    broker
        .submit_verified(req, pack)
        .map_err(|e| e.code().to_string())
}

/// Atomic tmp + rename into `status/<agent>/<request_id>.yaml`.
pub(super) fn publish(
    mur_home: &Path,
    agent: &str,
    request_id: &str,
    state: &str,
    code: Option<&str>,
    action_hash: Option<&str>,
) -> anyhow::Result<()> {
    let dir = status_dir(mur_home, agent);
    std::fs::create_dir_all(&dir)?;
    let body = serde_yaml_ng::to_string(&Status {
        state,
        code,
        action_hash,
    })?;
    let tmp = dir.join(format!("{request_id}.{STATUS_EXT}.{TMP_EXT}"));
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(body.as_bytes())?;
    f.sync_all()?;
    std::fs::rename(&tmp, dir.join(format!("{request_id}.{STATUS_EXT}")))?;
    Ok(())
}

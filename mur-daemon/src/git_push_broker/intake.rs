//! Inbox intake for the git push broker: the part of the sweeper that runs
//! before any agent is enrolled (spec §6: none is, until sub-project 2).
//!
//! For each agent home, every request file in `inbox/git-push/` is verified
//! against that agent's on-disk pubkey (the agent id comes from the verified
//! path, never the payload), consumed with its pack, and answered with a
//! status. Nothing here calls `Broker::submit_request`: with no enrollment
//! record there is no `RemotePolicy` or epoch to submit against, so a request
//! that passes every check is `rejected: not_enrolled`.

use std::io::Write;
use std::path::Path;

use mur_common::config::Config;
use mur_common::git_push::{inbox_dir, status_dir};
use mur_common::identity::AgentIdentity;
use mur_git_broker::request_file::{REQUEST_EXT, pack_path, validate_id, verify_request};

/// The agent is not in `git_push.agents`.
pub const CODE_NOT_ALLOWLISTED: &str = "agent_not_allowlisted";
/// Verified and allowlisted, but no remote is enrolled for it yet.
pub const CODE_NOT_ENROLLED: &str = "not_enrolled";
const STATE_REJECTED: &str = "rejected";
const STATUS_EXT: &str = "yaml";
const TMP_EXT: &str = "tmp";
const AGENTS_DIR: &str = "agents";
/// How often the inboxes are swept. Intake only reads small files.
pub const SWEEP_INTERVAL_SECS: u64 = 5;

/// The daemon loop. Each pass runs on the blocking pool: it is file I/O.
pub fn spawn(mur_home: std::path::PathBuf) {
    tokio::spawn(async move {
        let period = std::time::Duration::from_secs(SWEEP_INTERVAL_SECS);
        loop {
            let home = mur_home.clone();
            if let Err(e) = tokio::task::spawn_blocking(move || sweep_inboxes(&home)).await {
                tracing::warn!(error = %e, "git push sweep panicked");
            }
            tokio::time::sleep(period).await;
        }
    });
}

#[derive(serde::Serialize)]
struct Status<'a> {
    state: &'a str,
    code: &'a str,
}

/// One pass over every agent inbox under `mur_home`. Errors are logged per
/// file; one bad file never stops the sweep. With `git_push.enabled` off it
/// touches nothing, re-read each pass so the switch needs no restart.
pub fn sweep_inboxes(mur_home: &Path) {
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
            intake_one(mur_home, &cfg, &agent_home, agent, &path);
        }
    }
}

fn intake_one(mur_home: &Path, cfg: &Config, agent_home: &Path, agent: &str, path: &Path) {
    let verified = AgentIdentity::load_pubkey(agent_home)
        .map_err(|e| e.to_string())
        .and_then(|pk| verify_request(path, agent, &pk).map_err(|e| e.to_string()));
    // Single-shot either way, as in `snapshot_requests`: consume, then log.
    // The pack goes with it, refused or not. Its name comes from the file
    // name, which the agent chose, so it must be a valid id before it is
    // joined onto a path.
    let _ = std::fs::remove_file(path);
    if let (Some(dir), Some(stem)) = (path.parent(), path.file_stem().and_then(|s| s.to_str()))
        && validate_id(stem, "request_id").is_ok()
    {
        let _ = std::fs::remove_file(pack_path(dir, stem));
    }
    let req = match verified {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(agent, request = %path.display(), error = %e, "git push request refused");
            return;
        }
    };
    let code = if cfg.git_push.agents.iter().any(|a| a == agent) {
        CODE_NOT_ENROLLED
    } else {
        CODE_NOT_ALLOWLISTED
    };
    tracing::info!(agent, request_id = %req.request_id, code, "git push request rejected");
    if let Err(e) = publish(mur_home, agent, &req.request_id, code) {
        tracing::warn!(agent, request_id = %req.request_id, error = %e, "git push status not written");
    }
}

/// Atomic tmp + rename into `status/<agent>/<request_id>.yaml`.
fn publish(mur_home: &Path, agent: &str, request_id: &str, code: &str) -> anyhow::Result<()> {
    let dir = status_dir(mur_home, agent);
    std::fs::create_dir_all(&dir)?;
    let body = serde_yaml_ng::to_string(&Status {
        state: STATE_REJECTED,
        code,
    })?;
    let tmp = dir.join(format!("{request_id}.{STATUS_EXT}.{TMP_EXT}"));
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(body.as_bytes())?;
    f.sync_all()?;
    std::fs::rename(&tmp, dir.join(format!("{request_id}.{STATUS_EXT}")))?;
    Ok(())
}

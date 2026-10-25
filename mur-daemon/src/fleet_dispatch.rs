//! Sweep `~/.mur/inbox/fleet-dispatch-requests/` (#2116): launch fleets that
//! a sealed agent asked for but could not sign.
//!
//! An agent dispatching a fleet routed by ANOTHER agent cannot give its child
//! the router's key (the seal denies `keys/`, and the stdin handoff only ever
//! carries the agent's own key), so it drops a signed request instead. This
//! process runs outside every sandbox: its child reads `keys/<router>/` from
//! disk exactly like a terminal `mur fleet run`, so the run's channel events
//! are signed by the router and no agent ever holds another agent's key.
//!
//! The request file is attacker-writable, so nothing in it is trusted until
//! the signature verifies against the requester's on-disk pubkey, and then
//! every gate the runtime applied is applied AGAIN here from the user's own
//! config: the request can only ask, never authorize.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::Context;
use mur_common::config::Config;
use mur_common::identity::AgentIdentity;
use mur_track::fleet_dispatch_request::{
    FLEET_DISPATCH_RECORD_FILE, FLEET_DISPATCH_REQUEST_DIR, FleetDispatchRecord,
    FleetDispatchRequest, FleetDispatchStatus,
};

/// The child's log, beside its record — the path `fleet_run` already returned.
const RUN_LOG_FILE: &str = "fleet_run.log";

pub fn spawn(mur_dir: PathBuf) {
    tokio::spawn(async move {
        let dir = mur_dir.join(FLEET_DISPATCH_REQUEST_DIR);
        loop {
            // Re-read every pass: the allowlist is the user's to change at
            // any time, and a revoked fleet must stop launching immediately.
            let cfg = Config::try_load(&mur_dir.join("config.yaml"));
            let period = cfg
                .as_ref()
                .map(|c| c.fleet_dispatch.poll_secs)
                .unwrap_or_else(|_| mur_common::config::FleetDispatchConfig::default().poll_secs);
            match &cfg {
                Ok(cfg) => {
                    if let Err(e) = sweep(&mur_dir, &dir, cfg, &mur_bin()) {
                        tracing::warn!(error = %e, "fleet-dispatch sweep failed");
                    }
                }
                // Unreadable config = no allowlist = nothing may launch. The
                // requests stay queued and expire on their freshness window.
                Err(e) => {
                    tracing::warn!(error = %e, "fleet-dispatch: config unreadable; not sweeping")
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(period.max(1))).await;
        }
    });
}

/// The `mur` CLI to launch: `MUR_BIN` if set, else the `mur` beside this
/// binary (they ship together), else a PATH lookup as the last resort.
fn mur_bin() -> PathBuf {
    if let Some(v) = std::env::var_os("MUR_BIN") {
        return PathBuf::from(v);
    }
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("mur")))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from("mur"))
}

/// Same charset agent directories use; anything else is a traversal attempt.
fn valid_agent_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

pub(crate) fn sweep(
    mur_dir: &Path,
    dir: &Path,
    cfg: &Config,
    mur_bin: &Path,
) -> anyhow::Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    // One progress slot per fleet: two requests for the same fleet in one
    // pass would both see it idle, because the first child has not yet
    // written its progress file.
    let mut launched: HashSet<String> = HashSet::new();
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "yaml"))
        .collect();
    paths.sort();
    for path in paths {
        // Single-shot either way: a rejected request must not be retried
        // forever, and a launched one is done.
        let verdict = process_one(mur_dir, &path, cfg, mur_bin, &mut launched);
        let _ = std::fs::remove_file(&path);
        if let Err(e) = verdict {
            tracing::warn!(request = %path.display(), error = %format!("{e:#}"), "fleet dispatch request rejected");
        }
    }
    Ok(())
}

fn process_one(
    mur_dir: &Path,
    path: &Path,
    cfg: &Config,
    mur_bin: &Path,
    launched: &mut HashSet<String>,
) -> anyhow::Result<()> {
    let req: FleetDispatchRequest =
        serde_yaml_ng::from_str(&std::fs::read_to_string(path).context("read request")?)
            .context("parse request")?;
    let d = &req.dispatch;
    // Shape gates first: these strings become path segments below.
    anyhow::ensure!(valid_agent_name(&d.requested_by), "invalid agent name");
    anyhow::ensure!(
        mur_common::fleet::valid_fleet_name(&d.fleet),
        "invalid fleet name"
    );
    anyhow::ensure!(
        mur_core::run_status::valid_run_id(&d.run_id)
            && d.run_id.starts_with(&format!("fleet-{}-", d.fleet)),
        "invalid run id"
    );
    anyhow::ensure!(
        req.is_fresh(chrono::Utc::now(), cfg.fleet_dispatch.request_max_age_secs),
        "outside freshness window"
    );
    // Trust anchor: the identity in the requester's own home, which no other
    // agent can write. From here on `requested_by` is a verified fact.
    let identity = AgentIdentity::load(&mur_dir.join("agents").join(&d.requested_by))
        .map_err(|e| anyhow::anyhow!("load agent identity: {e}"))?;
    anyhow::ensure!(
        req.verify(&identity.verifying_key_bytes()),
        "signature verification failed"
    );

    let run_dir = mur_dir.join(mur_common::paths::RUNS).join(&d.run_id);
    let record_path = run_dir.join(FLEET_DISPATCH_RECORD_FILE);
    // A record means this run id was already decided: a replayed request.
    // Checked before writing anything, so a replay cannot overwrite the
    // original verdict either.
    anyhow::ensure!(!record_path.exists(), "run id already used (replay)");

    match authorize_and_launch(mur_dir, &req, cfg, mur_bin, &run_dir, launched) {
        Ok(router) => {
            write_record(
                &record_path,
                &req,
                Some(router),
                FleetDispatchStatus::Launched,
                None,
            )?;
            tracing::info!(fleet = %d.fleet, run_id = %d.run_id, dispatched_by = %d.requested_by, "fleet dispatched by daemon");
            Ok(())
        }
        Err(e) => {
            // Verified requester: leave the reason where its run handle points,
            // so `mur_job_status <run_id>` does not just stay silent.
            let _ = write_record(
                &record_path,
                &req,
                None,
                FleetDispatchStatus::Rejected,
                Some(format!("{e:#}")),
            );
            Err(e)
        }
    }
}

/// Re-apply every gate `fleet_run` applies, from the user's config — then
/// launch. Returns the router the child will sign as.
fn authorize_and_launch(
    mur_dir: &Path,
    req: &FleetDispatchRequest,
    cfg: &Config,
    mur_bin: &Path,
    run_dir: &Path,
    launched: &mut HashSet<String>,
) -> anyhow::Result<String> {
    let d = &req.dispatch;
    anyhow::ensure!(
        mur_agent_runtime::tools::fleet_run::allowed(&cfg.fleet_run, &d.requested_by, &d.fleet),
        "agent '{}' may not run fleet '{}' (fleet_run.agents / fleet_run.fleets)",
        d.requested_by,
        d.fleet
    );
    if let Some(dir) = &d.cwd {
        anyhow::ensure!(Path::new(dir).is_absolute(), "cwd must be absolute");
    }
    let fleet = mur_core::cmd::fleet::store::load_fleet(mur_dir, &d.fleet)?;
    anyhow::ensure!(
        !fleet
            .members
            .iter()
            .any(|m| m.eq_ignore_ascii_case(&d.requested_by)),
        "fleet '{}' lists the requesting agent as a member (self-delegation)",
        d.fleet
    );
    let router = fleet.router_or_concierge().to_string();
    // This path exists for a router that is NOT the requester; a same-router
    // fleet uses the runtime's local handoff and has no business here.
    anyhow::ensure!(
        !router.eq_ignore_ascii_case(&d.requested_by),
        "fleet '{}' is routed by the requester itself; dispatch it locally",
        d.fleet
    );
    anyhow::ensure!(
        !launched.contains(&d.fleet),
        "a {} run was launched earlier in this sweep",
        d.fleet
    );
    if let Some(active) = mur_agent_runtime::tools::fleet_run::live_run_id(mur_dir, &d.fleet) {
        anyhow::bail!("a {} run is already live: run_id {active}", d.fleet);
    }

    let args = mur_agent_runtime::tools::fleet_run::dispatch_argv(
        &d.fleet,
        d.goal.as_deref(),
        d.job.as_deref(),
        &d.run_id,
        d.cwd.as_deref().map(Path::new),
        d.cwd_inferred,
    );
    std::fs::create_dir_all(run_dir).context("create run dir")?;
    let log = std::fs::File::create(run_dir.join(RUN_LOG_FILE)).context("create run log")?;
    let log_err = log.try_clone()?;
    // No signing handoff: this process is unsealed, so the child loads the
    // router's key from disk — the whole point of launching it here.
    let mut child = std::process::Command::new(mur_bin)
        .args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(log))
        .stderr(std::process::Stdio::from(log_err))
        .spawn()
        .with_context(|| format!("spawn {}", mur_bin.display()))?;
    launched.insert(d.fleet.clone());
    // Detached; reap on a thread so it never lingers as a zombie. The run is
    // bounded by the fleet's limits and `mur fleet stop`, not by this daemon.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(router)
}

fn write_record(
    path: &Path,
    req: &FleetDispatchRequest,
    router: Option<String>,
    status: FleetDispatchStatus,
    reason: Option<String>,
) -> anyhow::Result<()> {
    let record = FleetDispatchRecord {
        run_id: req.dispatch.run_id.clone(),
        fleet: req.dispatch.fleet.clone(),
        dispatched_by: req.dispatch.requested_by.clone(),
        router,
        status,
        reason,
        decided_at: chrono::Utc::now(),
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&record)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
#[path = "fleet_dispatch_tests.rs"]
mod tests;

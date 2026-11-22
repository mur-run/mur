//! Daemon dispatch (#2116, #2081): the sealed half of the daemon handoff.
//!
//! A child spawned here holds one key, this agent's own: it may not read
//! `keys/`, and the stdin handoff carries nothing else. A run needs two
//! signers, the fleet's `router` for agent events and the control plane for
//! `System` ones. When this agent is not both (`runs_locally`), spawning
//! anyway is what produced runs of UNSIGNED channel events. So it writes one
//! signed request file and lets `mur-daemon` (outside every sandbox) launch
//! the run; the daemon's child reads keys from disk like a terminal run.

use std::path::Path;

use mur_track::fleet_dispatch_request::{
    FLEET_DISPATCH_REQUEST_DIR, FLEET_RUN_LOG_FILE, FleetDispatch, FleetDispatchRequest,
};

use super::messages::needs_the_daemon_because;
use super::{FleetRunTool, ToolError, ToolOutput, dispatched_reply};

pub(super) fn dispatch_via_daemon(
    tool: &FleetRunTool,
    dispatch: FleetDispatch,
    router: &str,
    stale_timeout: bool,
) -> Result<ToolOutput, ToolError> {
    let fleet = dispatch.fleet.clone();
    let run_id = dispatch.run_id.clone();
    let cfg = mur_common::config::Config::load_or_default(&tool.mur_home.join("config.yaml"))
        .fleet_dispatch;

    // Without a key there is nothing the daemon could verify, and an
    // unverifiable request is one it must reject. Say so now, not later.
    let why = needs_the_daemon_because(&fleet, &tool.agent_name, router);
    let Some(identity) = tool.signing.as_ref() else {
        return Err(ToolError::Execution(format!(
            "{why}, so it has to be launched by the MUR daemon, and this agent has no signing \
             key to sign that request with. Run it from a terminal instead: mur fleet run {fleet}"
        )));
    };
    // A request nobody sweeps is a run that silently never starts, while the
    // caller holds a handle that claims "dispatched".
    if !daemon_alive(&tool.mur_home, cfg.heartbeat_stale_secs, chrono::Utc::now()) {
        return Err(ToolError::Execution(format!(
            "{why}, so the MUR daemon has to launch it, and the daemon is not running. Start \
             it with `mur daemon start`, then dispatch again."
        )));
    }

    let req =
        FleetDispatchRequest::create(dispatch, identity, tool.key_version, chrono::Utc::now());
    write_request(&tool.mur_home, &req)
        .map_err(|e| ToolError::Execution(format!("write fleet dispatch request: {e}")))?;

    let log_path =
        mur_common::paths::fleet_run_dir(&tool.mur_home, &fleet, &run_id).join(FLEET_RUN_LOG_FILE);
    let mut reply = dispatched_reply(&run_id, &fleet, &log_path, stale_timeout);
    reply["via"] = serde_json::json!("daemon");
    reply["signed_as"] = serde_json::json!(router);
    Ok(reply.to_string().into())
}

/// Atomically drop the request: tmp + rename, named by its (unique) run id.
pub(super) fn write_request(mur_home: &Path, req: &FleetDispatchRequest) -> anyhow::Result<()> {
    let dir = mur_home.join(FLEET_DISPATCH_REQUEST_DIR);
    std::fs::create_dir_all(&dir)?;
    let name = format!("{}.yaml", req.dispatch.run_id);
    let tmp = dir.join(format!(".{name}.tmp"));
    std::fs::write(&tmp, serde_yaml_ng::to_string(req)?)?;
    std::fs::rename(&tmp, dir.join(name))?;
    Ok(())
}

/// Is the daemon's heartbeat fresh? A pid probe is not used: whether a sealed
/// process may signal another is up to the sandbox, the heartbeat is not.
///
/// Reads [`mur_common::paths::daemon_heartbeat_path`], not `murmurd.lock`:
/// the sandbox grants a read on the heartbeat DIRECTORY (Landlock pins a file
/// grant to one inode), and only a daemon that sweeps the request inbox
/// writes it — an older daemon correctly reads as down.
pub(crate) fn daemon_alive(
    mur_home: &Path,
    stale_secs: u64,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    let Ok(body) = std::fs::read_to_string(mur_common::paths::daemon_heartbeat_path(mur_home))
    else {
        return false;
    };
    serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("heartbeat_at")?.as_str().map(str::to_owned))
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok())
        .is_some_and(|hb| {
            let age = now.signed_duration_since(hb.with_timezone(&chrono::Utc));
            age <= chrono::Duration::seconds(stale_secs as i64)
        })
}

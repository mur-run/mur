//! The calls a client makes around the card: build a plan, apply an
//! approval, read the overview. murmur and Hub share them so the two cannot
//! drift (SSH setup spec, Goal 6). `plan_for` / `apply_for` are murmur's
//! `ssh_card::{build, run_apply}` (#2370) without the TUI.

use std::path::{Path, PathBuf};

use anyhow::Result;
use mur_common::agent::SshEntitlement;
use mur_ssh_request::RequestHost;

use super::super::status::{self, SshStatus};
use super::life::AgentLifecycle;
use super::pending::{OpenRequest, still_open};
use super::probe::{Probe, System};
use super::{Applied, Approval, SetupPlan, apply_and_restart, plan};

/// Validate a typed `host` / `port` with the request validator.
#[allow(dead_code)] // Hub-only (mur-hub-gui ssh_admin); the bin crate does not call it.
pub fn parse_target(host: &str, port: Option<u16>) -> Result<RequestHost, String> {
    RequestHost::parse(host, port).map_err(|e| e.to_string())
}

/// The plan for `agent` from its current profile: one `ssh-keyscan`.
pub fn plan_for(
    mur_home: &Path,
    agent: &str,
    target: RequestHost,
    user: Option<String>,
) -> Result<SetupPlan, String> {
    let (path, profile) =
        crate::cmd::agent::load_profile_for_edit(agent).map_err(|e| format!("{e:#}"))?;
    let agent_home = path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| mur_home.join("agents").join(agent));
    plan(
        &System,
        agent,
        &agent_home,
        &profile.entitlements,
        target,
        user,
    )
    .map_err(|e| format!("{e:#}"))
}

/// `apply_and_restart` with the quiet lifecycle: `mur_exe agent start` runs
/// as a child with its output captured, so no caller's screen is torn.
pub fn apply_for(
    mur_home: &Path,
    agent: &str,
    a: &Approval,
    mur_exe: PathBuf,
) -> Result<Applied, String> {
    let (_, profile) =
        crate::cmd::agent::load_profile_for_edit(agent).map_err(|e| format!("{e:#}"))?;
    let mut life = AgentLifecycle::quiet(agent, mur_home, mur_exe);
    apply_and_restart(mur_home, agent, profile.identity.key_version, a, &mut life)
        .map_err(|e| format!("{e:#}"))
}

/// Everything a status view shows for one agent.
#[allow(dead_code)] // Hub-only (mur-hub-gui ssh_admin); the bin crate does not call it.
#[derive(Debug, Clone)]
pub struct Overview {
    pub ssh: SshEntitlement,
    pub status: SshStatus,
    /// Open requests; a cleared one was removed on the way.
    pub open: Vec<OpenRequest>,
    /// Fingerprints the user's ssh-agent holds now (spec "Error handling": a
    /// granted key missing from it is named).
    pub loaded: Vec<String>,
}

#[allow(dead_code)] // Hub-only (mur-hub-gui ssh_admin); the bin crate does not call it.
pub fn overview(probe: &dyn Probe, mur_home: &Path, agent: &str) -> Result<Overview> {
    let (_, p) = crate::cmd::agent::load_profile_for_edit(agent)?;
    let ssh = p.entitlements.ssh;
    let st = status::status(mur_home, agent, &ssh);
    let open = still_open(
        &mur_home.join("agents").join(agent),
        &st.pairs,
        st.requests.clone(),
    );
    let loaded = probe
        .agent_keys()
        .into_iter()
        .map(|k| k.fingerprint)
        .collect();
    Ok(Overview {
        ssh,
        status: st,
        open,
        loaded,
    })
}

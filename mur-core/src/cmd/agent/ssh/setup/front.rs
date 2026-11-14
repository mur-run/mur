//! The calls a client makes around the card: build a plan, apply an
//! approval, read the overview. murmur and Hub share them so the two cannot
//! drift (SSH setup spec, Goal 6). `plan_for` / `apply_for` are murmur's
//! `ssh_card::{build, run_apply}` (#2370) without the TUI.
//!
//! Every call reads the agent from the `mur_home` it is given, never from
//! `MUR_HOME`, and every error is a `String` a client can show as is.

use std::path::{Path, PathBuf};

use mur_common::agent::SshEntitlement;
use mur_ssh_request::RequestHost;

use super::super::status::{self, SshStatus};
use mur_agent_runtime::sandbox::policy::ExecBackend;
use mur_common::agent::Entitlements;
use mur_common::config::Config;

use super::advisory::{self, AdvisoryResult};
use super::apply::{Finished, InstallOutcome, Restart, finish, install_step};
use super::dedicated::{self, dedicated_for};
use super::install::{Installer, SshInstaller};
use super::life::AgentLifecycle;
use super::pending::{OpenRequest, still_open};
use super::plan::{DedicatedOff, sealed_every_run};
use super::preapprove::Preapproval;
use super::probe::{AgentKey, Probe, System};
use super::{Applied, Approval, SetupPlan, apply_and_restart, plan};
use crate::cmd::agent::load_profile_at;

/// The global config under `mur_home`, home of `ssh_setup.generate_key`.
const CONFIG_FILE: &str = "config.yaml";

/// `ssh_setup.generate_key`, read from disk on every call: a card that was
/// open when the switch was turned off must not act on the old value.
pub fn generate_key_enabled(mur_home: &Path) -> bool {
    Config::load_or_default(&mur_home.join(CONFIG_FILE))
        .ssh_setup
        .generate_key
}

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
    let (_, profile) = load_profile_at(mur_home, agent).map_err(|e| format!("{e:#}"))?;
    let agent_home = mur_home.join("agents").join(agent);
    let dedicated = dedicated_for(
        mur_home,
        agent,
        &profile.entitlements,
        generate_key_enabled(mur_home),
        ExecBackend::current(),
    );
    plan(
        &System,
        agent,
        &agent_home,
        &profile.entitlements,
        target,
        user,
        dedicated,
    )
    .map_err(|e| format!("{e:#}"))
}

/// Why a key may not be generated for an agent with these entitlements now.
/// The client's plan may be stale, so this is asked again at the moment of
/// generating.
pub fn generate_refusal(
    ent: &Entitlements,
    generate_key: bool,
    backend: Option<ExecBackend>,
) -> Option<DedicatedOff> {
    if !generate_key {
        return Some(DedicatedOff::SwitchOff);
    }
    (!sealed_every_run(ent, backend)).then_some(DedicatedOff::NotSealedEveryRun)
}

/// Generate the agent's dedicated key (or take the one it has) and load it
/// into the user's ssh-agent. Grants nothing: the profile is not touched
/// until Approve (K2). The client re-reads its key lists afterwards.
pub fn generate_for(mur_home: &Path, agent: &str) -> Result<AgentKey, String> {
    let (_, profile) = load_profile_at(mur_home, agent).map_err(|e| format!("{e:#}"))?;
    generate_with(
        mur_home,
        agent,
        &profile.entitlements,
        generate_key_enabled(mur_home),
        ExecBackend::current(),
        dedicated::load,
    )
}

pub(super) fn generate_with(
    mur_home: &Path,
    agent: &str,
    ent: &Entitlements,
    generate_key: bool,
    backend: Option<ExecBackend>,
    load: impl FnOnce(&dedicated::StoredKey) -> Result<(), String>,
) -> Result<AgentKey, String> {
    if let Some(off) = generate_refusal(ent, generate_key, backend) {
        return Err(format!("no dedicated key was made: {}", off.reason()));
    }
    let key = dedicated::generate(mur_home, agent)?;
    load(&key).map_err(|e| {
        format!(
            "the dedicated key exists but could not be added to ssh-agent ({e}); start ssh-agent, \
             then choose the key again"
        )
    })?;
    Ok(AgentKey {
        fingerprint: key.fingerprint,
        comment: dedicated::comment_of(&key.public_line),
        path: None,
    })
}

/// `apply_and_restart` with the quiet lifecycle: `mur_exe agent start` runs
/// as a child with its output captured, so no caller's screen is torn.
pub fn apply_for(
    mur_home: &Path,
    agent: &str,
    a: &Approval,
    mur_exe: PathBuf,
) -> Result<Applied, String> {
    let (_, profile) = load_profile_at(mur_home, agent).map_err(|e| format!("{e:#}"))?;
    let mut life = AgentLifecycle::quiet(agent, mur_home, mur_exe);
    apply_and_restart(
        mur_home,
        agent,
        profile.identity.key_version,
        a,
        &mut life,
        &SshInstaller::for_agent(mur_home, agent),
    )
    .map_err(|e| format!("{e:#}"))
}

/// After a failed install: run it again, and on success stop, preapprove and
/// start. The profile is not written again.
#[allow(dead_code)] // The clients land in slice 6c.
pub fn retry_install_for(
    mur_home: &Path,
    agent: &str,
    a: &Approval,
    mur_exe: PathBuf,
) -> Result<Finished, String> {
    let (_, profile) = load_profile_at(mur_home, agent).map_err(|e| format!("{e:#}"))?;
    let mut life = AgentLifecycle::quiet(agent, mur_home, mur_exe);
    retry_install(
        mur_home,
        agent,
        profile.identity.key_version,
        a,
        &mut life,
        &SshInstaller::for_agent(mur_home, agent),
    )
}

pub(super) fn retry_install(
    mur_home: &Path,
    agent: &str,
    key_version: u32,
    a: &Approval,
    life: &mut dyn super::apply::Lifecycle,
    installer: &dyn Installer,
) -> Result<Finished, String> {
    let install = install_step(mur_home, agent, a, installer);
    if matches!(install, InstallOutcome::Failed(_)) {
        return Ok(Finished {
            install,
            preapproval: Preapproval::NotAttempted,
            restart: Restart::HeldForInstall,
        });
    }
    finish_with(mur_home, agent, key_version, a, life, install)
}

/// "Restart anyway" after a failed install: no install, and the preapproval
/// is still written, so the first signature does not ask.
#[allow(dead_code)] // The clients land in slice 6c.
pub fn finish_for(
    mur_home: &Path,
    agent: &str,
    a: &Approval,
    mur_exe: PathBuf,
) -> Result<Finished, String> {
    let (_, profile) = load_profile_at(mur_home, agent).map_err(|e| format!("{e:#}"))?;
    let mut life = AgentLifecycle::quiet(agent, mur_home, mur_exe);
    finish_with(
        mur_home,
        agent,
        profile.identity.key_version,
        a,
        &mut life,
        InstallOutcome::NotChosen,
    )
}

pub(super) fn finish_with(
    mur_home: &Path,
    agent: &str,
    key_version: u32,
    a: &Approval,
    life: &mut dyn super::apply::Lifecycle,
    install: InstallOutcome,
) -> Result<Finished, String> {
    let (preapproval, restart) =
        finish(mur_home, agent, key_version, a, life).map_err(|e| format!("{e:#}"))?;
    Ok(Finished {
        install,
        preapproval,
        restart,
    })
}

/// Does the server accept the agent's dedicated key? Advisory only: it runs
/// outside the agent's sandbox, writes nothing and records no decision.
#[allow(dead_code)] // The clients land in slice 6c.
pub fn advisory_test_for(
    mur_home: &Path,
    agent: &str,
    host: &RequestHost,
    user: &str,
    known_hosts: &str,
) -> AdvisoryResult {
    advisory::advisory_test(mur_home, agent, host, user, known_hosts)
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
pub fn overview(probe: &dyn Probe, mur_home: &Path, agent: &str) -> Result<Overview, String> {
    let (_, p) = load_profile_at(mur_home, agent).map_err(|e| format!("{e:#}"))?;
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

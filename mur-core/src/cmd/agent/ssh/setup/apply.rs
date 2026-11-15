//! `apply_and_restart` (SSH setup spec, "Journey" 4): one profile write,
//! the public-key install when it was chosen, then stop, preapproval under
//! the `running.sentinel` flock, start. The write is the only step that can
//! roll back, so it runs first; nothing after it undoes it. A failed install
//! holds the restart: the grant is written, and the user retries or restarts
//! anyway (`front::retry_install_for`, `front::finish_for`).

use std::path::Path;

use anyhow::{Result, bail};
use mur_common::lock_file::{SentinelProbe, probe_sentinel};

use mur_ssh_request::RequestHost;

use super::super::commit;
use super::super::edit::{self, AllowOutcome, Grant, HostGrant};
use super::dedicated;
use super::install::{InstallRequest, Installed, Installer};
use super::plan::SpawnChange;
use super::preapprove::{self, Pair, Preapproval};
use crate::cmd::agent::load_profile_at;

/// Where the dedicated public key goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallTarget {
    pub user: String,
    pub host: RequestHost,
}

/// What the user approved.
#[derive(Debug, Clone)]
pub struct Approval {
    pub key_fp: String,
    pub key_label: Option<String>,
    /// Empty: the key is listed but signs for no host.
    pub hosts: Vec<HostGrant>,
    /// The spawn change to write with the grant, if any.
    pub spawn: Option<SpawnChange>,
    /// The separate consent `NoneToStrict` needs (spec "Step 3: summary").
    pub spawn_consented: bool,
    /// False: `--no-restart`. Nothing is stopped and no preapproval is
    /// written; the first signature asks once.
    pub restart: bool,
    /// Install the agent's dedicated public key on this host after the
    /// write. `key_fp` must then be the dedicated key.
    pub install: Option<InstallTarget>,
}

/// The runtime, as `apply` needs it. Injected so tests never touch a
/// service manager or a real process.
pub trait Lifecycle {
    fn running_pid(&self) -> Option<u32>;
    /// Stop the runtime and keep its supervisor from respawning it.
    fn stop(&mut self, pid: u32) -> Result<()>;
    /// Start the runtime and wait until it reports ready.
    fn start(&mut self) -> Result<()>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Restart {
    /// `--no-restart`.
    Skipped,
    /// Stopped and started again; ready.
    Restarted,
    /// It was not running, and is left that way.
    WasStopped,
    /// Written, not restarted: the old runtime would not stop.
    StopFailed(String),
    /// Written, not restarted: something else started the agent between the
    /// stop and the preapproval. Nothing more was written.
    StartedElsewhere,
    /// Written and stopped, but the new runtime did not come up.
    StartFailed(String),
    /// Written, not restarted: the public-key install failed, and the user
    /// decides between a retry and a restart without it.
    HeldForInstall,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallOutcome {
    NotChosen,
    Done {
        installed: Installed,
        user: String,
        /// `host:port`, as shown.
        host: String,
    },
    Failed(String),
}

impl InstallOutcome {
    pub fn line(&self) -> Option<String> {
        match self {
            Self::NotChosen => None,
            Self::Done {
                installed: Installed::Added,
                user,
                host,
            } => Some(format!("✓ public key installed for {user} on {host}")),
            Self::Done {
                installed: Installed::AlreadyPresent,
                ..
            } => Some("✓ public key was already installed".into()),
            Self::Failed(e) => Some(format!(
                "Granted, key not installed: {e}. Nothing was restarted."
            )),
        }
    }
}

/// What ran after the profile write: the tail of `apply_and_restart`, or a
/// retry of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finished {
    pub install: InstallOutcome,
    pub preapproval: Preapproval,
    pub restart: Restart,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    pub outcome: AllowOutcome,
    pub spawn: Option<SpawnChange>,
    pub install: InstallOutcome,
    pub preapproval: Preapproval,
    pub restart: Restart,
}

pub fn apply_and_restart(
    mur_home: &Path,
    agent: &str,
    key_version: u32,
    a: &Approval,
    life: &mut dyn Lifecycle,
    installer: &dyn Installer,
) -> Result<Applied> {
    if a.spawn == Some(SpawnChange::NoneToStrict) && !a.spawn_consented {
        bail!(
            "'{agent}' has spawn mode `none`, which lets it run no program, ssh included. \
             Switching it to `strict` needs its own consent. Nothing was changed."
        );
    }
    let grants = grants(a);
    // 4.2: one locked write covering the grant, the spawn change, the pin
    // and `known_hosts`; rolled back whole on failure.
    let mut outcome = AllowOutcome::Updated;
    commit::apply_entitlements(agent, |ent| {
        for g in &grants {
            if edit::allow(&mut ent.ssh, g) == AllowOutcome::Added {
                outcome = AllowOutcome::Added;
            }
        }
        if let Some(c) = a.spawn {
            c.apply(&mut ent.processes);
        }
        Ok(())
    })?;
    // 4.3: the install. Independent of the restart, so `--no-restart` still
    // runs it; a failure holds the restart.
    let install = install_step(mur_home, agent, a, installer);
    let done = |install, preapproval, restart| Applied {
        outcome,
        spawn: a.spawn,
        install,
        preapproval,
        restart,
    };
    if matches!(install, InstallOutcome::Failed(_)) {
        let restart = if a.restart {
            Restart::HeldForInstall
        } else {
            Restart::Skipped
        };
        return Ok(done(install, Preapproval::NotAttempted, restart));
    }
    let (preapproval, restart) = finish(mur_home, agent, key_version, a, life)?;
    Ok(done(install, preapproval, restart))
}

/// The install the approval asked for, if any. Never an `Err`: the profile is
/// already written, so a failure is an outcome the user acts on.
pub fn install_step(
    mur_home: &Path,
    agent: &str,
    a: &Approval,
    installer: &dyn Installer,
) -> InstallOutcome {
    let Some(t) = &a.install else {
        return InstallOutcome::NotChosen;
    };
    match run_install(mur_home, agent, a, t, installer) {
        Ok(installed) => InstallOutcome::Done {
            installed,
            user: t.user.clone(),
            host: t.host.canonical(),
        },
        Err(e) => InstallOutcome::Failed(format!("{e:#}")),
    }
}

fn run_install(
    mur_home: &Path,
    agent: &str,
    a: &Approval,
    t: &InstallTarget,
    installer: &dyn Installer,
) -> Result<Installed> {
    let key = dedicated::stored(mur_home, agent)
        .map_err(anyhow::Error::msg)?
        .ok_or_else(|| anyhow::anyhow!("'{agent}' has no dedicated key"))?;
    // Only the dedicated key is ever installed: putting one of the user's own
    // keys on a host is not this feature.
    if key.fingerprint != a.key_fp {
        bail!("the granted key is not the dedicated key of '{agent}'");
    }
    installer.install(&InstallRequest {
        host: &t.host,
        user: &t.user,
        public_line: &key.public_line,
        known_hosts: &approved_known_hosts(a),
    })
}

/// The host keys approved on this card, as `known_hosts` lines.
fn approved_known_hosts(a: &Approval) -> String {
    a.hosts
        .iter()
        .filter_map(|h| h.known_host.as_ref())
        .map(|kh| format!("{} {} {}\n", kh.host, kh.key_type, kh.key_base64))
        .collect()
}

/// Steps 4.4 to 4.6: stop, preapproval under the sentinel, start. Reads the
/// entitlements back from the profile, so a preapproval never covers a pair
/// the profile does not grant, whoever calls this and whenever.
pub fn finish(
    mur_home: &Path,
    agent: &str,
    key_version: u32,
    a: &Approval,
    life: &mut dyn Lifecycle,
) -> Result<(Preapproval, Restart)> {
    if !a.restart {
        return Ok((Preapproval::NotAttempted, Restart::Skipped));
    }
    let (_, profile) = load_profile_at(mur_home, agent)?;
    let ent = profile.entitlements;
    // 4.4: stop.
    let was_running = life.running_pid();
    if let Some(pid) = was_running
        && let Err(e) = life.stop(pid)
    {
        return Ok((
            Preapproval::NotAttempted,
            Restart::StopFailed(format!("{e:#}")),
        ));
    }
    // 4.5: record with the sentinel held; nothing may start the agent until
    // it is dropped (spec "Open for planning").
    let agent_home = mur_home.join("agents").join(agent);
    let preapproval = match probe_sentinel(&agent_home) {
        SentinelProbe::Free(held) => {
            let p = preapprove::record(mur_home, agent, key_version, &ent, &pairs(a));
            drop(held);
            p
        }
        SentinelProbe::Owned => {
            return Ok((Preapproval::NotAttempted, Restart::StartedElsewhere));
        }
        SentinelProbe::NoAgentDir => bail!("agent '{agent}' has no home directory"),
    };
    if was_running.is_none() {
        return Ok((preapproval, Restart::WasStopped));
    }
    // 4.6: start and wait for ready.
    let restart = match life.start() {
        Ok(()) => Restart::Restarted,
        Err(e) => Restart::StartFailed(format!("{e:#}")),
    };
    Ok((preapproval, restart))
}

fn grants(a: &Approval) -> Vec<Grant> {
    let one = |host| Grant {
        key_fp: a.key_fp.clone(),
        key_label: a.key_label.clone(),
        host,
    };
    if a.hosts.is_empty() {
        return vec![one(None)];
    }
    a.hosts.iter().map(|h| one(Some(h.clone()))).collect()
}

fn pairs(a: &Approval) -> Vec<Pair> {
    a.hosts
        .iter()
        .map(|h| Pair {
            key_fp: a.key_fp.clone(),
            key_label: a.key_label.clone(),
            host_key_fp: h.host_key_fp.clone(),
            host_label: h.label.clone(),
        })
        .collect()
}

impl Restart {
    /// The line the CLI prints; the card renders the same states.
    pub fn line(&self, agent: &str) -> Option<String> {
        match self {
            Self::Skipped => Some(format!(
                "Not restarted (--no-restart). Restart '{agent}' to use the grant; the first \
                 signature will ask you once."
            )),
            // The install line says it.
            Self::Restarted | Self::HeldForInstall => None,
            Self::WasStopped => Some(format!(
                "'{agent}' is not running; the grant applies when you start it."
            )),
            Self::StopFailed(e) => Some(format!(
                "Written, not restarted: '{agent}' could not be stopped ({e}). Restart it with \
                 `mur agent restart {agent}`."
            )),
            Self::StartedElsewhere => Some(format!(
                "Written, not restarted: '{agent}' was started by something else while it was \
                 stopped. The first signature will ask you once."
            )),
            Self::StartFailed(e) => Some(format!(
                "Written, but '{agent}' did not come up: {e}. Start it with \
                 `mur agent start {agent}`."
            )),
        }
    }
}

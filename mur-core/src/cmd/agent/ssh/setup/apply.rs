//! `apply_and_restart` (SSH setup spec, "Journey" 4): one profile write,
//! then stop, preapproval under the `running.sentinel` flock, start. The
//! write is the only step that can roll back, so it runs first; nothing
//! after it undoes it.

use std::path::Path;

use anyhow::{Result, bail};
use mur_common::lock_file::{SentinelProbe, probe_sentinel};

use super::super::commit;
use super::super::edit::{self, AllowOutcome, Grant, HostGrant};
use super::plan::SpawnChange;
use super::preapprove::{self, Pair, Preapproval};

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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    pub outcome: AllowOutcome,
    pub spawn: Option<SpawnChange>,
    pub preapproval: Preapproval,
    pub restart: Restart,
}

pub fn apply_and_restart(
    mur_home: &Path,
    agent: &str,
    key_version: u32,
    a: &Approval,
    life: &mut dyn Lifecycle,
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
    let mut written = None;
    commit::apply_entitlements(agent, |ent| {
        for g in &grants {
            if edit::allow(&mut ent.ssh, g) == AllowOutcome::Added {
                outcome = AllowOutcome::Added;
            }
        }
        if let Some(c) = a.spawn {
            c.apply(&mut ent.processes);
        }
        written = Some(ent.clone());
        Ok(())
    })?;
    let Some(ent) = written else {
        bail!("the profile write returned without running the edit");
    };
    let done = |preapproval, restart| Applied {
        outcome,
        spawn: a.spawn,
        preapproval,
        restart,
    };
    if !a.restart {
        return Ok(done(Preapproval::NotAttempted, Restart::Skipped));
    }
    // 4.4: stop.
    let was_running = life.running_pid();
    if let Some(pid) = was_running
        && let Err(e) = life.stop(pid)
    {
        return Ok(done(
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
            return Ok(done(Preapproval::NotAttempted, Restart::StartedElsewhere));
        }
        SentinelProbe::NoAgentDir => bail!("agent '{agent}' has no home directory"),
    };
    if was_running.is_none() {
        return Ok(done(preapproval, Restart::WasStopped));
    }
    // 4.6: start and wait for ready.
    let restart = match life.start() {
        Ok(()) => Restart::Restarted,
        Err(e) => Restart::StartFailed(format!("{e:#}")),
    };
    Ok(done(preapproval, restart))
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
            Self::Restarted => None,
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

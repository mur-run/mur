//! `plan`: everything the card (or `mur agent ssh allow`) shows before the
//! user approves. Reads only; the one network touch is the host scan, run
//! once per plan (SSH setup spec, `plan`).

use std::path::{Path, PathBuf};

use anyhow::Result;
use mur_agent_runtime::sandbox::policy::{ExecBackend, ExecReach, SandboxPolicy};
use mur_common::agent::{Entitlements, ProcessesEntitlement, SpawnMode};
use mur_ssh_request::{RequestHost, SSH_BINARY, ssh_spawnable};

use super::super::resolve::ScannedKey;
use super::probe::{AgentKey, Probe};
use super::verdict::{Verdict, verdict};

/// How the spawn policy must change for `ssh` to be exec'able.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnChange {
    /// `allowlist` / `strict` without `ssh`: add it. Covered by the main
    /// consent.
    AddSsh,
    /// `none`: switch to `strict` and add `ssh`. Needs its own consent
    /// (spec "Spawn mode").
    NoneToStrict,
}

impl SpawnChange {
    pub fn needed(p: &ProcessesEntitlement) -> Option<Self> {
        match p.spawn.mode {
            _ if ssh_spawnable(p) => None,
            SpawnMode::None => Some(Self::NoneToStrict),
            _ => Some(Self::AddSsh),
        }
    }

    pub fn apply(self, p: &mut ProcessesEntitlement) {
        if self == Self::NoneToStrict {
            p.spawn.mode = SpawnMode::Strict;
        }
        if !p.spawn.allowed.iter().any(|b| b == SSH_BINARY) {
            p.spawn.allowed.push(SSH_BINARY.to_string());
        }
    }
}

/// What the agent may run once the change lands, from the computed policy
/// on this platform's backend, never from a fixed string (spec "Spawn
/// mode", requirements).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecSurface {
    pub reach: ExecReach,
}

/// Whether this agent can have a dedicated key (SSH setup spec, "Key
/// generation").
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dedicated {
    /// Not offered; the card shows the reason.
    Off(DedicatedOff),
    /// Offered; no key exists yet.
    Offer,
    /// The agent's key exists. Its fingerprint is also in `keys_loaded`
    /// when it is loaded.
    Stored { fingerprint: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DedicatedOff {
    /// `ssh_setup.generate_key: false`.
    SwitchOff,
    /// Nothing keeps the agent from reading the key file (K3).
    NotSealedEveryRun,
    /// A file is where the key should be and is not a key this store wrote.
    /// The text names the file to delete.
    Unusable(String),
}

impl DedicatedOff {
    /// Why the option is disabled, as the card shows it.
    pub fn reason(&self) -> String {
        match self {
            Self::SwitchOff => "turned off by `ssh_setup.generate_key: false`".into(),
            Self::NotSealedEveryRun => {
                "this agent is not sandboxed on every run, so it could read the key file".into()
            }
            Self::Unusable(why) => why.clone(),
        }
    }
}

/// The CLI reads the host half; murmur's `/ssh` card (and Hub's, slice 5)
/// reads all of it.
#[derive(Debug, Clone)]
pub struct SetupPlan {
    pub agent: String,
    pub host: RequestHost,
    pub user: Option<String>,
    pub keys_loaded: Vec<AgentKey>,
    pub keys_unloaded: Vec<AgentKey>,
    pub scanned: Vec<ScannedKey>,
    pub verdict: Verdict,
    /// Scanned fingerprints equal to one the user stores (`Trusted` only).
    pub matched: Vec<String>,
    pub spawn: Option<SpawnChange>,
    /// Present only with a spawn change.
    pub exec_surface: Option<ExecSurface>,
    pub dedicated: Dedicated,
}

/// Build the plan for `agent` from its current entitlements. `dedicated`
/// comes from [`super::dedicated::dedicated_for`]; `plan` reads no key store.
pub fn plan(
    probe: &dyn Probe,
    agent: &str,
    agent_home: &Path,
    ent: &Entitlements,
    host: RequestHost,
    user: Option<String>,
    dedicated: Dedicated,
) -> Result<SetupPlan> {
    let scanned = probe.scan(&host)?;
    let stored = probe.stored_host_keys(&host);
    let writable = probe
        .user_known_hosts()
        .is_some_and(|kh| may_write(ent, &quiet_policy(ent, agent_home), &kh));
    let (verdict, matched) = verdict(&scanned, &stored, writable);
    let (loaded, unloaded) = split_keys(probe);
    let spawn = SpawnChange::needed(&ent.processes);
    let exec_surface = spawn.and_then(|c| {
        let mut after = ent.clone();
        c.apply(&mut after.processes);
        exec_surface(&quiet_policy(&after, agent_home))
    });
    Ok(SetupPlan {
        agent: agent.to_string(),
        host,
        user,
        keys_loaded: loaded,
        keys_unloaded: unloaded,
        scanned,
        verdict,
        matched,
        spawn,
        exec_surface,
        dedicated,
    })
}

/// The user's ssh-agent keys, then the `~/.ssh/*.pub` files not among them.
/// Re-read by the card after an `ssh-add`, so it is its own step.
pub fn split_keys(probe: &dyn Probe) -> (Vec<AgentKey>, Vec<AgentKey>) {
    let loaded = probe.agent_keys();
    let unloaded = probe
        .unloaded_keys()
        .into_iter()
        .filter(|k| !loaded.iter().any(|l| l.fingerprint == k.fingerprint))
        .collect();
    (loaded, unloaded)
}

/// True when the computed write grants cover `known_hosts` and no deny
/// takes it back: then a `known_hosts` match may be the agent's own doing.
pub fn agent_can_write(policy: &SandboxPolicy, known_hosts: &Path) -> bool {
    policy.fs_write.iter().any(|w| known_hosts.starts_with(w))
        && !policy.fs_deny.iter().any(|d| known_hosts.starts_with(d))
}

/// Whether the agent may have written `known_hosts`. Policy paths bound it
/// only when a kernel enforces them every run: with no backend, or with
/// `fail_closed_on_sandbox_error` off (the runtime may start unsealed), the
/// file is as writable as the user's own account makes it.
pub fn may_write(ent: &Entitlements, policy: &SandboxPolicy, known_hosts: &Path) -> bool {
    !sealed_every_run(ent, ExecBackend::current()) || agent_can_write(policy, known_hosts)
}

/// A kernel backend exists and the runtime refuses to start without it.
pub fn sealed_every_run(ent: &Entitlements, backend: Option<ExecBackend>) -> bool {
    backend.is_some() && ent.fail_closed_on_sandbox_error
}

pub fn exec_surface(policy: &SandboxPolicy) -> Option<ExecSurface> {
    ExecBackend::current().map(|b| ExecSurface {
        reach: policy.exec_reach(b),
    })
}

impl ExecSurface {
    /// The consent wording (spec "Spawn mode"), listing exactly what the
    /// backend's rules open.
    pub fn consent_text(&self) -> String {
        let r = &self.reach;
        if r.unrestricted {
            return "This lets the agent run any program.\n".into();
        }
        let show = |v: &[PathBuf]| -> String {
            v.iter()
                .map(|p| format!("\n  {}", p.display()))
                .collect::<String>()
        };
        let mut s = String::new();
        if !r.files.is_empty() {
            s.push_str(&format!(
                "This lets the agent run these programs:{}\n",
                show(&r.files)
            ));
        }
        if !r.dirs.is_empty() {
            s.push_str(&format!(
                "This lets the agent run any program in these directories:{}\n",
                show(&r.dirs)
            ));
        }
        if r.rw_exec {
            s.push_str(
                "On Linux this includes every directory the agent can read or write, so a \
                 program it writes there can also run.\n",
            );
        }
        s
    }
}

/// Build the sandbox policy for a preview without its start-up warnings.
///
/// `from_entitlements` warns once per inactive build-lane grant — meant for
/// the runtime's `stderr.log` at agent start. The plan is built while
/// murmur's TUI owns the screen, where a stderr line paints straight over
/// the `/ssh` card. The dropped grants still land in the policy; only the
/// log line is muted.
fn quiet_policy(ent: &Entitlements, agent_home: &Path) -> SandboxPolicy {
    tracing::subscriber::with_default(tracing::subscriber::NoSubscriber::default(), || {
        SandboxPolicy::from_entitlements(ent, agent_home)
    })
}

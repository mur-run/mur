//! `plan`: everything the card (or `mur agent ssh allow`) shows before the
//! user approves. Reads only; the one network touch is the host scan, run
//! once per plan (SSH setup spec, `plan`).

use std::path::{Path, PathBuf};

use anyhow::Result;
use mur_agent_runtime::sandbox::policy::SandboxPolicy;
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

/// The directories whose binaries the agent may run once the change lands,
/// from the computed policy, never from a fixed string (spec "Spawn mode",
/// requirements).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecSurface {
    pub dirs: Vec<PathBuf>,
    /// Linux grants Execute on every read and write grant too.
    pub linux_rw_exec: bool,
}

/// The CLI reads the host half; the key and user fields are for the
/// murmur and Hub cards (slices 4 and 5).
#[derive(Debug, Clone)]
#[cfg_attr(not(test), allow(dead_code))]
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
}

/// Build the plan for `agent` from its current entitlements.
pub fn plan(
    probe: &dyn Probe,
    agent: &str,
    agent_home: &Path,
    ent: &Entitlements,
    host: RequestHost,
    user: Option<String>,
) -> Result<SetupPlan> {
    let scanned = probe.scan(&host)?;
    let stored = probe.stored_host_keys(&host);
    let writable = probe
        .user_known_hosts()
        .is_some_and(|kh| agent_can_write(&SandboxPolicy::from_entitlements(ent, agent_home), &kh));
    let (verdict, matched) = verdict(&scanned, &stored, writable);
    let loaded = probe.agent_keys();
    let unloaded = probe
        .unloaded_keys()
        .into_iter()
        .filter(|k| !loaded.iter().any(|l| l.fingerprint == k.fingerprint))
        .collect();
    let spawn = SpawnChange::needed(&ent.processes);
    let exec_surface = spawn.map(|c| {
        let mut after = ent.clone();
        c.apply(&mut after.processes);
        exec_surface(&SandboxPolicy::from_entitlements(&after, agent_home))
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
    })
}

/// True when the computed write grants cover `known_hosts` and no deny
/// takes it back: then a `known_hosts` match may be the agent's own doing.
pub fn agent_can_write(policy: &SandboxPolicy, known_hosts: &Path) -> bool {
    policy.fs_write.iter().any(|w| known_hosts.starts_with(w))
        && !policy.fs_deny.iter().any(|d| known_hosts.starts_with(d))
}

pub fn exec_surface(policy: &SandboxPolicy) -> ExecSurface {
    let linux = cfg!(target_os = "linux");
    let mut dirs: Vec<PathBuf> = policy.fs_exec.clone();
    let extra: Vec<&PathBuf> = if linux {
        policy.fs_read.iter().chain(&policy.fs_write).collect()
    } else {
        policy.spawn_allowed_prefixes.iter().collect()
    };
    for d in extra {
        if !dirs.contains(d) {
            dirs.push(d.clone());
        }
    }
    ExecSurface {
        dirs,
        linux_rw_exec: linux,
    }
}

impl ExecSurface {
    /// The consent wording decided 2026-11-11 (spec "Spawn mode").
    pub fn consent_text(&self) -> String {
        let dirs: Vec<String> = self.dirs.iter().map(|d| d.display().to_string()).collect();
        let mut s = format!(
            "This lets the agent run any program in these directories:\n  {}\n",
            dirs.join("\n  ")
        );
        if self.linux_rw_exec {
            s.push_str(
                "On Linux this includes every directory the agent can read or write, so a \
                 program it writes there can also run.\n",
            );
        }
        s.push_str("This narrows when mur-run/mur#2343 is fixed.\n");
        s
    }
}

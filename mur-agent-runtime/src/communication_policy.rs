//! sends_to (intent filter) and accepts_from (authoritative security boundary).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub fn sends_to_allows(list: &[String], peer: &str) -> bool {
    list.iter().any(|p| glob_match(p, peer))
}

pub fn accepts_from_allows(list: &[String], caller: &str) -> bool {
    list.iter().any(|p| glob_match(p, caller))
}

fn glob_match(pattern: &str, s: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    match glob::Pattern::new(pattern) {
        Ok(p) => p.matches(s),
        Err(_) => pattern == s,
    }
}

/// Given an agents directory, return the name whose running.lock's pid matches.
/// Returns None if not found (common for CLI callers — treat as trusted user).
pub fn resolve_caller_name(agents_dir: &Path, caller_pid: u32) -> Option<String> {
    running_agents(agents_dir).remove(&caller_pid)
}

/// pid → agent name for every agent under `agents_dir` whose running.lock
/// names a live pid. One unreadable entry never hides the rest.
///
/// Liveness is `pid_alive`, deliberately NOT `lock_file::is_stale`: that
/// probe opens the sibling's sentinel read+write, which the sandbox denies
/// inside a sibling's home — the error would read as "stale", drop the
/// sibling from this map, and let its requests through as the user.
fn running_agents(agents_dir: &Path) -> HashMap<u32, String> {
    let mut out = HashMap::new();
    let Ok(entries) = std::fs::read_dir(agents_dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let lock_path = entry.path().join("running.lock");
        let Ok(Some(lock)) = mur_common::lock_file::read(&lock_path) else {
            continue;
        };
        if !mur_common::lock_file::pid_alive(lock.pid) {
            continue;
        }
        out.insert(lock.pid, lock.name);
    }
    out
}

/// Who is on the other end of an A2A socket connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Caller {
    /// This agent, or a process it spawned (tools, the MCP shim).
    SelfAgent,
    /// Another running agent, or a process that agent spawned.
    Agent(String),
    /// Not descended from any running agent: the user's CLI / GUI.
    User,
    /// The peer's pid could not be read on this platform.
    Unknown,
}

/// Name the caller by walking the peer's process lineage, nearest first,
/// until it meets a running agent. The walk (not a bare pid match) is the
/// point: a request from a tool agent B spawned must count as B, or B could
/// reach anyone by shelling out.
///
/// Known limit: a process that daemonizes (double-fork, reparented to pid 1)
/// has no lineage left and reads as `User`. Closing that needs a credential
/// on the wire, not a better walk.
pub fn identify(
    peer_pid: Option<u32>,
    self_pid: u32,
    running: &HashMap<u32, String>,
    parent: impl Fn(u32) -> Option<u32>,
) -> Caller {
    let Some(mut cur) = peer_pid.filter(|p| *p != 0) else {
        return Caller::Unknown;
    };
    for _ in 0..crate::hitl::shim_ticket::MAX_LINEAGE_DEPTH {
        if cur == self_pid {
            return Caller::SelfAgent;
        }
        if let Some(name) = running.get(&cur) {
            return Caller::Agent(name.clone());
        }
        match parent(cur) {
            Some(p) if p > 1 && p != cur => cur = p,
            _ => return Caller::User,
        }
    }
    Caller::User
}

/// Decide whether `caller` may use this agent's socket.
///
/// `accepts_from` names peer agents; the user and the agent itself are never
/// subject to it (it is a peer boundary, and refusing the owner would lock
/// them out of their own agent). An unidentifiable caller is admitted only
/// when the list admits everyone — fail closed as soon as it narrows.
pub fn admits(accepts_from: &[String], caller: &Caller) -> Result<(), String> {
    match caller {
        Caller::SelfAgent | Caller::User => Ok(()),
        Caller::Agent(name) if accepts_from_allows(accepts_from, name) => Ok(()),
        Caller::Agent(name) => Err(format!(
            "communication denied: agent '{name}' is not in this agent's accepts_from"
        )),
        Caller::Unknown if accepts_from.iter().any(|p| p == "*") => Ok(()),
        Caller::Unknown => Err(
            "communication denied: caller could not be identified and accepts_from is restricted"
                .into(),
        ),
    }
}

/// The accepts_from gate as the unix socket transport applies it: once per
/// connection, at accept time.
#[derive(Debug, Clone)]
pub struct AcceptPolicy {
    pub accepts_from: Vec<String>,
    pub agents_dir: PathBuf,
    pub self_pid: u32,
}

impl AcceptPolicy {
    pub fn check(&self, peer_pid: Option<u32>) -> Result<Caller, String> {
        let open = self.accepts_from.iter().any(|p| p == "*");
        // `*` admits every caller, so skip the directory scan entirely.
        if open {
            return Ok(Caller::Unknown);
        }
        let running = running_agents(&self.agents_dir);
        let caller = identify(
            peer_pid,
            self.self_pid,
            &running,
            crate::hitl::shim_ticket::parent_of,
        );
        admits(&self.accepts_from, &caller).map(|()| caller)
    }
}

#[cfg(test)]
#[path = "communication_policy_tests.rs"]
mod tests;

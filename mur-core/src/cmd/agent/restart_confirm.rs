//! Confirmation half of `mur agent restart` — the "did it come back?" part.
//!
//! The runtime writes `running.lock` only after it has applied its sandbox and
//! loaded its model, so a cold start can take minutes with the process alive
//! the whole time. A fixed confirmation window has to choose between declaring
//! a slow-but-healthy agent dead and stalling forever on a hung one. The middle
//! path taken here: only kick the service when no runnable fresh process exists
//! (the agent is genuinely not coming back), and when one does exist, keep
//! waiting — bounded, and failing fast if it dies.
//!
//! `launchctl kickstart -k` / `systemctl restart` kills the current instance,
//! so kicking a cold-starting agent restarts the clock from zero — the exact
//! thing that made a slow cold start look like a failed restart in the field
//! ([[mem:gotcha_restart_confirm_window_slow_startup]]).

use std::path::Path;
use std::process::Command;

use anyhow::bail;

use super::restart::{direct_respawn, kickstart_service, poll_new_lock};

/// Passive wait for launchd/systemd's natural respawn. Covers respawn latency
/// (old-process shutdown + ExitTimeOut + any ThrottleInterval) with headroom —
/// real launchd respawn can take ~2 min after rapid restarts.
const RESPAWN_WAIT_SECS: u64 = 120;

/// Short window after an active kick / direct re-spawn. The passive wait
/// already absorbed the service manager's worst-case latency; this one only
/// needs to cover a fresh start.
const RETRY_WAIT_SECS: u64 = 30;

/// Extra patience for a slow cold start (sandbox apply + model load). Only
/// spent while a runnable fresh process is alive — a process that dies fails
/// fast instead of burning the whole window, and a wrongly-"runnable" zombie
/// still gets kicked once the ceiling expires.
const SLOW_START_WAIT_SECS: u64 = 240;

/// Wait for a restarted agent's fresh `running.lock`, kicking the service only
/// when the agent is genuinely not coming back.
///
/// Returns `(pid, build_sha)` of the fresh lock once it appears, or `None`
/// when the agent could not be confirmed within the combined windows.
pub(super) fn wait_for_confirmed_lock(
    name: &str,
    agent_home: &Path,
    lock_path: &Path,
    old_pid: u32,
    has_service: bool,
    direct_pid: Option<u32>,
) -> anyhow::Result<Option<(u32, String)>> {
    // Passive: launchd KeepAlive=true respawns on process exit; wait for the
    // new lock to appear.
    let mut new_pid = poll_new_lock(lock_path, old_pid, RESPAWN_WAIT_SECS);

    if new_pid.is_none() {
        // A runnable fresh process that has not written its lock yet is a slow
        // cold start, not a failure. Kicking / re-spawning it would kill the
        // in-progress start and restart the clock. Give it real time instead —
        // whether the fresh process is the service manager's or one we
        // pre-spawned directly (no service unit).
        let fresh = fresh_process_pid(managed_pid(name), direct_pid, old_pid);
        if fresh.is_some() {
            new_pid = poll_while_runnable(fresh, lock_path, old_pid, SLOW_START_WAIT_SECS);
        }

        // No runnable process (or the slow start gave up): the agent is not
        // coming back on its own. Kick the service, or re-spawn directly. A
        // service-manager-tracked zombie counts as not-runnable here, so it is
        // kicked instead of being mistaken for a cold start.
        if new_pid.is_none() {
            let fresh_after = if has_service {
                match kickstart_service(name) {
                    Ok(true) => {
                        println!("agent '{name}': no respawn seen; kicked the service unit");
                        // The kick restarts the clock from zero, so gate the
                        // post-kick window on the service manager's new pid.
                        managed_pid(name)
                    }
                    Ok(false) => {
                        println!(
                            "agent '{name}': service kickstart failed; falling back to direct respawn"
                        );
                        Some(direct_respawn(name, agent_home)?)
                    }
                    Err(e) => {
                        // Attestation failed — fail-closed: do NOT kick the
                        // service manager and do NOT fall through to a direct
                        // spawn (which would also verify and fail). The runtime
                        // binary needs repair before the agent can restart.
                        bail!("{e:#}");
                    }
                }
            } else {
                println!("agent '{name}': no respawn seen; retrying direct respawn");
                Some(direct_respawn(name, agent_home)?)
            };
            // A kick / fresh spawn is a cold start from zero: give the fresh
            // process the same liveness-gated slow-start window. Only when no
            // fresh pid is known (e.g. the kick raced the service manager's
            // bookkeeping) fall back to the short fixed window.
            match fresh_after {
                Some(pid) => {
                    new_pid =
                        poll_while_runnable(Some(pid), lock_path, old_pid, SLOW_START_WAIT_SECS);
                }
                None => new_pid = poll_new_lock(lock_path, old_pid, RETRY_WAIT_SECS),
            }
        }
    }

    Ok(new_pid)
}

/// The pid of a fresh process alive and able to write the lock: the first of
/// the service manager's current pid or a pre-spawned direct pid that differs
/// from the pid we signalled and is runnable (not a zombie). A zombie keeps
/// `kill(pid, 0)` succeeding but will never write a lock.
fn fresh_process_pid(managed: Option<u32>, direct: Option<u32>, old_pid: u32) -> Option<u32> {
    [managed, direct]
        .into_iter()
        .flatten()
        .find(|pid| *pid != old_pid && pid_runnable(*pid))
}

/// Poll the lock, but only while `fresh_pid` is a runnable fresh process.
/// Fails fast on a dead process instead of burning the whole window.
fn poll_while_runnable(
    fresh_pid: Option<u32>,
    lock_path: &Path,
    old_pid: u32,
    secs: u64,
) -> Option<(u32, String)> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    loop {
        if let Some(l) = poll_new_lock(lock_path, old_pid, 5) {
            return Some(l);
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        if !fresh_pid.is_some_and(|pid| pid != old_pid && pid_runnable(pid)) {
            return None;
        }
    }
}

/// The pid the service manager currently tracks for `name`, if any.
#[cfg(target_os = "macos")]
fn managed_pid(name: &str) -> Option<u32> {
    let uid = unsafe { libc::getuid() };
    let out = Command::new("launchctl")
        .args(["print", &format!("gui/{uid}/run.mur.agent.{name}")])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    s.lines()
        .find_map(|l| l.trim().strip_prefix("pid = "))
        .and_then(|v| v.trim().parse::<u32>().ok())
        .filter(|pid| *pid != 0)
}

#[cfg(target_os = "linux")]
fn managed_pid(name: &str) -> Option<u32> {
    let out = Command::new("systemctl")
        .args([
            "--user",
            "show",
            &format!("mur-agent-{name}"),
            "-p",
            "MainPID",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    s.strip_prefix("MainPID=")
        .and_then(|v| v.trim().parse::<u32>().ok())
        .filter(|pid| *pid != 0)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn managed_pid(_name: &str) -> Option<u32> {
    None
}

/// `kill(pid, 0)` alone cannot tell a zombie from a running process; a process
/// state of `Z` is a zombie, and anything else with a state is alive enough to
/// eventually write a lock. Fail open on a probe hiccup — never kick a healthy
/// cold-starting agent because `ps` misbehaved; the slow-start window is
/// bounded, so a wrongly-"runnable" zombie still gets kicked at the end.
fn pid_runnable(pid: u32) -> bool {
    match Command::new("ps")
        .args(["-o", "state=", "-p", &pid.to_string()])
        .output()
    {
        Ok(o) if o.status.success() => ps_state_runnable(&String::from_utf8_lossy(&o.stdout)),
        _ => true,
    }
}

/// Parse `ps -o state=` output: a state other than `Z` (zombie) on a live
/// process means it can still write a lock. Empty output = no such process.
fn ps_state_runnable(state_out: &str) -> bool {
    let st = state_out.trim();
    !st.is_empty() && !st.starts_with('Z')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ps_state_zombie_is_not_runnable() {
        assert!(!ps_state_runnable("Z"));
    }

    #[test]
    fn ps_state_running_is_runnable() {
        assert!(ps_state_runnable("S"));
        assert!(ps_state_runnable("R+"));
        assert!(ps_state_runnable("U"));
    }

    #[test]
    fn ps_state_empty_means_no_such_process() {
        assert!(!ps_state_runnable(""));
    }

    #[test]
    fn poll_while_runnable_fails_closed_without_fresh_pid() {
        // Negative control: with no fresh pid the gate must give up in a
        // bounded time instead of burning the whole (slow-start) window —
        // a lock that never appears must not stall the caller.
        let lock =
            std::env::temp_dir().join(format!("mur-restart-confirm-none-{}", std::process::id()));
        let start = std::time::Instant::now();
        assert_eq!(poll_while_runnable(None, &lock, 1, 1), None);
        assert!(start.elapsed() < std::time::Duration::from_secs(10));
    }
}

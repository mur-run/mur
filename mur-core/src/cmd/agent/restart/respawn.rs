//! Respawn / service-kick primitives shared by `restart` and `restart_confirm`.
//! Moved out of `restart.rs` unchanged (800-line rule).

use std::fs::OpenOptions;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};

use super::super::attest::verify_runtime_at;
use super::super::{resolve_bin_dir, resolve_mur_home, stale};
use super::read_lock;

/// Spawn the runtime directly (detached) for an agent with no service unit.
///
/// Returns the spawned child's pid so callers can liveness-gate the wait for
/// its fresh `running.lock` (a slow cold start is a healthy start).
pub(in crate::cmd::agent) fn direct_respawn(name: &str, agent_home: &Path) -> Result<u32> {
    // The agent's OWN runtime, not the one beside `mur`. These are not always
    // the same file, and when they differ this path used to relaunch the exact
    // binary `--stale` had just flagged — a restart that reported success and
    // changed nothing.
    let target = stale::runtime_path_for(name);
    verify_runtime_at(&target)
        .with_context(|| format!("cannot respawn agent '{name}' — runtime attestation failed"))?;
    let mur_home = resolve_mur_home()?;
    let stderr_log = agent_home.join("stderr.log");
    let stdout_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&stderr_log)
        .map_err(|e| anyhow::anyhow!("open {} for respawn stdout: {e}", stderr_log.display()))?;
    let stderr_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&stderr_log)
        .map_err(|e| anyhow::anyhow!("open {} for respawn stderr: {e}", stderr_log.display()))?;
    let child = Command::new(&target)
        .arg("--profile")
        .arg(name)
        .env("MUR_HOME", &mur_home)
        .stdin(Stdio::null())
        .stdout(stdout_file)
        .stderr(stderr_file)
        .spawn()
        .map_err(|e| {
            anyhow::anyhow!(
                "failed to respawn runtime for '{name}' at {}: {e}",
                target.display()
            )
        })?;
    Ok(child.id())
}

/// Wait up to `secs` for `lock_path` to hold a pid different from `old_pid`.
pub(in crate::cmd::agent) fn poll_new_lock(
    lock_path: &Path,
    old_pid: u32,
    secs: u64,
) -> Option<(u32, String)> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    loop {
        std::thread::sleep(std::time::Duration::from_millis(200));
        if std::time::Instant::now() >= deadline {
            return None;
        }
        if let Ok(Some(new_lock)) = read_lock(lock_path)
            && new_lock.pid != old_pid
        {
            return Some((new_lock.pid, new_lock.build_sha));
        }
    }
}

/// Actively (re)start the agent's service unit, killing whatever instance the
/// service manager currently tracks — including a zombie whose pid never
/// matched the lock's.
///
/// Verifies the runtime before the kick; on verification failure, returns
/// `Err` (fail-closed — the caller must NOT fall through to a direct spawn).
/// Returns `Ok(false)` when the service manager command itself fails, which
/// callers may treat as "nothing kicked" and fall back to a direct respawn.
/// Returns `Ok(true)` when the kick succeeded.
#[cfg(target_os = "macos")]
pub(in crate::cmd::agent) fn kickstart_service(name: &str) -> Result<bool> {
    let symlink = resolve_bin_dir()?.join(format!("mur_agent_{name}"));
    verify_runtime_at(&symlink)
        .with_context(|| format!("cannot kick agent '{name}' — runtime attestation failed"))?;
    let uid = unsafe { libc::getuid() };
    let ok = Command::new("launchctl")
        .args([
            "kickstart",
            "-k",
            &format!("gui/{uid}/run.mur.agent.{name}"),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if ok {
        return Ok(true);
    }
    // kickstart can only kick a job launchd is tracking. After a manual
    // `launchctl bootout` (or an unload that outlived a login) the plist is
    // still on disk but not loaded — returning Ok(false) here sent the caller
    // to an UNSUPERVISED direct respawn, the exact state a service install
    // exists to prevent, and the operator's only way back was
    // `mur agent install-service`. Mirror `mur agent start`: `load -w`
    // re-registers the job and RunAtLoad brings it up by itself.
    let Some(home) = dirs::home_dir() else {
        return Ok(false);
    };
    let plist = super::super::service::service_file_in(&home, name);
    if !plist.exists() {
        return Ok(false);
    }
    let ok = Command::new("launchctl")
        .args(["load", "-w"])
        .arg(&plist)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    Ok(ok)
}

#[cfg(target_os = "linux")]
pub(in crate::cmd::agent) fn kickstart_service(name: &str) -> Result<bool> {
    let symlink = resolve_bin_dir()?.join(format!("mur_agent_{name}"));
    verify_runtime_at(&symlink)
        .with_context(|| format!("cannot kick agent '{name}' — runtime attestation failed"))?;
    let ok = Command::new("systemctl")
        .args(["--user", "restart", &format!("mur-agent-{name}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    Ok(ok)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(in crate::cmd::agent) fn kickstart_service(_name: &str) -> Result<bool> {
    Ok(false)
}

// ─── helpers ─────────────────────────────────────────────────────────

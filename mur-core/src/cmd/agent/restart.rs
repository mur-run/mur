//! `mur agent restart` — graceful restart for upgraded runtime binaries.
//!
//! Mechanism: SIGTERM the running pid (Task 5 drains the in-flight turn),
//! wait for exit, then poll for a fresh `running.lock` with a DIFFERENT pid
//! (launchd `KeepAlive=true` respawns on exit). Lock is NOT pre-removed —
//! launchd needs the process to exit, not the lock to disappear.

use std::fs;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Result, bail};
use mur_common::{AgentProfile as _AgentProfile, LockFile};

use super::{pid_alive, resolve_mur_home, resolve_runtime_target, stale};

/// Extra grace period added on top of the runtime's `stop_timeout_secs` before
/// the SIGKILL fallback fires.  The SIGKILL wait MUST outlast the runtime's own
/// drain bound so we never abort a turn mid-flight.
const RESTART_KILL_GRACE_SECS: u64 = 5;

/// Wait to see the respawned lock (no SIGKILL risk here). Generous on purpose:
/// real launchd respawn latency = the old process's shutdown + ExitTimeOut + any
/// ThrottleInterval, observed at ~2 min after rapid restarts. A shorter wait
/// false-warns "did not respawn" on a restart that actually succeeded.
const RESTART_RESPAWN_WAIT_SECS: u64 = 120;

/// Compute the total seconds to wait before firing the SIGKILL fallback.
///
/// Pure helper so the math can be unit-tested independently of live processes.
pub(crate) fn kill_wait_secs(stop_timeout_secs: u64) -> u64 {
    stop_timeout_secs + RESTART_KILL_GRACE_SECS
}

/// Return the names of running agents to restart.
///
/// Selection rules (exactly one must be active):
/// - `names` non-empty → those agents (each must have a running.lock;
///   validated before any restart happens — no partial action)
/// - `all = true` → all agents with a running.lock
/// - `stale_only = true` → only running agents where `is_stale(lock, on_disk_sha())`
///
/// If none of the three selectors is active an error is returned — bare
/// `mur agent restart` (no args) must never silently restart everything.
/// Passing explicit `names` together with `--all`/`--stale` is also an error
/// (mutually exclusive selectors).
///
/// `home` is the MUR_HOME path (e.g. `~/.mur`). Testable with a temp dir.
pub(crate) fn select_targets(
    home: &Path,
    names: &[&str],
    all: bool,
    stale_only: bool,
) -> Result<Vec<String>> {
    // Only invoke the subprocess that computes the on-disk sha when we actually
    // need it for the stale filter; pass an empty placeholder otherwise.
    let on_disk = if stale_only {
        stale::on_disk_sha()
    } else {
        String::new()
    };
    select_targets_with_on_disk(home, names, all, stale_only, &on_disk)
}

/// Inner implementation that accepts the on-disk sha as a parameter so tests
/// can inject a synthetic value without needing a real runtime binary.
pub(crate) fn select_targets_with_on_disk(
    home: &Path,
    names: &[&str],
    all: bool,
    stale_only: bool,
    on_disk: &str,
) -> Result<Vec<String>> {
    let agents_dir = home.join("agents");

    if !names.is_empty() {
        if all || stale_only {
            bail!("cannot combine an agent name with --all or --stale");
        }
        // Validate ALL names before restarting any — no partial action.
        let mut not_running: Vec<&str> = Vec::new();
        for n in names {
            let lock_path = agents_dir.join(n).join("running.lock");
            if !lock_path.exists() {
                not_running.push(n);
            }
        }
        if !not_running.is_empty() {
            bail!("agent(s) not running: {}", not_running.join(", "));
        }
        return Ok(names.iter().map(|n| n.to_string()).collect());
    }

    // Require an explicit selector — never enumerate implicitly.
    if !all && !stale_only {
        bail!("specify an agent name, --all, or --stale");
    }

    // Enumerate all agents with a running.lock
    let mut targets: Vec<String> = Vec::new();
    if !agents_dir.exists() {
        return Ok(targets);
    }

    for entry in fs::read_dir(&agents_dir)? {
        let entry = entry?;
        let agent_name = entry.file_name().to_string_lossy().to_string();
        let lock_path = entry.path().join("running.lock");
        if !lock_path.exists() {
            continue;
        }
        if stale_only {
            let bytes = match fs::read(&lock_path) {
                Ok(b) => b,
                Err(_) => continue,
            };
            let lock: LockFile = match serde_json::from_slice(&bytes) {
                Ok(l) => l,
                Err(_) => continue,
            };
            if !stale::is_stale(&lock, on_disk) {
                continue;
            }
        }
        targets.push(agent_name);
    }
    targets.sort();
    Ok(targets)
}

/// Gracefully restart one or more agents.
///
/// - `names` / `all` / `stale`: select targets (mirror `cmd_stop`).
/// - `dry_run`: print targets and return without acting.
pub fn cmd_restart(names: &[String], all: bool, stale: bool, dry_run: bool) -> Result<()> {
    let mur_home = resolve_mur_home()?;
    let agents_dir = mur_home.join("agents");

    let names_ref: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
    let targets = select_targets(&mur_home, &names_ref, all, stale)?;

    if targets.is_empty() {
        println!("No agents to restart.");
        return Ok(());
    }

    if dry_run {
        let on_disk = if stale {
            stale::on_disk_sha()
        } else {
            String::new()
        };
        for t in &targets {
            if stale {
                let lock_path = agents_dir.join(t).join("running.lock");
                let old_sha = read_build_sha(&lock_path).unwrap_or_else(|| "(unknown)".to_string());
                println!(
                    "[dry-run] would restart '{t}' (running={}, on-disk={})",
                    short8(&old_sha),
                    short8(&on_disk)
                );
            } else {
                println!("[dry-run] would restart '{t}'");
            }
        }
        return Ok(());
    }

    let on_disk = stale::on_disk_sha();
    let mut any_err = false;

    for agent_name in &targets {
        if let Err(e) = restart_one(agent_name, &agents_dir, &on_disk) {
            eprintln!("error restarting '{agent_name}': {e}");
            any_err = true;
        }
    }

    if any_err {
        bail!("one or more agents failed to restart");
    }
    Ok(())
}

/// Restart a single agent: SIGTERM, wait for exit, poll for fresh lock.
fn restart_one(name: &str, agents_dir: &Path, on_disk_sha: &str) -> Result<()> {
    let agent_home = agents_dir.join(name);
    let lock_path = agent_home.join("running.lock");

    if !lock_path.exists() {
        bail!("agent '{name}' is not running");
    }

    let bytes = fs::read(&lock_path).map_err(|e| anyhow::anyhow!("read lock for '{name}': {e}"))?;
    let lock: LockFile = serde_json::from_slice(&bytes)
        .map_err(|e| anyhow::anyhow!("parse lock for '{name}': {e}"))?;
    let old_pid = lock.pid;
    let old_sha = lock.build_sha.clone();

    // Whether a launchd/systemd service unit is installed for this agent —
    // computed before signalling so we know, once the old pid is dead,
    // whether to expect an automatic respawn or do it ourselves.
    let has_service = service_unit_path(name)
        .map(|p| service_unit_exists(&p))
        .unwrap_or(false);

    // Load stop_timeout_secs from the agent's profile (mirrors cmd_stop).
    // The SIGKILL fallback must wait at least this long so the runtime's
    // cooperative drain can complete before we force-kill.
    let stop_timeout = {
        let pp = agent_home.join("profile.yaml");
        fs::read_to_string(&pp)
            .ok()
            .and_then(|y| serde_yaml_ng::from_str::<_AgentProfile>(&y).ok())
            .map(|p| p.lifecycle.stop_timeout_secs)
            .unwrap_or(15)
    };

    // ── SIGTERM (drain) ───────────────────────────────────────────────
    #[cfg(unix)]
    unsafe {
        libc::kill(old_pid as libc::pid_t, libc::SIGTERM);
    }

    // Wait for the old pid to exit.  Timeout = stop_timeout_secs + RESTART_KILL_GRACE_SECS
    // so the SIGKILL fallback never fires while the runtime is still draining.
    let term_deadline =
        std::time::Instant::now() + std::time::Duration::from_secs(kill_wait_secs(stop_timeout));
    while std::time::Instant::now() < term_deadline {
        if !pid_alive(old_pid) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    // Fallback SIGKILL if drain timed out (mirrors cmd_stop)
    if pid_alive(old_pid) {
        #[cfg(unix)]
        unsafe {
            libc::kill(old_pid as libc::pid_t, libc::SIGKILL);
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }

    // ── No installed service → nothing will respawn us automatically ──
    // Spawn the runtime directly, detached, so the respawn-poll loop below
    // (shared with the service-managed path) has a fresh process to find.
    if !has_service {
        println!("agent '{name}' has no service installed; respawning runtime directly");
        let target = resolve_runtime_target();
        let mur_home = resolve_mur_home()?;
        let stderr_log = agent_home.join("stderr.log");
        let stdout_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&stderr_log)
            .map_err(|e| {
                anyhow::anyhow!("open {} for respawn stdout: {e}", stderr_log.display())
            })?;
        let stderr_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&stderr_log)
            .map_err(|e| {
                anyhow::anyhow!("open {} for respawn stderr: {e}", stderr_log.display())
            })?;
        Command::new(&target)
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
    }

    // ── Poll for fresh running.lock with a different pid ──────────────
    // launchd KeepAlive=true respawns on process exit; we wait up to
    // RESTART_RESPAWN_WAIT_SECS for the new lock to appear.
    let respawn_deadline =
        std::time::Instant::now() + std::time::Duration::from_secs(RESTART_RESPAWN_WAIT_SECS);
    let new_pid = loop {
        std::thread::sleep(std::time::Duration::from_millis(200));
        if std::time::Instant::now() >= respawn_deadline {
            break None;
        }
        if let Ok(Some(new_lock)) = read_lock(&lock_path)
            && new_lock.pid != old_pid
        {
            break Some((new_lock.pid, new_lock.build_sha));
        }
    };

    match new_pid {
        Some((_pid, new_sha)) => {
            println!(
                "agent '{name}' restarted ({} → {})",
                short8(&old_sha),
                short8(if new_sha.is_empty() {
                    on_disk_sha
                } else {
                    &new_sha
                }),
            );
        }
        None => {
            eprintln!(
                "note: '{name}' was stopped; its respawn was not seen within {RESTART_RESPAWN_WAIT_SECS} s — \
                 launchd may still be bringing it up. Check 'mur agent runtime-doctor' shortly; \
                 if it stays down, confirm launchd KeepAlive is enabled."
            );
        }
    }

    Ok(())
}

// ─── helpers ─────────────────────────────────────────────────────────

/// Whether a service-unit file exists at `path`. Pure and OS-agnostic —
/// callers build the path with [`service_unit_path`], which is the only
/// cfg-gated piece.
fn service_unit_exists(path: &Path) -> bool {
    path.exists()
}

/// Build the expected service-unit path for `name`, mirroring the exact
/// paths `cmd_install_service` writes in `service.rs`:
///   - macOS: `~/Library/LaunchAgents/run.mur.agent.<name>.plist`
///   - Linux: `$XDG_CONFIG_HOME/systemd/user/mur-agent-<name>.service`
///
/// Returns `None` when the home/config dir can't be resolved, or on
/// unsupported platforms (mirrors `install-service`'s platform gate).
#[cfg(target_os = "macos")]
fn service_unit_path(name: &str) -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(format!("Library/LaunchAgents/run.mur.agent.{name}.plist")))
}

#[cfg(target_os = "linux")]
fn service_unit_path(name: &str) -> Option<PathBuf> {
    Some(dirs::config_dir()?.join(format!("systemd/user/mur-agent-{name}.service")))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn service_unit_path(_name: &str) -> Option<PathBuf> {
    None
}

fn read_lock(path: &Path) -> std::io::Result<Option<LockFile>> {
    mur_common::lock_file::read(path)
}

fn read_build_sha(lock_path: &Path) -> Option<String> {
    let bytes = fs::read(lock_path).ok()?;
    let lock: LockFile = serde_json::from_slice(&bytes).ok()?;
    Some(lock.build_sha)
}

/// First 8 chars of a sha, or the whole string if shorter.
fn short8(s: &str) -> &str {
    let end = s.char_indices().nth(8).map(|(i, _)| i).unwrap_or(s.len());
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::{LockFile, agent::LockTransports};
    use std::fs;

    fn write_lock(path: &Path, pid: u32, build_sha: &str) {
        let lock = LockFile {
            schema: 1,
            uuid: "test-uuid".to_string(),
            name: "test-agent".to_string(),
            pid,
            ppid: 1,
            started_at: "2026-01-01T00:00:00Z".to_string(),
            binary_version: "0.0.0".to_string(),
            transports: LockTransports {
                stdio: true,
                unix_socket: None,
                tcp: None,
                webhook: None,
            },
            card_digest: "abc".to_string(),
            capabilities: vec![],
            build_sha: build_sha.to_string(),
            proto_version: 1,
        };
        fs::write(path, serde_json::to_vec(&lock).unwrap()).unwrap();
    }

    /// Fix 1: bare `mur agent restart` (no name, no --all, no --stale) must
    /// return an error — never silently enumerate all running agents.
    #[test]
    fn select_targets_no_args_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let agents = home.join("agents");

        // Even with a running agent present, no-args must bail.
        let alpha_dir = agents.join("alpha");
        fs::create_dir_all(&alpha_dir).unwrap();
        write_lock(&alpha_dir.join("running.lock"), 1111, "somesha");

        let result = select_targets_with_on_disk(home, &[], false, false, "somesha");
        assert!(result.is_err(), "bare restart with no selector must error");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("--all") || msg.contains("--stale"),
            "error message should mention --all or --stale, got: {msg}"
        );
    }

    /// Fix 2: `select_targets_with_on_disk` stale_only branch returns exactly
    /// the agent whose build_sha differs from the injected on-disk sha.
    #[test]
    fn select_targets_stale_only_returns_stale_agent() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let agents = home.join("agents");

        // Agent "alpha": stale sha
        let alpha_dir = agents.join("alpha");
        fs::create_dir_all(&alpha_dir).unwrap();
        write_lock(&alpha_dir.join("running.lock"), 1111, "oldsha000000");

        // Agent "beta": up-to-date sha (matches injected on_disk)
        let beta_dir = agents.join("beta");
        fs::create_dir_all(&beta_dir).unwrap();
        write_lock(&beta_dir.join("running.lock"), 2222, "cur000000000");

        let result = select_targets_with_on_disk(home, &[], false, true, "cur000000000").unwrap();
        assert_eq!(
            result,
            vec!["alpha"],
            "only the stale agent should be returned"
        );
    }

    #[test]
    fn select_targets_named_not_running_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        fs::create_dir_all(home.join("agents").join("ghost")).unwrap();
        // No running.lock
        let result = select_targets(home, &["ghost"], false, false);
        assert!(result.is_err());
    }

    #[test]
    fn select_targets_multiple_names_both_running() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let a_dir = home.join("agents").join("a");
        let b_dir = home.join("agents").join("b");
        fs::create_dir_all(&a_dir).unwrap();
        fs::create_dir_all(&b_dir).unwrap();
        write_lock(&a_dir.join("running.lock"), 1111, "shaaaaaaaaaa");
        write_lock(&b_dir.join("running.lock"), 2222, "shabbbbbbbb");

        let mut result = select_targets_with_on_disk(home, &["a", "b"], false, false, "").unwrap();
        result.sort();
        assert_eq!(result, vec!["a", "b"]);
    }

    #[test]
    fn select_targets_multiple_names_one_not_running_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let a_dir = home.join("agents").join("a");
        let b_dir = home.join("agents").join("b");
        fs::create_dir_all(&a_dir).unwrap();
        fs::create_dir_all(&b_dir).unwrap();
        write_lock(&a_dir.join("running.lock"), 1111, "shaaaaaaaaaa");
        // No running.lock for 'b'

        let result = select_targets_with_on_disk(home, &["a", "b"], false, false, "");
        assert!(
            result.is_err(),
            "must fail-closed when any name isn't running"
        );
    }

    #[test]
    fn select_targets_names_and_all_mutually_exclusive() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        fs::create_dir_all(home.join("agents")).unwrap();

        let result = select_targets_with_on_disk(home, &["a"], true, false, "");
        assert!(result.is_err(), "names + --all must be rejected");
    }

    #[test]
    fn select_targets_empty_agents_dir_returns_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        fs::create_dir_all(home.join("agents")).unwrap();
        let result = select_targets(home, &[], true, false).unwrap();
        assert!(result.is_empty());
    }

    /// The pure exists-helper simply reflects on-disk state for the path
    /// it's given — path construction is cfg-gated separately and not
    /// exercised here (this test is OS-agnostic).
    #[test]
    fn service_unit_exists_reflects_disk_state() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("run.mur.agent.test.plist");
        assert!(!service_unit_exists(&path), "must be false before creation");
        fs::write(&path, b"unit").unwrap();
        assert!(
            service_unit_exists(&path),
            "must be true once the file exists"
        );
    }

    /// Fix 1: SIGKILL-fallback wait must be derived from stop_timeout_secs so
    /// it always outlasts the runtime's cooperative drain bound.
    #[test]
    fn kill_wait_secs_exceeds_stop_timeout() {
        // Default: 15 s drain + 5 s grace = 20 s
        assert_eq!(kill_wait_secs(15), 20);
        // Raised: 60 s drain + 5 s grace = 65 s  (never truncated to 30)
        assert_eq!(kill_wait_secs(60), 65);
        // Grace is always exactly RESTART_KILL_GRACE_SECS
        assert_eq!(kill_wait_secs(0), RESTART_KILL_GRACE_SECS);
        // Result always strictly exceeds the drain bound
        for t in [1u64, 15, 30, 60, 120] {
            assert!(kill_wait_secs(t) > t, "kill wait must exceed drain bound");
        }
    }
}

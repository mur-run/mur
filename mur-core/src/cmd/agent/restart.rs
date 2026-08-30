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

use anyhow::{Context, Result, bail};
use mur_common::{AgentProfile as _AgentProfile, LockFile};

use super::attest::verify_runtime_at;
use super::{pid_alive, resolve_bin_dir, resolve_mur_home, restart_confirm, stale};

/// Extra grace period added on top of the runtime's `stop_timeout_secs` before
/// the SIGKILL fallback fires.  The SIGKILL wait MUST outlast the runtime's own
/// drain bound so we never abort a turn mid-flight.
const RESTART_KILL_GRACE_SECS: u64 = 5;

/// Fallback `stop_timeout_secs` used when the agent's `profile.yaml` cannot be
/// read or parsed. Kept as a named const (not a bare literal) so the value
/// used in the SIGKILL-timing computation can never drift from the value
/// reported in the operator-facing warning.
const DEFAULT_STOP_TIMEOUT_SECS: u64 = 15;

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
/// - `stale_only = true` → only running agents where `is_stale` holds against
///   [`stale::on_disk_sha_for`] — that agent's OWN runtime, not a global one
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
    select_targets_with_on_disk(home, names, all, stale_only, &stale::on_disk_sha_for)
}

/// Inner implementation that accepts the on-disk sha *resolver* so tests can
/// inject a synthetic value without needing a real runtime binary.
///
/// It is a per-agent resolver, not one string: see [`stale::on_disk_sha_for`].
pub(crate) fn select_targets_with_on_disk(
    home: &Path,
    names: &[&str],
    all: bool,
    stale_only: bool,
    on_disk: &dyn Fn(&str) -> String,
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
            if !stale::is_stale(&lock, &on_disk(&agent_name)) {
                continue;
            }
        }
        targets.push(agent_name);
    }
    targets.sort();
    Ok(targets)
}

/// One agent's restart outcome, for the end-of-run summary.
struct RestartReport {
    ok: bool,
    detail: String,
}

/// Restart every agent in `targets`, then print a final verdict table. This
/// is the accountability step: fire-and-forget restarts are exactly how a
/// stopped concierge went unnoticed in the field. Errors and unconfirmed
/// agents both fail the run — after the table, so the user sees the whole
/// picture, not the first casualty.
fn run_restarts(targets: &[String], agents_dir: &Path) -> Result<()> {
    let mut reports: Vec<(String, RestartReport)> = Vec::new();
    for agent_name in targets {
        let on_disk = stale::on_disk_sha_for(agent_name);
        let report = match restart_one(agent_name, agents_dir, &on_disk) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("error restarting '{agent_name}': {e:#}");
                RestartReport {
                    ok: false,
                    detail: format!("'{agent_name}' restart errored: {e:#}"),
                }
            }
        };
        reports.push((agent_name.clone(), report));
    }
    let failed = reports.iter().filter(|(_, r)| !r.ok).count();
    println!();
    println!(
        "restart summary — {}/{} confirmed running:",
        reports.len() - failed,
        reports.len()
    );
    for (_, r) in &reports {
        println!("  {} {}", if r.ok { "✓" } else { "✗" }, r.detail);
    }
    if failed > 0 {
        bail!("{failed} agent(s) not confirmed running — see the summary above");
    }
    Ok(())
}

/// Restart every STALE agent except the excluded names — the
/// `mur update --restart-agents` entry point. Exclusions come from
/// `update.restart_exclude` in `~/.mur/config.yaml` and are honored even when
/// the excluded agent is stale (the point: some agents must never be bounced
/// unattended). Unlike `cmd_restart`, an excluded-but-stale agent is reported,
/// not an error.
pub fn restart_stale_excluding(exclude: &[String]) -> Result<()> {
    let mur_home = resolve_mur_home()?;
    let agents_dir = mur_home.join("agents");

    let all_stale = select_targets(&mur_home, &[], false, true)?;
    let (targets, skipped): (Vec<String>, Vec<String>) =
        all_stale.into_iter().partition(|t| !exclude.contains(t));

    if !skipped.is_empty() {
        println!(
            "skipping excluded agent(s) still on the old binary: {}",
            skipped.join(", ")
        );
    }
    if targets.is_empty() {
        println!("No stale agents to restart.");
        report_unexamined(&agents_dir);
        return Ok(());
    }

    run_restarts(&targets, &agents_dir)
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
        if all || stale {
            report_unexamined(&agents_dir);
        }
        return Ok(());
    }

    if dry_run {
        for t in &targets {
            if stale {
                let lock_path = agents_dir.join(t).join("running.lock");
                let old_sha = read_build_sha(&lock_path).unwrap_or_else(|| "(unknown)".to_string());
                println!(
                    "[dry-run] would restart '{t}' (running={}, on-disk={})",
                    short8(&old_sha),
                    short8(&stale::on_disk_sha_for(t))
                );
            } else {
                println!("[dry-run] would restart '{t}'");
            }
        }
        if all || stale {
            report_unexamined(&agents_dir);
        }
        return Ok(());
    }

    run_restarts(&targets, &agents_dir)?;
    if all || stale {
        report_unexamined(&agents_dir);
    }
    Ok(())
}

/// Name the agents a bulk selector never looked at.
///
/// `--all` / `--stale` only ever consider agents with a `running.lock`, so a
/// stopped one is silently absent — and "No agents to restart." then reads as
/// "everything is current". A stopped agent picks up the new binary at its next
/// start, so this is a reporting gap rather than a staleness one; the exception
/// is an agent with a service descriptor, which is supposed to be running and
/// isn't.
fn report_unexamined(agents_dir: &Path) {
    let (stopped, should_be_running) = unexamined(agents_dir, &|n| {
        super::service::installed_service(n).is_some()
    });
    if !stopped.is_empty() {
        println!(
            "not examined ({} not running): {}",
            stopped.len(),
            stopped.join(", ")
        );
    }
    if !should_be_running.is_empty() {
        // Not an anomaly to fix, and deliberately NOT auto-started: this is
        // exactly the state `mur agent stop` leaves behind (bootout / systemctl
        // stop, descriptor left in place), so starting these would override a
        // stop the user asked for. Say what will actually happen instead.
        println!(
            "not running, service still installed: {} — each starts again at your next login; `mur agent start <name>` to bring one up now",
            should_be_running.join(", ")
        );
    }
}

/// Split the lock-less agents into `(stopped, should_be_running)`.
///
/// `has_service` is injected so this is testable without writing into the real
/// `~/Library/LaunchAgents`.
fn unexamined(agents_dir: &Path, has_service: &dyn Fn(&str) -> bool) -> (Vec<String>, Vec<String>) {
    let Ok(entries) = fs::read_dir(agents_dir) else {
        return (Vec::new(), Vec::new());
    };
    let mut stopped: Vec<String> = Vec::new();
    let mut should_be_running: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        // What makes a directory an agent is its profile — `~/.mur/agents` also
        // holds non-agent dirs (`.git`, for one), and naming those as "not
        // running" is its own small lie.
        if !entry.path().join("profile.yaml").exists() {
            continue;
        }
        if entry.path().join("running.lock").exists() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if has_service(&name) {
            should_be_running.push(name);
        } else {
            stopped.push(name);
        }
    }
    stopped.sort();
    should_be_running.sort();
    (stopped, should_be_running)
}

/// Restart a single agent: SIGTERM, wait for exit, respawn, verify the fresh
/// lock. `ok: false` means the agent was NOT confirmed running — never a
/// silent note.
fn restart_one(name: &str, agents_dir: &Path, on_disk_sha: &str) -> Result<RestartReport> {
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
        match fs::read_to_string(&pp)
            .ok()
            .and_then(|y| serde_yaml_ng::from_str::<_AgentProfile>(&y).ok())
        {
            Some(p) => p.lifecycle.stop_timeout_secs,
            None => {
                // Profile missing/unreadable/malformed: fall back to the
                // default, but say so — silently truncating an operator's
                // configured drain window is exactly the kind of thing
                // that only shows up as a mysteriously-killed agent later.
                println!(
                    "agent '{name}': could not read stop_timeout_secs from \
                     {}; defaulting to {DEFAULT_STOP_TIMEOUT_SECS}s",
                    pp.display()
                );
                DEFAULT_STOP_TIMEOUT_SECS
            }
        }
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
    // The spawned pid rides along so the confirmation window can liveness-gate
    // its slow cold start instead of treating it as "not coming back".
    let mut direct_pid: Option<u32> = None;
    if !has_service {
        println!("agent '{name}' has no service installed; respawning runtime directly");
        direct_pid = Some(direct_respawn(name, &agent_home)?);
    }

    // ── Poll for fresh running.lock with a different pid ──────────────
    // launchd KeepAlive=true respawns on process exit. Confirmation lives in
    // `restart_confirm`: it kicks the service only when no runnable fresh
    // process exists, and gives a slow cold start (sandbox + model load) time
    // to write the lock instead of killing it mid-start.
    let new_pid = restart_confirm::wait_for_confirmed_lock(
        name,
        &agent_home,
        &lock_path,
        old_pid,
        has_service,
        direct_pid,
    )?;

    Ok(match new_pid {
        Some((_pid, new_sha)) => {
            let landed = if new_sha.is_empty() {
                on_disk_sha
            } else {
                new_sha.as_str()
            };
            if restart_changed_nothing(&old_sha, on_disk_sha, landed) {
                // The process is alive, so every liveness check passes — and the
                // binary is the one we were replacing. Reporting this as success
                // is how an upgrade silently doesn't happen.
                let line = format!(
                    "'{name}' came back on the SAME binary ({}) — expected {}; its runtime is {}",
                    short8(&old_sha),
                    short8(on_disk_sha),
                    stale::runtime_path_for(name).display()
                );
                eprintln!("✗ {line}");
                return Ok(RestartReport {
                    ok: false,
                    detail: line,
                });
            }
            let line = format!(
                "agent '{name}' restarted ({} → {})",
                short8(&old_sha),
                short8(landed),
            );
            println!("{line}");
            RestartReport {
                ok: true,
                detail: line,
            }
        }
        None => {
            let line = format!(
                "'{name}' not confirmed running — check `mur agent status {name}` and its stderr.log"
            );
            eprintln!("✗ {line}");
            RestartReport {
                ok: false,
                detail: line,
            }
        }
    })
}

/// Spawn the runtime directly (detached) for an agent with no service unit.
///
/// Returns the spawned child's pid so callers can liveness-gate the wait for
/// its fresh `running.lock` (a slow cold start is a healthy start).
pub(super) fn direct_respawn(name: &str, agent_home: &Path) -> Result<u32> {
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
pub(super) fn poll_new_lock(lock_path: &Path, old_pid: u32, secs: u64) -> Option<(u32, String)> {
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
pub(super) fn kickstart_service(name: &str) -> Result<bool> {
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
    let plist = super::service::service_file_in(&home, name);
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
pub(super) fn kickstart_service(name: &str) -> Result<bool> {
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
pub(super) fn kickstart_service(_name: &str) -> Result<bool> {
    Ok(false)
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

/// Did a restart that was supposed to move the agent onto a new binary fail to?
///
/// `true` only when all three are known and the agent was expected to change
/// (`on_disk != old`) but did not (`landed == old`). A restart of an
/// already-current agent legitimately lands on the same sha, so that is NOT a
/// failure — which is exactly why this cannot be written as `landed == old`.
fn restart_changed_nothing(old: &str, on_disk: &str, landed: &str) -> bool {
    const UNKNOWN: &str = "unknown";
    if old.is_empty() || on_disk.is_empty() || old == UNKNOWN || on_disk == UNKNOWN {
        return false;
    }
    on_disk != old && landed == old
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
            sandbox: None,
        };
        fs::write(path, serde_json::to_vec(&lock).unwrap()).unwrap();
    }

    /// The staleness baseline is resolved PER AGENT, not once globally.
    ///
    /// Two agents on the same lock sha, whose own runtime symlinks resolve to
    /// different binaries: only the one whose binary actually moved is stale.
    /// The old single-string baseline could not express this — it had to call
    /// both stale or neither, which is how a `--stale` run reported success
    /// while restarting agents straight back onto their old binary.
    #[test]
    fn stale_baseline_is_resolved_per_agent() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        for name in ["keg", "devtree"] {
            let dir = home.join("agents").join(name);
            fs::create_dir_all(&dir).unwrap();
            write_lock(&dir.join("running.lock"), 1, "oldsha000000");
        }

        // 'keg' points at an upgraded binary; 'devtree' still resolves to the
        // very binary it is already running.
        let per_agent = |agent: &str| match agent {
            "keg" => "newsha111111".to_string(),
            _ => "oldsha000000".to_string(),
        };
        let targets = select_targets_with_on_disk(home, &[], false, true, &per_agent).unwrap();
        assert_eq!(targets, vec!["keg".to_string()]);

        // Negative control: with the OLD global baseline (one sha for all),
        // 'devtree' is dragged in as a false positive.
        let global = |_: &str| "newsha111111".to_string();
        let targets = select_targets_with_on_disk(home, &[], false, true, &global).unwrap();
        assert_eq!(targets, vec!["devtree".to_string(), "keg".to_string()]);
    }

    /// A bulk selector never looks at an agent without a `running.lock`, so
    /// those names have to come out somewhere — and one with a service
    /// descriptor is a different, louder problem than one you stopped.
    #[test]
    fn unexamined_separates_stopped_from_should_be_running() {
        let tmp = tempfile::tempdir().unwrap();
        let agents = tmp.path().join("agents");
        for name in ["live", "stopped", "supervised"] {
            fs::create_dir_all(agents.join(name)).unwrap();
            fs::write(agents.join(name).join("profile.yaml"), "name: x\n").unwrap();
        }
        write_lock(&agents.join("live").join("running.lock"), 1, "sha");
        // A non-agent directory must not be reported as a stopped agent.
        fs::create_dir_all(agents.join(".git")).unwrap();

        let (stopped, should_be_running) = unexamined(&agents, &|n| n == "supervised");
        assert_eq!(stopped, vec!["stopped".to_string()]);
        assert_eq!(should_be_running, vec!["supervised".to_string()]);
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

        let result = select_targets_with_on_disk(home, &[], false, false, &|_| "somesha".into());
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

        let result =
            select_targets_with_on_disk(home, &[], false, true, &|_| "cur000000000".into())
                .unwrap();
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

        let mut result =
            select_targets_with_on_disk(home, &["a", "b"], false, false, &|_| String::new())
                .unwrap();
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

        let result =
            select_targets_with_on_disk(home, &["a", "b"], false, false, &|_| String::new());
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

        let result = select_targets_with_on_disk(home, &["a"], true, false, &|_| String::new());
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

    // ── Attestation mount tests ──────────────────────────────────────────

    /// `direct_respawn` refuses to spawn when the runtime target cannot be
    /// resolved (the attestation mount canonicalizes before verifying).
    /// Negative control: the error comes from the attestation mount on the
    /// spawn path, proving the verify call exists.
    #[test]
    fn direct_respawn_refuses_unresolvable_runtime() {
        let tmp = tempfile::tempdir().unwrap();
        let agent_home = tmp.path().join("agents").join("test-agent");
        std::fs::create_dir_all(&agent_home).unwrap();
        unsafe { std::env::set_var("MUR_AGENT_RUNTIME_BIN", "/nonexistent/mur_agent_nope") };
        let err = direct_respawn("test-agent", &agent_home).unwrap_err();
        unsafe { std::env::remove_var("MUR_AGENT_RUNTIME_BIN") };
        let msg = format!("{err:#}");
        assert!(
            msg.contains("resolve") && msg.contains("mur_agent_nope"),
            "expected resolution error from attestation mount, got: {msg}"
        );
    }

    /// `kickstart_service` (macOS path) refuses to kick when the symlink
    /// points at a non-resolvable target. This is a structural test: the
    /// function now returns `Result<bool>` and the `Err` variant means
    /// "attestation failed — never kick."
    #[test]
    #[cfg(target_os = "macos")]
    fn kickstart_service_refuses_unresolvable_symlink() {
        // Redirect bin_dir to a tmp dir with a broken symlink.
        let tmp = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("MUR_AGENT_BIN_DIR", tmp.path()) };
        // No mur_agent_test-kick symlink → canonicalize fails → Err.
        let result = kickstart_service("test-kick");
        unsafe { std::env::remove_var("MUR_AGENT_BIN_DIR") };
        match result {
            Err(e) => {
                let msg = e.to_string();
                assert!(
                    msg.contains("resolve") || msg.contains("attestation"),
                    "expected attestation error, got: {msg}"
                );
            }
            Ok(false) => {} // No symlink, no service unit — the kick command just
            // wasn't found. On macOS without a loaded unit, this is
            // expected (the key assertion is that we didn't panic).
            Ok(true) => panic!("kick should not succeed without a unit loaded"),
        }
    }

    #[test]
    fn a_restart_that_lands_on_the_same_binary_is_not_a_success() {
        // The exact shape of the reported bug: --stale picked the agent because
        // on-disk moved to `new`, but the respawn relaunched `old`.
        assert!(restart_changed_nothing("old", "new", "old"));
    }

    #[test]
    fn restarting_an_already_current_agent_is_still_a_success() {
        // Negative control. Without this, "landed == old" alone would call every
        // ordinary restart of an up-to-date agent a failure.
        assert!(!restart_changed_nothing("same", "same", "same"));
    }

    #[test]
    fn a_restart_that_moved_to_the_new_binary_is_a_success() {
        assert!(!restart_changed_nothing("old", "new", "new"));
    }

    #[test]
    fn unknown_shas_never_manufacture_a_failure() {
        // We cannot tell, so we do not accuse.
        assert!(!restart_changed_nothing("unknown", "new", "unknown"));
        assert!(!restart_changed_nothing("old", "unknown", "old"));
        assert!(!restart_changed_nothing("", "new", ""));
    }
}

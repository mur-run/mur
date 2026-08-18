//! `mur agent start` — start a stopped agent.
//!
//! Three paths, in order:
//! 1. already running (live `running.lock` pid) → no-op;
//! 2. a service unit exists (launchd plist / systemd --user unit) → start it
//!    through the service manager so supervision stays with launchd/systemd;
//! 3. no unit (Hub-managed or ad-hoc agents) → spawn the per-agent runtime
//!    symlink detached, with stdout/stderr appended to the agent's logs.
//!    This is the escape hatch when the Hub isn't around to respawn its
//!    sidecar — the process runs UNSUPERVISED (no auto-restart).

use std::fs;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use mur_common::LockFile;

use super::attest::verify_runtime_at;
use super::{pid_alive, resolve_bin_dir, resolve_mur_home};

pub fn cmd_start(name: &str) -> Result<()> {
    let mur_home = resolve_mur_home()?;
    let agent_home = mur_home.join("agents").join(name);
    if !agent_home.exists() {
        bail!("agent '{name}' not found under {}", mur_home.display());
    }

    // Best-effort program-deps preflight — informational only, never blocks
    // the start. A load/aggregate error is swallowed.
    let _ = (|| -> Result<()> {
        let deps = crate::cmd::deps::aggregate_agent(&mur_home, name)?;
        let report = crate::cmd::deps::doctor::build_report(&deps, &mur_home);
        if crate::cmd::deps::doctor::missing_count(&report) > 0 {
            eprintln!(
                "warning: agent '{name}' has missing program dependencies — run `mur agent doctor {name}` for details or `mur agent install-deps {name}` to install them."
            );
        }
        Ok(())
    })();

    // 1. Already running?
    let lock_path = agent_home.join("running.lock");
    if let Ok(bytes) = fs::read(&lock_path)
        && let Ok(lock) = serde_json::from_slice::<LockFile>(&bytes)
        && pid_alive(lock.pid)
    {
        println!("agent '{name}' is already running (pid {})", lock.pid);
        return Ok(());
    }

    // 2. Service manager owns it?
    #[cfg(target_os = "macos")]
    {
        let plist = dirs::home_dir()
            .context("no home dir")?
            .join(format!("Library/LaunchAgents/run.mur.agent.{name}.plist"));
        if plist.exists() {
            verify_runtime_at(&resolve_bin_dir()?.join(format!("mur_agent_{name}")))?;
            let label = format!("run.mur.agent.{name}");
            let uid = unsafe { libc::getuid() };
            let kick = Command::new("launchctl")
                .args(["kickstart", &format!("gui/{uid}/{label}")])
                .output()?;
            if kick.status.success() {
                println!("started '{name}' via launchd ({label})");
                return confirm_startup(name, &agent_home, &service_log_hint(name));
            }
            // Not loaded (e.g. after a manual bootout) — load brings it up
            // by itself thanks to RunAtLoad.
            let load = Command::new("launchctl")
                .args(["load", "-w"])
                .arg(&plist)
                .output()?;
            if load.status.success() {
                println!("loaded + started '{name}' via launchd ({label})");
                return confirm_startup(name, &agent_home, &service_log_hint(name));
            }
            bail!(
                "launchd refused to start '{name}': kickstart: {} / load: {}",
                String::from_utf8_lossy(&kick.stderr).trim(),
                String::from_utf8_lossy(&load.stderr).trim()
            );
        }
    }
    #[cfg(target_os = "linux")]
    {
        let unit = dirs::config_dir()
            .context("no config dir")?
            .join(format!("systemd/user/mur-agent-{name}.service"));
        if unit.exists() {
            verify_runtime_at(&resolve_bin_dir()?.join(format!("mur_agent_{name}")))?;
            let out = Command::new("systemctl")
                .args(["--user", "start", &format!("mur-agent-{name}.service")])
                .output()?;
            if !out.status.success() {
                bail!(
                    "systemd refused to start '{name}': {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                );
            }
            println!("started '{name}' via systemd --user");
            return confirm_startup(name, &agent_home, &service_log_hint(name));
        }
    }

    // 3. No unit — detached spawn of the per-agent symlink.
    let symlink = resolve_bin_dir()?.join(format!("mur_agent_{name}"));
    if !symlink.exists() {
        bail!(
            "no service unit and no runtime symlink at {} — create it with `mur agent create` or install a service with `mur agent install-service {name}`",
            symlink.display()
        );
    }
    verify_runtime_at(&symlink)?;
    let stdout = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(agent_home.join("stdout.log"))?;
    let stderr = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(agent_home.join("stderr.log"))?;
    let mut cmd = Command::new(&symlink);
    cmd.arg("start")
        .stdin(std::process::Stdio::null())
        .stdout(stdout)
        .stderr(stderr);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0); // detach from our session so it survives us
    }
    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawn {}", symlink.display()))?;
    let pid = child.id();
    // Don't report success off a successful fork: a runtime that dies one
    // second in (failed MCP admission, attestation, sandbox fail-closed)
    // otherwise still printed "started" and the only evidence landed in a log
    // nobody is told about. The lock is the runtime's own liveness claim; an
    // exit inside the window is a verdict and surfaces the log tail.
    let stderr_log = agent_home.join("stderr.log");
    let deadline = Instant::now() + CONFIRM_WINDOW;
    loop {
        if let Some(lock_pid) = live_lock_pid(&agent_home) {
            println!(
                "started '{name}' (pid {lock_pid}) — UNSUPERVISED: no auto-restart on crash or login.\nFor a persistent service run: mur agent install-service {name}"
            );
            return Ok(());
        }
        if let Ok(Some(status)) = child.try_wait() {
            bail!(
                "agent '{name}' exited during startup ({status}).\n--- last lines of {} ---\n{}",
                stderr_log.display(),
                tail_of(&stderr_log, 4096)
            );
        }
        if Instant::now() >= deadline {
            println!(
                "launching '{name}' (pid {pid}) — not confirmed within {}s; slow starts are normal.\nCheck `mur agent status {name}` or `tail {}`.\nUNSUPERVISED: no auto-restart on crash or login. For a persistent service run: mur agent install-service {name}",
                CONFIRM_WINDOW.as_secs(),
                stderr_log.display()
            );
            return Ok(());
        }
        std::thread::sleep(CONFIRM_POLL);
    }
}

/// How long `mur agent start` waits for the runtime to write a live
/// `running.lock`. Long enough to catch an immediate crash, short enough not
/// to stall the CLI: heavy starts (local model load) legitimately take 60s+
/// (the install-service startup window), so elapsing this window means
/// "not confirmed yet" — never "failed".
const CONFIRM_WINDOW: Duration = Duration::from_secs(8);
const CONFIRM_POLL: Duration = Duration::from_millis(250);

/// The pid from a live `running.lock`, if the runtime has written one and
/// that pid is still alive. Same check as cmd_start's "already running" leg.
fn live_lock_pid(agent_home: &std::path::Path) -> Option<u32> {
    let bytes = fs::read(agent_home.join("running.lock")).ok()?;
    let lock = serde_json::from_slice::<LockFile>(&bytes).ok()?;
    pid_alive(lock.pid).then_some(lock.pid)
}

/// Shared post-start confirmation for the service paths (no child handle to
/// watch there): wait for a live `running.lock`, and on timeout say where to
/// look instead of guessing a verdict.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn confirm_startup(name: &str, agent_home: &std::path::Path, log_hint: &str) -> Result<()> {
    let deadline = Instant::now() + CONFIRM_WINDOW;
    loop {
        if let Some(pid) = live_lock_pid(agent_home) {
            println!("✓ '{name}' is up (pid {pid})");
            return Ok(());
        }
        if Instant::now() >= deadline {
            println!(
                "ℹ start not confirmed within {}s — slow starts are normal; check `mur agent status {name}` or {log_hint}",
                CONFIRM_WINDOW.as_secs()
            );
            return Ok(());
        }
        std::thread::sleep(CONFIRM_POLL);
    }
}

#[cfg(target_os = "macos")]
fn service_log_hint(name: &str) -> String {
    format!(
        "`tail {}`",
        super::service::service_stderr_log(name).display()
    )
}

#[cfg(target_os = "linux")]
fn service_log_hint(name: &str) -> String {
    format!("`journalctl --user -u mur-agent-{name}.service`")
}

/// Last `max_bytes` of `path`, trimmed to whole lines (at most 20) — the
/// failure evidence lives at the end of an append-forever log.
fn tail_of(path: &std::path::Path, max_bytes: u64) -> String {
    let Ok(mut f) = fs::File::open(path) else {
        return "(log not readable)".into();
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let _ = f.seek(SeekFrom::Start(len.saturating_sub(max_bytes)));
    let mut raw = Vec::new();
    let _ = f.read_to_end(&mut raw);
    let text = String::from_utf8_lossy(&raw);
    let lines: Vec<&str> = text.lines().collect();
    let keep = lines.len().saturating_sub(20);
    lines[keep..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::tail_of;

    #[test]
    fn tail_of_returns_last_lines_only() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("stderr.log");
        let body: String = (1..=30).map(|i| format!("line {i}\n")).collect();
        std::fs::write(&p, body).unwrap();
        let tail = tail_of(&p, 4096);
        assert!(tail.ends_with("line 30"));
        assert!(!tail.contains("line 1\n"), "must trim to the last 20 lines");
        assert_eq!(tail.lines().count(), 20);
    }

    #[test]
    fn tail_of_missing_log_says_so() {
        assert_eq!(
            tail_of(std::path::Path::new("/nonexistent/stderr.log"), 4096),
            "(log not readable)"
        );
    }
}

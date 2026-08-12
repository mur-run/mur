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
use std::process::Command;

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
                return Ok(());
            }
            // Not loaded (e.g. after a manual bootout) — load brings it up
            // by itself thanks to RunAtLoad.
            let load = Command::new("launchctl")
                .args(["load", "-w"])
                .arg(&plist)
                .output()?;
            if load.status.success() {
                println!("loaded + started '{name}' via launchd ({label})");
                return Ok(());
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
            return Ok(());
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
    let child = cmd
        .spawn()
        .with_context(|| format!("spawn {}", symlink.display()))?;
    println!(
        "started '{name}' (pid {}) — UNSUPERVISED: no auto-restart on crash or login.\nFor a persistent service run: mur agent install-service {name}",
        child.id()
    );
    Ok(())
}

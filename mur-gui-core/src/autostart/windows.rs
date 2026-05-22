//! Windows autostart via HKCU Run registry key (spec §3.1).
//!
//! Registry path: `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`
//! Key name: `run.mur.agent.<slug>`

use anyhow::{Context, Result};
use std::path::Path;
use std::process::Command;

const REG_RUN: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

pub fn register(
    slug: &str,
    _display_name: &str,
    runtime_binary: &Path,
    _mur_home: &Path,
) -> Result<()> {
    let key = format!("run.mur.agent.{slug}");
    let cmd = format!(r#""{}" --profile {} start"#, runtime_binary.display(), slug);
    let status = Command::new("reg")
        .args(["add", REG_RUN, "/v", &key, "/t", "REG_SZ", "/d", &cmd, "/f"])
        .status()
        .context("reg add")?;
    if !status.success() {
        anyhow::bail!("reg add failed for {key} (exit {:?})", status.code());
    }
    Ok(())
}

pub fn unregister(slug: &str, _mur_home: &Path) -> Result<()> {
    let key = format!("run.mur.agent.{slug}");
    let _ = Command::new("reg")
        .args(["delete", REG_RUN, "/v", &key, "/f"])
        .status(); // non-fatal if key doesn't exist
    Ok(())
}

pub fn start_service(slug: &str) -> Result<()> {
    // On Windows we start the process directly; the Run key ensures autostart.
    // We don't track the PID here — the process is self-managing.
    let status = Command::new("cmd")
        .args(["/C", "start", "", &format!("run.mur.agent.{slug}")])
        .status()
        .context("cmd start")?;
    if !status.success() {
        anyhow::bail!(
            "failed to start run.mur.agent.{slug} (exit {:?})",
            status.code()
        );
    }
    Ok(())
}

pub fn stop_service(slug: &str) -> Result<()> {
    // Kill any running mur-agent-runtime processes for this slug.
    let _ = Command::new("taskkill")
        .args([
            "/F",
            "/IM",
            "mur-agent-runtime.exe",
            "/FI",
            &format!("WINDOWTITLE eq run.mur.agent.{slug}"),
        ])
        .status(); // non-fatal
    Ok(())
}

pub fn is_running(slug: &str) -> bool {
    let out = Command::new("tasklist")
        .args(["/FI", "IMAGENAME eq mur-agent-runtime.exe", "/NH"])
        .output();
    match out {
        Ok(o) => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            stdout.contains("mur-agent-runtime.exe")
                && stdout.contains(&format!("run.mur.agent.{slug}"))
        }
        Err(_) => false,
    }
}

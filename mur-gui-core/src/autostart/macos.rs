//! macOS autostart via launchd LaunchAgents (spec §3.1, §3.5).
//!
//! Plist: `~/Library/LaunchAgents/run.mur.agent.<slug>.plist`
//! The `AssociatedBundleIdentifiers` key links the agent service to Hub so
//! Activity Monitor shows it under Hub's process tree (spec §3.5).

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn register(
    slug: &str,
    display_name: &str,
    runtime_binary: &Path,
    mur_home: &Path,
) -> Result<()> {
    let plist_path = plist_path(slug)?;
    if let Some(parent) = plist_path.parent() {
        std::fs::create_dir_all(parent).context("create LaunchAgents dir")?;
    }

    let log_dir = mur_home.join("agents").join(slug);
    std::fs::create_dir_all(&log_dir).context("create agent log dir")?;

    let stdout_log = log_dir.join("stdout.log");
    let stderr_log = log_dir.join("stderr.log");

    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>run.mur.agent.{slug}</string>
    <key>AssociatedBundleIdentifiers</key>
    <array>
        <string>run.mur.host</string>
    </array>
    <key>ProgramArguments</key>
    <array>
        <string>{runtime}</string>
        <string>--profile</string>
        <string>{slug}</string>
        <string>start</string>
    </array>
    <key>KeepAlive</key>
    <true/>
    <key>RunAtLoad</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{stdout}</string>
    <key>StandardErrorPath</key>
    <string>{stderr}</string>
</dict>
</plist>
"#,
        slug = slug,
        runtime = runtime_binary.display(),
        stdout = stdout_log.display(),
        stderr = stderr_log.display(),
    );

    std::fs::write(&plist_path, &plist).context("write launchd plist")?;

    // Load (registers + starts the service).
    let status = Command::new("launchctl")
        .args(["load", &plist_path.to_string_lossy()])
        .status()
        .context("launchctl load")?;
    if !status.success() {
        anyhow::bail!(
            "launchctl load failed for run.mur.agent.{slug} (exit {:?})",
            status.code()
        );
    }

    let _ = display_name; // stored in plist label, not needed in body
    Ok(())
}

pub fn unregister(slug: &str, _mur_home: &Path) -> Result<()> {
    let plist_path = plist_path(slug)?;
    if plist_path.exists() {
        let _ = Command::new("launchctl")
            .args(["unload", &plist_path.to_string_lossy()])
            .status(); // non-fatal if service isn't loaded
        std::fs::remove_file(&plist_path).context("remove launchd plist")?;
    }
    Ok(())
}

pub fn start_service(slug: &str) -> Result<()> {
    let uid = unsafe { libc::getuid() };
    let label = format!("run.mur.agent.{slug}");
    let target = format!("user/{uid}/{label}");
    let status = Command::new("launchctl")
        .args(["kickstart", "-k", &target])
        .status()
        .context("launchctl kickstart")?;
    if !status.success() {
        anyhow::bail!(
            "launchctl kickstart failed for {label} (exit {:?})",
            status.code()
        );
    }
    Ok(())
}

pub fn stop_service(slug: &str) -> Result<()> {
    let uid = unsafe { libc::getuid() };
    let label = format!("run.mur.agent.{slug}");
    let target = format!("user/{uid}/{label}");
    // kill TERM → launchd will restart if KeepAlive=true, but stop_service
    // is called before unregister which does unload (true stop).
    let _ = Command::new("launchctl")
        .args(["kill", "TERM", &target])
        .status(); // non-fatal if not running
    Ok(())
}

pub fn is_running(slug: &str) -> bool {
    let label = format!("run.mur.agent.{slug}");
    let Ok(out) = Command::new("launchctl").args(["list", &label]).output() else {
        return false;
    };
    if !out.status.success() {
        return false;
    }
    // launchctl list prints a JSON-ish blob; "PID" key is present only when running.
    String::from_utf8_lossy(&out.stdout).contains("\"PID\"")
}

fn plist_path(slug: &str) -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME not set")?;
    Ok(home
        .join("Library")
        .join("LaunchAgents")
        .join(format!("run.mur.agent.{slug}.plist")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_contains_associated_bundle_ids() {
        let runtime = Path::new("/usr/local/bin/mur-agent-runtime");
        let mur_home = Path::new("/tmp/mur");
        let log_dir = mur_home.join("agents").join("coach");
        let stdout_log = log_dir.join("stdout.log");
        let stderr_log = log_dir.join("stderr.log");

        let plist = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>run.mur.agent.{slug}</string>
    <key>AssociatedBundleIdentifiers</key>
    <array>
        <string>run.mur.host</string>
    </array>
    <key>ProgramArguments</key>
    <array>
        <string>{runtime}</string>
        <string>--profile</string>
        <string>{slug}</string>
        <string>start</string>
    </array>
    <key>KeepAlive</key>
    <true/>
    <key>RunAtLoad</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{stdout}</string>
    <key>StandardErrorPath</key>
    <string>{stderr}</string>
</dict>
</plist>
"#,
            slug = "coach",
            runtime = runtime.display(),
            stdout = stdout_log.display(),
            stderr = stderr_log.display(),
        );

        assert!(plist.contains("AssociatedBundleIdentifiers"));
        assert!(plist.contains("run.mur.host"));
        assert!(plist.contains("run.mur.agent.coach"));
        assert!(plist.contains("KeepAlive"));
        assert!(plist.contains("RunAtLoad"));
        assert!(plist.contains("--profile"));
        assert!(plist.contains("stdout.log"));
    }
}

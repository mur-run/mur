//! macOS autostart via launchd LaunchAgents (spec §3.1, §3.5).
//!
//! Plist: `~/Library/LaunchAgents/run.mur.agent.<slug>.plist`
//! The `AssociatedBundleIdentifiers` key links the agent service to Hub so
//! Activity Monitor shows it under Hub's process tree (spec §3.5).

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Build the launchd plist contents for an agent. Pure (no I/O) so it is unit-testable.
/// Sets `MUR_HOME` in the agent's environment so the launchd-spawned runtime resolves
/// the same data directory the Hub used to register it (not just the default ~/.mur).
fn plist_contents(
    slug: &str,
    runtime_binary: &Path,
    mur_home: &Path,
    stdout_log: &Path,
    stderr_log: &Path,
) -> String {
    format!(
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
    <key>EnvironmentVariables</key>
    <dict>
        <key>MUR_HOME</key>
        <string>{mur_home}</string>
    </dict>
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
        mur_home = mur_home.display(),
        stdout = stdout_log.display(),
        stderr = stderr_log.display(),
    )
}

/// launchctl domain for the current GUI (Aqua) login session.
fn gui_domain(uid: u32) -> String {
    format!("gui/{uid}")
}

/// launchctl service target in the GUI (Aqua) domain. The Hub is a GUI app, so its
/// per-agent LaunchAgents are bootstrapped into `gui/$UID`. The previous code targeted
/// `user/$UID`, which made `kickstart`/`kill` fail with "Could not find service … in
/// domain for uid" (exit 113) even though the plist had been loaded.
fn service_target(uid: u32, slug: &str) -> String {
    format!("gui/{uid}/run.mur.agent.{slug}")
}

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

    let _ = display_name; // stored in the plist label, not needed in the body
    let plist = plist_contents(slug, runtime_binary, mur_home, &stdout_log, &stderr_log);

    std::fs::write(&plist_path, &plist).context("write launchd plist")?;

    // Bootstrap into the GUI (Aqua) domain (RunAtLoad starts it). Clear any prior
    // registration first — both the modern domain target and a legacy `launchctl
    // load` — so re-register is idempotent and migrates agents that were previously
    // loaded the old way. Both clears are best-effort.
    let uid = unsafe { libc::getuid() };
    let domain = gui_domain(uid);
    let target = service_target(uid, slug);
    let plist_str = plist_path.to_string_lossy().to_string();
    let _ = Command::new("launchctl").args(["bootout", &target]).status();
    let _ = Command::new("launchctl")
        .args(["unload", &plist_str])
        .status();
    let status = Command::new("launchctl")
        .args(["bootstrap", &domain, &plist_str])
        .status()
        .context("launchctl bootstrap")?;
    if !status.success() {
        anyhow::bail!(
            "launchctl bootstrap failed for {target} (exit {:?})",
            status.code()
        );
    }
    Ok(())
}

pub fn unregister(slug: &str, _mur_home: &Path) -> Result<()> {
    let uid = unsafe { libc::getuid() };
    let target = service_target(uid, slug);
    // Modern bootout (gui domain); fall back to legacy unload. Both best-effort.
    let _ = Command::new("launchctl").args(["bootout", &target]).status();
    let plist_path = plist_path(slug)?;
    if plist_path.exists() {
        let _ = Command::new("launchctl")
            .args(["unload", &plist_path.to_string_lossy()])
            .status();
        std::fs::remove_file(&plist_path).context("remove launchd plist")?;
    }
    Ok(())
}

pub fn start_service(slug: &str) -> Result<()> {
    let uid = unsafe { libc::getuid() };
    let target = service_target(uid, slug);
    let status = Command::new("launchctl")
        .args(["kickstart", "-k", &target])
        .status()
        .context("launchctl kickstart")?;
    if !status.success() {
        anyhow::bail!(
            "launchctl kickstart failed for {target} (exit {:?})",
            status.code()
        );
    }
    Ok(())
}

pub fn stop_service(slug: &str) -> Result<()> {
    let uid = unsafe { libc::getuid() };
    let target = service_target(uid, slug);
    // kill TERM → launchd will restart if KeepAlive=true, but stop_service
    // is called before unregister which boots it out (true stop).
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

        let plist = plist_contents("coach", runtime, mur_home, &stdout_log, &stderr_log);

        assert!(plist.contains("AssociatedBundleIdentifiers"));
        assert!(plist.contains("run.mur.host"));
        assert!(plist.contains("run.mur.agent.coach"));
        assert!(plist.contains("KeepAlive"));
        assert!(plist.contains("RunAtLoad"));
        assert!(plist.contains("--profile"));
        assert!(plist.contains("stdout.log"));
    }

    #[test]
    fn plist_sets_mur_home_env() {
        // The launchd-spawned runtime must inherit MUR_HOME so it resolves the
        // same data directory the Hub registered it with.
        let plist = plist_contents(
            "coach",
            Path::new("/usr/local/bin/mur-agent-runtime"),
            Path::new("/tmp/custom-mur-home"),
            Path::new("/tmp/custom-mur-home/agents/coach/stdout.log"),
            Path::new("/tmp/custom-mur-home/agents/coach/stderr.log"),
        );
        assert!(plist.contains("<key>EnvironmentVariables</key>"));
        assert!(plist.contains("<key>MUR_HOME</key>"));
        assert!(plist.contains("<string>/tmp/custom-mur-home</string>"));
    }

    #[test]
    fn service_target_uses_gui_domain() {
        // Regression: kickstart/kill/bootout must target gui/$UID, not user/$UID,
        // or launchctl can't find the loaded LaunchAgent (exit 113).
        assert_eq!(gui_domain(501), "gui/501");
        assert_eq!(service_target(501, "coach"), "gui/501/run.mur.agent.coach");
    }
}

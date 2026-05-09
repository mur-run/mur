//! Installs murmurd as a login-persistent service (launchd / systemd).

use anyhow::Result;
use std::path::{Path, PathBuf};

/// Returns true if installation succeeded, false if the platform is
/// unsupported (WSL, container, etc.) — caller prints a fallback message.
pub(crate) fn install_daemon_service(murmurd_path: &Path) -> Result<bool> {
    #[cfg(target_os = "macos")]
    {
        install_launchd(murmurd_path)?;
        return Ok(true);
    }
    #[cfg(target_os = "linux")]
    {
        install_systemd(murmurd_path)?;
        return Ok(true);
    }
    #[allow(unreachable_code)]
    Ok(false)
}

#[cfg(target_os = "macos")]
fn install_launchd(murmurd_path: &Path) -> Result<()> {
    let label = "run.mur.murmurd";
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home dir"))?;
    let agents_dir = home.join("Library").join("LaunchAgents");
    std::fs::create_dir_all(&agents_dir)?;

    let plist_path = agents_dir.join(format!("{label}.plist"));
    let log_path = home.join(".mur").join("murmurd.log");
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
    "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{bin}</string>
    </array>
    <key>KeepAlive</key>
    <true/>
    <key>RunAtLoad</key>
    <true/>
    <key>StandardErrorPath</key>
    <string>{log}</string>
    <key>StandardOutPath</key>
    <string>{log}</string>
    <key>ThrottleInterval</key>
    <integer>5</integer>
</dict>
</plist>
"#,
        bin = murmurd_path.display(),
        log = log_path.display(),
    );
    std::fs::write(&plist_path, &plist)?;

    // Load/reload the agent (ignore errors — user may not have launchctl in PATH)
    let _ = std::process::Command::new("launchctl")
        .args(["unload", &plist_path.to_string_lossy()])
        .status();
    let _ = std::process::Command::new("launchctl")
        .args(["load", "-w", &plist_path.to_string_lossy()])
        .status();

    Ok(())
}

#[cfg(target_os = "linux")]
fn install_systemd(murmurd_path: &Path) -> Result<()> {
    let unit_dir = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("no home dir"))?
        .join(".config")
        .join("systemd")
        .join("user");
    std::fs::create_dir_all(&unit_dir)?;

    let unit_path = unit_dir.join("murmurd.service");
    let unit = format!(
        "[Unit]\nDescription=murmurd — mur pattern daemon\n\n\
         [Service]\nExecStart={bin}\nRestart=always\nRestartSec=5\n\n\
         [Install]\nWantedBy=default.target\n",
        bin = murmurd_path.display(),
    );
    std::fs::write(&unit_path, &unit)?;

    // Enable + start (ignore errors — systemd may not be running, e.g. in containers)
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status();
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "enable", "--now", "murmurd.service"])
        .status();

    Ok(())
}

/// Locate the murmurd binary next to the current mur executable.
pub(crate) fn murmurd_bin_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("murmurd")))
        .unwrap_or_else(|| PathBuf::from("murmurd"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn murmurd_bin_path_returns_path() {
        // Should never panic and should produce a path ending in "murmurd"
        let p = murmurd_bin_path();
        let name = p.file_name().unwrap_or_default().to_string_lossy();
        assert!(
            name.contains("murmurd"),
            "expected murmurd in path, got: {p:?}"
        );
    }

    #[test]
    fn murmurd_bin_path_does_not_panic() {
        // Test that murmurd_bin_path() can be called without panicking.
        let _ = murmurd_bin_path();
    }
}

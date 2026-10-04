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
    let log_path = mur_common::home::mur_home_or_err()?.join("murmurd.log");
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
    <integer>5</integer>{env}
</dict>
</plist>
"#,
        bin = murmurd_path.display(),
        log = log_path.display(),
        env = launchd_env_block(mur_common::home::env_override().as_deref()),
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
         [Service]\nExecStart={bin}\n{env}Restart=always\nRestartSec=5\n\n\
         [Install]\nWantedBy=default.target\n",
        bin = murmurd_path.display(),
        env = systemd_env_line(mur_common::home::env_override().as_deref()),
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

/// `EnvironmentVariables` entry carrying `MUR_HOME` into the launchd job, or
/// nothing when it is unset. launchd does not inherit the installing shell's
/// environment, so without this the daemon reads `~/.mur` while the CLI
/// writes `$MUR_HOME` (#1696).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn launchd_env_block(mur_home: Option<&Path>) -> String {
    let Some(h) = mur_home else {
        return String::new();
    };
    let v = h
        .display()
        .to_string()
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    format!(
        "\n    <key>EnvironmentVariables</key>\n    <dict>\n        <key>{key}</key>\n        <string>{v}</string>\n    </dict>",
        key = mur_common::home::MUR_HOME_ENV,
    )
}

/// systemd `Environment=` line carrying `MUR_HOME`, or empty when unset.
/// Same reason as [`launchd_env_block`].
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn systemd_env_line(mur_home: Option<&Path>) -> String {
    let Some(h) = mur_home else {
        return String::new();
    };
    let v = h
        .display()
        .to_string()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    format!(
        "Environment=\"{key}={v}\"\n",
        key = mur_common::home::MUR_HOME_ENV
    )
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
    fn service_units_carry_mur_home_only_when_set() {
        assert_eq!(launchd_env_block(None), "");
        assert_eq!(systemd_env_line(None), "");
        let b = launchd_env_block(Some(Path::new("/data/a&b")));
        assert!(b.contains("<key>MUR_HOME</key>"), "{b}");
        assert!(b.contains("<string>/data/a&amp;b</string>"), "{b}");
        assert_eq!(
            systemd_env_line(Some(Path::new("/data/my mur"))),
            "Environment=\"MUR_HOME=/data/my mur\"\n"
        );
    }

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

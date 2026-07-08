//! `mur agent install-service` — generate launchd / systemd unit files.

use std::fs;
use std::path::Path;

use anyhow::{Result, anyhow, bail};

use super::{resolve_bin_dir, resolve_mur_home, write_atomic};

pub fn cmd_install_service(name: &str, dry_run: bool) -> Result<()> {
    // Confirm the agent exists so we fail fast on typos.
    let mur_home = resolve_mur_home()?;
    if !mur_home.join("agents").join(name).exists() {
        bail!("agent '{name}' not found under {}", mur_home.display());
    }
    let bin_dir = resolve_bin_dir()?;
    let symlink = bin_dir.join(format!("mur_agent_{name}"));

    #[cfg(target_os = "macos")]
    {
        let plist = darwin_plist(name, &symlink, &derive_service_path());
        if dry_run {
            print!("{plist}");
            return Ok(());
        }
        let dest = dirs::home_dir()
            .ok_or_else(|| anyhow!("no home dir"))?
            .join(format!("Library/LaunchAgents/run.mur.agent.{name}.plist"));
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        write_atomic(&dest, plist.as_bytes())?;
        let load_output = std::process::Command::new("launchctl")
            .args(["load", "-w"])
            .arg(&dest)
            .output()?;
        if !load_output.status.success() {
            let stderr = String::from_utf8_lossy(&load_output.stderr);
            bail!(
                "wrote plist to {} but `launchctl load -w` failed: {stderr}; the plist is still on disk, try `launchctl bootstrap gui/$UID {}` to load it manually",
                dest.display(),
                dest.display()
            );
        }
        println!("Installed launchd service at {}", dest.display());
    }
    #[cfg(target_os = "linux")]
    {
        let unit = linux_unit(name, &symlink, &derive_service_path());
        if dry_run {
            print!("{unit}");
            return Ok(());
        }
        let dest = dirs::config_dir()
            .ok_or_else(|| anyhow!("no config dir"))?
            .join(format!("systemd/user/mur-agent-{name}.service"));
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        write_atomic(&dest, unit.as_bytes())?;
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .status();
        let enable_output = std::process::Command::new("systemctl")
            .args(["--user", "enable", "--now"])
            .arg(format!("mur-agent-{name}.service"))
            .output()?;
        if !enable_output.status.success() {
            let stderr = String::from_utf8_lossy(&enable_output.stderr);
            bail!(
                "wrote unit to {} but `systemctl --user enable --now` failed: {stderr}; the unit file is still on disk, try `systemctl --user enable --now mur-agent-{name}.service` to retry manually",
                dest.display()
            );
        }
        println!("Installed systemd --user unit at {}", dest.display());
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (name, dry_run, symlink);
        bail!("install-service only supports macOS (launchd) and Linux (systemd --user)")
    }
    Ok(())
}

/// Directories launchd hands every process by default (its minimal PATH),
/// used as the tail of the derived service PATH.
const LAUNCHD_DEFAULT_PATH_DIRS: &[&str] = &["/usr/bin", "/bin", "/usr/sbin", "/sbin"];

/// Derive a PATH string for the installed service so PATH-installed MCP
/// binaries (npm/homebrew) resolve when launchd/systemd starts the agent
/// with its minimal default PATH.
///
/// Includes, in order: `<npm global prefix>/bin` (best-effort, skipped
/// gracefully if npm is absent or fails), `/opt/homebrew/bin`,
/// `/usr/local/bin`, then the launchd default dirs.
fn derive_service_path() -> String {
    let mut dirs: Vec<String> = Vec::new();

    if let Ok(output) = std::process::Command::new("npm")
        .args(["config", "get", "prefix"])
        .output()
    {
        if output.status.success() {
            let prefix = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !prefix.is_empty() {
                dirs.push(format!("{prefix}/bin"));
            }
        }
    }

    dirs.push("/opt/homebrew/bin".to_string());
    dirs.push("/usr/local/bin".to_string());
    dirs.extend(LAUNCHD_DEFAULT_PATH_DIRS.iter().map(|d| d.to_string()));

    dirs.join(":")
}

#[cfg(target_os = "macos")]
fn darwin_plist(name: &str, symlink: &Path, service_path: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>run.mur.agent.{name}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{sym}</string>
        <string>start</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>EnvironmentVariables</key>
    <dict>
        <key>PATH</key>
        <string>{path}</string>
    </dict>
    <key>StandardErrorPath</key>
    <string>/tmp/mur-agent-{name}.err.log</string>
    <key>StandardOutPath</key>
    <string>/tmp/mur-agent-{name}.out.log</string>
</dict>
</plist>
"#,
        sym = symlink.display(),
        path = service_path,
    )
}

#[cfg(target_os = "linux")]
fn linux_unit(name: &str, symlink: &Path, service_path: &str) -> String {
    format!(
        r#"[Unit]
Description=murmur agent {name}
After=default.target

[Service]
Type=simple
Environment=PATH={path}
ExecStart={sym} start
Restart=on-failure
RestartSec=3

[Install]
WantedBy=default.target
"#,
        sym = symlink.display(),
        path = service_path,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_includes_path_env() {
        let plist = darwin_plist(
            "aura",
            Path::new("/path/to/mur_agent_aura"),
            "/opt/homebrew/bin:/usr/bin:/bin",
        );
        assert!(plist.contains("<key>EnvironmentVariables</key>"));
        assert!(plist.contains("<key>PATH</key>"));
        assert!(plist.contains("/opt/homebrew/bin"));
    }

    #[test]
    fn derive_service_path_always_has_launchd_defaults() {
        let path = derive_service_path();
        for dir in LAUNCHD_DEFAULT_PATH_DIRS {
            assert!(path.contains(dir), "missing {dir} in {path}");
        }
    }
}

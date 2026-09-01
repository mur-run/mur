//! `mur agent install-service` — generate launchd / systemd unit files, and
//! the stop/remove half that keeps the supervisor in step with the CLI.
//!
//! Everything that locates a descriptor goes through [`service_file_in`]. When
//! install wrote one path and stop looked at another, stop silently did
//! nothing and still reported success — so the path has exactly one source.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow, bail};

use super::{resolve_bin_dir, resolve_mur_home, write_atomic};

/// The service descriptor's path for `name`, under `base`.
///
/// `base` is the home dir on macOS (`~/Library/LaunchAgents/...`) and the
/// config dir on Linux (`$XDG_CONFIG_HOME/systemd/user/...`), matching what
/// [`cmd_install_service`] writes.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(super) fn service_file_in(base: &Path, name: &str) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        base.join(format!("Library/LaunchAgents/run.mur.agent.{name}.plist"))
    }
    #[cfg(target_os = "linux")]
    {
        base.join(format!("systemd/user/mur-agent-{name}.service"))
    }
}

/// The installed service descriptor for `name`, if this platform has services
/// and one is actually on disk.
pub(super) fn installed_service(name: &str) -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    let base = dirs::home_dir()?;
    #[cfg(target_os = "linux")]
    let base = dirs::config_dir()?;
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = name;
        return None;
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let path = service_file_in(&base, name);
        path.exists().then_some(path)
    }
}

/// Tell the service manager to stop supervising `name`, so it stops respawning
/// the runtime the moment we signal it. Returns whether a service existed.
///
/// Nothing here trusts the exit status: `launchctl bootout` reports failure
/// when the job simply is not loaded, which is the state we want anyway. The
/// caller confirms the outcome by looking for a live lock afterwards.
///
/// The stop lasts until the next login, not forever: `bootout` (and
/// `systemctl --user stop`) unload the job from the CURRENT session without
/// disabling it, and the descriptor stays on disk with `RunAtLoad`, so the
/// login after this brings the agent back. That is the right default for a
/// supervised service — but callers must say so rather than let "stopped"
/// read as permanent. [`lasting_stop_hint`] is the command that does make it
/// permanent.
pub(super) fn stop_service(name: &str) -> bool {
    let Some(path) = installed_service(name) else {
        return false;
    };
    let _ = path;
    #[cfg(target_os = "macos")]
    {
        let uid = unsafe { libc::getuid() };
        let _ = std::process::Command::new("launchctl")
            .args(["bootout", &format!("gui/{uid}/run.mur.agent.{name}")])
            .output();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "stop", &format!("mur-agent-{name}.service")])
            .output();
    }
    true
}

/// Stop the service and delete its descriptor. Returns the path removed.
///
/// Without this, `mur agent remove` left the descriptor on disk and loaded:
/// the supervisor kept trying to exec the `mur_agent_<name>` symlink that
/// remove had just deleted, and reloaded it again at the next login.
pub(super) fn remove_service(name: &str) -> Option<PathBuf> {
    let path = installed_service(name)?;
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "disable", "--now"])
            .arg(format!("mur-agent-{name}.service"))
            .output();
    }
    #[cfg(not(target_os = "linux"))]
    {
        stop_service(name);
    }
    if fs::remove_file(&path).is_err() {
        return None;
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .status();
    }
    Some(path)
}

/// The command that makes a stop outlive the next login, for the message that
/// tells the user their stop does not. There is no `mur` subcommand for this
/// on purpose: disabling is a launchd/systemd-level statement about the job,
/// and `mur agent remove` (which deletes the descriptor) is the only MUR verb
/// that currently ends a service for good.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(super) fn lasting_stop_hint(name: &str) -> String {
    #[cfg(target_os = "macos")]
    {
        let uid = unsafe { libc::getuid() };
        format!("launchctl disable gui/{uid}/run.mur.agent.{name}")
    }
    #[cfg(target_os = "linux")]
    {
        format!("systemctl --user disable mur-agent-{name}.service")
    }
}

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
        let plist = darwin_plist(name, &symlink, &derive_service_path(&bin_dir));
        if dry_run {
            print!("{plist}");
            return Ok(());
        }
        let dest = service_file_in(
            &dirs::home_dir().ok_or_else(|| anyhow!("no home dir"))?,
            name,
        );
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
        let unit = linux_unit(name, &symlink, &derive_service_path(&bin_dir));
        if dry_run {
            print!("{unit}");
            return Ok(());
        }
        let dest = service_file_in(
            &dirs::config_dir().ok_or_else(|| anyhow!("no config dir"))?,
            name,
        );
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
/// Includes, in order: `bin_dir` (where MUR installed its own binaries),
/// `<npm global prefix>/bin` (best-effort, skipped gracefully if npm is
/// absent or fails), `/opt/homebrew/bin`, `/usr/local/bin`, then the launchd
/// default dirs.
///
/// `bin_dir` leads for a reason. Since #935 an installed MUR lives in
/// `~/.local/bin`, which is in the user's interactive PATH but in neither
/// launchd's nor systemd's. Without it a sibling binary the agent pins --
/// `mur-research-gateway` is the one that bit -- resolves to whatever OLD
/// copy is left in `/opt/homebrew/bin` under the service and to the current
/// one in an interactive shell. Two different binaries for the same agent
/// depending on how it was launched, so the B0 rule 6 pin verifies
/// interactively and fail-closes under launchd, crash-looping the agent.
fn derive_service_path(bin_dir: &Path) -> String {
    let mut dirs: Vec<String> = vec![bin_dir.to_string_lossy().into_owned()];

    if let Ok(output) = std::process::Command::new("npm")
        .args(["config", "get", "prefix"])
        .output()
        && output.status.success()
    {
        let prefix = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !prefix.is_empty() {
            dirs.push(format!("{prefix}/bin"));
        }
    }

    dirs.push("/opt/homebrew/bin".to_string());
    dirs.push("/usr/local/bin".to_string());
    dirs.extend(LAUNCHD_DEFAULT_PATH_DIRS.iter().map(|d| d.to_string()));

    // `bin_dir` is often one of the constants below it (a brew install).
    let mut seen = std::collections::HashSet::new();
    dirs.retain(|d| seen.insert(d.clone()));
    dirs.join(":")
}

#[cfg(target_os = "macos")]
/// Where the launchd unit sends the runtime's stderr — single source for the
/// plist below and for `mur agent start`'s "where to look" hint.
pub(crate) fn service_stderr_log(name: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(format!("/tmp/mur-agent-{name}.err.log"))
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
    <string>{err}</string>
    <key>StandardOutPath</key>
    <string>/tmp/mur-agent-{name}.out.log</string>
</dict>
</plist>
"#,
        sym = symlink.display(),
        path = service_path,
        err = service_stderr_log(name).display(),
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
    #[cfg(target_os = "macos")]
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
        let path = derive_service_path(Path::new("/Users/x/.local/bin"));
        for dir in LAUNCHD_DEFAULT_PATH_DIRS {
            assert!(path.contains(dir), "missing {dir} in {path}");
        }
    }

    /// The service must resolve sibling binaries out of the SAME directory the
    /// runtime was installed into, ahead of any older copy left on the system
    /// PATH. When it did not, a pinned `mur-research-gateway` resolved to the
    /// stale `/opt/homebrew/bin` copy under launchd while an interactive start
    /// got `~/.local/bin`, and the pin check fail-closed into a crash loop.
    #[test]
    fn install_dir_leads_the_service_path() {
        let path = derive_service_path(Path::new("/Users/x/.local/bin"));
        assert!(
            path.starts_with("/Users/x/.local/bin:"),
            "install dir must lead: {path}"
        );
        assert!(
            path.find("/Users/x/.local/bin").unwrap() < path.find("/opt/homebrew/bin").unwrap(),
            "install dir must outrank homebrew: {path}"
        );
    }

    /// A brew install has `bin_dir == /opt/homebrew/bin`; it must appear once.
    #[test]
    fn a_bin_dir_that_repeats_a_constant_is_not_duplicated() {
        let path = derive_service_path(Path::new("/opt/homebrew/bin"));
        assert_eq!(
            path.split(':')
                .filter(|d| *d == "/opt/homebrew/bin")
                .count(),
            1,
            "duplicated entry in {path}"
        );
    }

    /// The label `stop_service` boots out is derived separately from the path
    /// the descriptor lives at, so this pins the pair against the plist that
    /// `darwin_plist` actually writes. If they ever drift, stop would boot out
    /// a label nothing runs under and still report success — the exact shape of
    /// the bug this whole change exists to fix.
    #[test]
    #[cfg(target_os = "macos")]
    fn stop_boots_out_the_label_the_installed_plist_declares() {
        let path = service_file_in(Path::new("/Users/x"), "kelp");
        assert_eq!(
            path,
            Path::new("/Users/x/Library/LaunchAgents/run.mur.agent.kelp.plist")
        );

        let plist = darwin_plist("kelp", Path::new("/bin/mur_agent_kelp"), "/usr/bin");
        assert!(
            plist.contains("<string>run.mur.agent.kelp</string>"),
            "stop_service boots out gui/<uid>/run.mur.agent.kelp; the plist must declare that label"
        );
    }

    /// An agent that never had a service must not turn `remove` into an error,
    /// and must not make `stop` claim it unloaded something.
    #[test]
    fn no_installed_service_is_not_an_error() {
        assert!(!stop_service("definitely-not-an-agent-abc123"));
        assert!(remove_service("definitely-not-an-agent-abc123").is_none());
    }
}

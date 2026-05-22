//! Linux autostart via systemd --user units (spec §3.1).
//!
//! Unit: `~/.config/systemd/user/run.mur.agent.<slug>.service`

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn register(
    slug: &str,
    display_name: &str,
    runtime_binary: &Path,
    _mur_home: &Path,
) -> Result<()> {
    let unit_path = unit_path(slug)?;
    if let Some(parent) = unit_path.parent() {
        std::fs::create_dir_all(parent).context("create systemd user dir")?;
    }

    let unit = format!(
        "[Unit]\n\
         Description=MuR Agent: {display_name}\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart={runtime} --profile {slug} start\n\
         Restart=on-failure\n\
         RestartSec=5\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        display_name = display_name,
        runtime = runtime_binary.display(),
        slug = slug,
    );
    std::fs::write(&unit_path, &unit).context("write systemd unit")?;

    // Reload daemon so the new unit is visible.
    let _ = Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status();

    let label = format!("run.mur.agent.{slug}");
    let status = Command::new("systemctl")
        .args(["--user", "enable", &label])
        .status()
        .context("systemctl enable")?;
    if !status.success() {
        anyhow::bail!(
            "systemctl --user enable failed for {label} (exit {:?})",
            status.code()
        );
    }
    Ok(())
}

pub fn unregister(slug: &str, _mur_home: &Path) -> Result<()> {
    let label = format!("run.mur.agent.{slug}");
    let _ = Command::new("systemctl")
        .args(["--user", "disable", &label])
        .status();
    if let Ok(path) = unit_path(slug) {
        let _ = std::fs::remove_file(path);
    }
    Ok(())
}

pub fn start_service(slug: &str) -> Result<()> {
    let label = format!("run.mur.agent.{slug}");
    let status = Command::new("systemctl")
        .args(["--user", "start", &label])
        .status()
        .context("systemctl start")?;
    if !status.success() {
        anyhow::bail!(
            "systemctl --user start failed for {label} (exit {:?})",
            status.code()
        );
    }
    Ok(())
}

pub fn stop_service(slug: &str) -> Result<()> {
    let label = format!("run.mur.agent.{slug}");
    let _ = Command::new("systemctl")
        .args(["--user", "stop", &label])
        .status(); // non-fatal
    Ok(())
}

pub fn is_running(slug: &str) -> bool {
    let label = format!("run.mur.agent.{slug}");
    Command::new("systemctl")
        .args(["--user", "is-active", "--quiet", &label])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn unit_path(slug: &str) -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME not set")?;
    Ok(home
        .join(".config")
        .join("systemd")
        .join("user")
        .join(format!("run.mur.agent.{slug}.service")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_file_has_restart_policy() {
        let runtime = Path::new("/usr/bin/mur-agent-runtime");
        let unit = format!(
            "[Unit]\n\
             Description=MuR Agent: {display_name}\n\
             \n\
             [Service]\n\
             Type=simple\n\
             ExecStart={runtime} --profile {slug} start\n\
             Restart=on-failure\n\
             RestartSec=5\n\
             \n\
             [Install]\n\
             WantedBy=default.target\n",
            display_name = "Coach",
            runtime = runtime.display(),
            slug = "coach",
        );
        assert!(unit.contains("Restart=on-failure"));
        assert!(unit.contains("RestartSec=5"));
        assert!(unit.contains("WantedBy=default.target"));
        assert!(unit.contains("--profile coach start"));
    }
}

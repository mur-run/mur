//! Tri-state "is the murmurd job still loaded?" probes for `stop`.
//!
//! A probe that cannot tell is an error, never "not loaded": treating an
//! unknown as absent would let `stop` report success while launchd or systemd
//! still supervises murmurd and will respawn it.

use anyhow::{Context, Result, bail};

use super::{SYSTEMD_UNIT, launchd_target};

/// `launchctl print` exit status for "Could not find service" (`ESRCH`-style
/// 113). Every other non-zero status (bad domain 64, permission, IPC failure)
/// means the probe could not tell.
pub(super) const LAUNCHCTL_NO_SUCH_SERVICE: i32 = 113;

/// Ask launchd whether the murmurd job is loaded.
pub(super) fn launchd_loaded() -> Result<bool> {
    let target = launchd_target();
    let status = std::process::Command::new("launchctl")
        .args(["print", &target])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .context("could not run `launchctl print`")?;
    launchd_print_loaded(status.code())
        .with_context(|| format!("`launchctl print {target}` could not tell if murmurd is loaded"))
}

/// Classify a `launchctl print` exit code. `None` is death by signal.
pub(super) fn launchd_print_loaded(code: Option<i32>) -> Result<bool> {
    match code {
        Some(0) => Ok(true),
        Some(LAUNCHCTL_NO_SUCH_SERVICE) => Ok(false),
        Some(code) => bail!("launchctl print exited with {code}"),
        None => bail!("launchctl print was killed by a signal"),
    }
}

/// Ask systemd whether the murmurd unit is running or will be respawned.
pub(super) fn systemd_running() -> Result<bool> {
    let out = std::process::Command::new("systemctl")
        .args([
            "--user",
            "show",
            SYSTEMD_UNIT,
            "--property=LoadState,ActiveState,SubState",
        ])
        .output()
        .context("could not run `systemctl --user show`")?;
    if !out.status.success() {
        bail!(
            "`systemctl --user show {SYSTEMD_UNIT}` exited with {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    systemd_show_running(&String::from_utf8_lossy(&out.stdout))
}

/// Classify `systemctl show` output. `auto-restart` counts as running: the
/// unit is waiting out `RestartSec=` and systemd will start it again.
pub(super) fn systemd_show_running(show: &str) -> Result<bool> {
    let prop = |key: &str| {
        show.lines()
            .find_map(|l| l.strip_prefix(key)?.strip_prefix('='))
            .map(str::trim)
    };
    if prop("LoadState") == Some("not-found") {
        return Ok(false);
    }
    if prop("SubState") == Some("auto-restart") {
        return Ok(true);
    }
    match prop("ActiveState") {
        Some("active" | "activating" | "deactivating" | "reloading" | "refreshing") => Ok(true),
        Some("inactive" | "failed") => Ok(false),
        Some(other) => bail!("unrecognised systemd ActiveState `{other}`"),
        None => bail!("`systemctl show` output had no ActiveState"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launchd_probe_only_trusts_could_not_find_service_as_absent() {
        assert!(launchd_print_loaded(Some(0)).unwrap());
        assert!(!launchd_print_loaded(Some(LAUNCHCTL_NO_SUCH_SERVICE)).unwrap());
        // Bad domain, permission, IPC failure, killed by a signal: unknown.
        assert!(launchd_print_loaded(Some(64)).is_err());
        assert!(launchd_print_loaded(Some(1)).is_err());
        assert!(launchd_print_loaded(None).is_err());
    }

    #[test]
    fn systemd_probe_counts_auto_restart_as_still_supervised() {
        let show = |load: &str, active: &str, sub: &str| {
            format!("LoadState={load}\nActiveState={active}\nSubState={sub}\n")
        };
        assert!(!systemd_show_running(&show("not-found", "inactive", "dead")).unwrap());
        assert!(!systemd_show_running(&show("loaded", "inactive", "dead")).unwrap());
        assert!(!systemd_show_running(&show("loaded", "failed", "failed")).unwrap());
        assert!(systemd_show_running(&show("loaded", "active", "running")).unwrap());
        // Waiting out RestartSec= — systemd will respawn it.
        assert!(systemd_show_running(&show("loaded", "activating", "auto-restart")).unwrap());
        assert!(systemd_show_running(&show("loaded", "inactive", "auto-restart")).unwrap());
        assert!(systemd_show_running("").is_err());
    }

    #[test]
    fn systemd_probe_rejects_unknown_active_state() {
        let out = "LoadState=loaded\nActiveState=weird\nSubState=x\n";
        assert!(systemd_show_running(out).is_err());
    }
}

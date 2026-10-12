//! The one boot-time read of `<mur_home>/config.yaml` (#1796).
//!
//! Every boot-once consumer (skills / memory settings, the `remember` capture
//! mode, attribution, the model-switch config, and the `fleet_run` /
//! `git_push` registration gates) takes its values from this snapshot instead
//! of re-reading the file after the sandbox seal. A post-seal re-read can fail
//! if `config.yaml` was atomically replaced between the seal and the read
//! (the granted inode is gone), and the old `load_or_default` turned that
//! failure into silent defaults — including `capture: auto_announce` for a
//! user who had turned capture off.
//!
//! Live readers that must see edits after boot (per-turn global limits,
//! the per-call `fleet_run` authorization) are deliberately not served from
//! here.

use mur_common::config::{CaptureMode, Config};

/// File name of the global config under `<mur_home>`.
const CONFIG_FILE: &str = "config.yaml";

/// Load the boot snapshot. Call it BEFORE `sandbox::apply`.
///
/// - Absent file: the defaults (nothing was configured, nothing to protect).
/// - Present but unreadable or unparseable: the defaults with privacy and
///   security fields failed closed, and a loud warning on both the log and
///   stderr. `fleet_run` / `git_push` already default to deny; memory capture
///   is forced to `off` so a broken read can never re-enable it.
pub(crate) fn load_boot_config(mur_home: &std::path::Path) -> Config {
    let path = mur_home.join(CONFIG_FILE);
    match Config::try_load(&path) {
        Ok(cfg) => cfg,
        Err(e) => {
            let msg = format!(
                "config.yaml could not be loaded at boot ({e}); running with defaults, \
                 memory capture OFF, fleet_run and git_push denied. Fix the file and \
                 restart the agent."
            );
            tracing::error!(error = %e, "{msg}");
            eprintln!("warning: {msg}");
            fail_closed_defaults()
        }
    }
}

/// Defaults for a config that exists but could not be read.
fn fail_closed_defaults() -> Config {
    let mut cfg = Config::default();
    cfg.memory.capture = CaptureMode::Off;
    cfg
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(home: &std::path::Path, yaml: &str) {
        std::fs::write(home.join(CONFIG_FILE), yaml).unwrap();
    }

    #[test]
    fn absent_file_is_plain_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = load_boot_config(tmp.path());
        assert_eq!(cfg.memory.capture, CaptureMode::AutoAnnounce);
    }

    #[test]
    fn readable_file_is_used_as_is() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "memory:\n  capture: off\n");
        assert_eq!(
            load_boot_config(tmp.path()).memory.capture,
            CaptureMode::Off
        );
        write(tmp.path(), "memory:\n  capture: ask\n");
        assert_eq!(
            load_boot_config(tmp.path()).memory.capture,
            CaptureMode::Ask
        );
    }

    #[test]
    fn unreadable_file_fails_capture_closed() {
        let tmp = tempfile::tempdir().unwrap();
        // A directory where the file should be: present, but `read_to_string`
        // fails with something other than NotFound.
        std::fs::create_dir(tmp.path().join(CONFIG_FILE)).unwrap();
        let cfg = load_boot_config(tmp.path());
        assert_eq!(cfg.memory.capture, CaptureMode::Off);
        assert!(cfg.fleet_run.agents.is_empty());
        assert!(!cfg.git_push.enabled);
    }

    #[test]
    fn unparseable_file_fails_capture_closed() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "memory: [this is not\n");
        assert_eq!(
            load_boot_config(tmp.path()).memory.capture,
            CaptureMode::Off
        );
    }

    /// The acceptance case: capture is off, the snapshot is taken, and then
    /// `config.yaml` is replaced by something the agent can no longer read.
    /// Consumers hold the snapshot, so capture stays off.
    #[test]
    fn snapshot_survives_a_post_boot_replace() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "memory:\n  capture: off\n");
        let snapshot = load_boot_config(tmp.path());
        std::fs::remove_file(tmp.path().join(CONFIG_FILE)).unwrap();
        std::fs::create_dir(tmp.path().join(CONFIG_FILE)).unwrap();
        assert_eq!(snapshot.memory.capture, CaptureMode::Off);
    }
}

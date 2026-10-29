//! Shared fixtures for the orchestrator tests: a temp home, a fake runtime
//! boot, and a fake `Launcher`. Test-only.

use std::path::Path;

use mur_common::LockFile;
use mur_common::agent::{OverrideFlags, OverrideRecord, OverrideRejection};
use mur_common::startup_override::claim::{ClaimAs, Claimed, claim, overrides_dir};
use mur_common::startup_override::{ConfirmedBy, OverrideFile};
use serde_json::json;

use super::super::restart::{Launcher, RestartNote, StopFailed};
use super::writer::{self, LockError};
use super::{Issuer, Mode, Prepared};

pub(super) const NAME: &str = "ovr-run-test";
/// A pid that is never alive in a test: the "old" runtime.
pub(super) const OLD_PID: u32 = 3_999_999;

pub(super) fn flags() -> OverrideFlags {
    OverrideFlags {
        no_hitl: true,
        ..OverrideFlags::default()
    }
}

pub(super) fn issuer() -> Issuer {
    Issuer::Cli {
        confirmed_by: ConfirmedBy::FlagConfirm,
    }
}

pub(super) struct Home {
    pub(super) tmp: tempfile::TempDir,
}

impl Home {
    pub(super) fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("agents").join(NAME)).unwrap();
        Self { tmp }
    }
    pub(super) fn path(&self) -> &Path {
        self.tmp.path()
    }
    pub(super) fn dir(&self) -> std::path::PathBuf {
        overrides_dir(self.path())
    }
    pub(super) fn lock_path(&self) -> std::path::PathBuf {
        self.path().join("agents").join(NAME).join("running.lock")
    }
    pub(super) fn prepared(&self, mode: Mode) -> Prepared {
        Prepared {
            home: self.path().to_owned(),
            name: NAME.to_owned(),
            mode,
            flags: flags(),
            has_service: false,
            old_pid: (mode == Mode::Restart).then_some(OLD_PID),
            stop_timeout_secs: 0,
            _lock: writer::lock(&self.dir(), NAME).unwrap(),
        }
    }
    pub(super) fn audit(&self) -> String {
        std::fs::read_to_string(
            self.path()
                .join("agents")
                .join(NAME)
                .join("permissions/audit.jsonl"),
        )
        .unwrap_or_default()
    }
    pub(super) fn lock_is_free(&self) -> bool {
        !matches!(writer::lock(&self.dir(), NAME), Err(LockError::Busy))
    }
}

/// What the booting runtime does, in the order the fake performs it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Boot {
    /// Claim the file and record our nonce as applied.
    Applies,
    /// Claim it and record our nonce as rejected.
    Rejects,
    /// Claim it, record nothing.
    ConsumesSilently,
    /// Never touch the file; write a lock with no override.
    Ignores,
}

pub(super) fn base_lock(pid: u32) -> LockFile {
    serde_json::from_value(json!({
        "schema": 1, "uuid": "u", "name": NAME, "pid": pid, "ppid": 1,
        "started_at": "2026-10-28T00:00:00Z", "binary_version": "0.0.0",
        "transports": {"stdio": true, "unix_socket": null, "tcp": null},
        "card_digest": "d", "capabilities": [], "build_sha": "s", "proto_version": 1,
    }))
    .unwrap()
}

pub(super) fn boot(home: &Path, how: Boot) {
    let dir = overrides_dir(home);
    let mut lock = base_lock(std::process::id());
    if how != Boot::Ignores {
        let Claimed::Took(path) = claim(&dir, NAME, ClaimAs::Consume) else {
            panic!("fake runtime found no live file");
        };
        let file: OverrideFile = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        match how {
            Boot::Applies => {
                lock.startup_override = Some(OverrideRecord {
                    flags: file.flags(),
                    nonce: file.nonce,
                    confirmed_by: file.confirmed_by,
                });
            }
            Boot::Rejects => {
                lock.override_rejected = Some(OverrideRejection {
                    nonce: Some(file.nonce),
                    reason: "override rejected: stale".into(),
                });
            }
            _ => {}
        }
    }
    let lock_path = home.join("agents").join(NAME).join("running.lock");
    std::fs::write(lock_path, serde_json::to_vec(&lock).unwrap()).unwrap();
}

pub(super) struct Fake {
    pub(super) home: std::path::PathBuf,
    pub(super) boot: Boot,
    pub(super) stop_fails: bool,
    /// Boot during `stop` (KeepAlive respawning before stop reports).
    pub(super) boot_in_stop: bool,
    pub(super) start_fails: bool,
    pub(super) panic_in_stop: bool,
    /// Make the overrides dir read-only once booted, so withdraw fails.
    pub(super) jam_withdraw: bool,
    pub(super) calls: Vec<&'static str>,
}

impl Fake {
    pub(super) fn new(home: &Home, boot: Boot) -> Self {
        Self {
            home: home.path().to_owned(),
            boot,
            stop_fails: false,
            boot_in_stop: false,
            start_fails: false,
            panic_in_stop: false,
            jam_withdraw: false,
            calls: vec![],
        }
    }
    pub(super) fn jam(&self) {
        #[cfg(unix)]
        if self.jam_withdraw {
            use std::os::unix::fs::PermissionsExt;
            let dir = overrides_dir(&self.home);
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        }
    }
}

impl Launcher for Fake {
    fn stop(&mut self, _old: u32, _wait: std::time::Duration) -> Result<(), StopFailed> {
        self.calls.push("stop");
        assert!(!self.panic_in_stop, "injected panic in stop");
        if self.boot_in_stop {
            boot(&self.home, self.boot);
        }
        if self.stop_fails {
            return Err(StopFailed { pid: OLD_PID });
        }
        Ok(())
    }
    fn start(&mut self) -> anyhow::Result<Option<u32>> {
        self.calls.push("start");
        if self.start_fails {
            anyhow::bail!("attestation failed");
        }
        if !self.boot_in_stop {
            boot(&self.home, self.boot);
        }
        self.jam();
        Ok(Some(std::process::id()))
    }
    fn kick(&mut self, _note: &mut dyn FnMut(RestartNote)) -> anyhow::Result<Option<u32>> {
        self.calls.push("kick");
        Ok(None)
    }
    fn fresh_pid(&self, _old: Option<u32>) -> Option<u32> {
        None
    }
}

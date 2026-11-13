//! The browser state key, handed to a live-browser MCP child over a pipe
//! (#2369). See `mur_common::browser_state_key` for why it exists.
//!
//! The key is read once, before the seal (`supervisor::seal`), into the
//! pre-seal secret cache. At spawn, a matching child gets the read end of a
//! fresh pipe as an inherited fd. The key — newline-terminated, far below
//! the pipe buffer — is written and the write end closed BEFORE the spawn,
//! so this process never holds a write end while a child starts: a reader
//! has the whole key the moment it looks and stops at the newline, never
//! waiting for an EOF a stray write-end holder could withhold (macOS has no
//! `pipe2`, so `std::io::pipe` sets close-on-exec a step after creating it).
//! That same gap applies to the read end: an MCP child spawned on another
//! thread between `pipe()` and the close-on-exec step inherits it too. It
//! cannot stall startup (the write end is closed first), but it could read
//! the key, or drain it before the browser does. Accepted: such a child is
//! already inside this agent's sandbox, and the window is a few instructions.
//! Only the `mur` CLI in MUR's own install dir is ever handed the key.

use crate::sandbox::policy::SandboxPolicy;
use mur_common::agent::McpServerEntry;
use std::process::{Child, Command};

/// `spawn_sandboxed`, plus the key handover when `entry` is a live browser
/// bound to a profile. Off unix there is no seatbelt to work around.
pub(super) fn spawn(
    mut cmd: Command,
    entry: &McpServerEntry,
    policy: &SandboxPolicy,
) -> std::io::Result<Child> {
    #[cfg(unix)]
    {
        // Held across the spawn so the fd number named in the env stays ours;
        // dropped (closing the parent's read end) once the child has it.
        let _armed = unix::arm(&mut cmd, entry);
        crate::sandbox::child::spawn_sandboxed(cmd, policy)
    }
    #[cfg(not(unix))]
    {
        let _ = entry;
        cmd.env_remove(mur_common::browser_state_key::FD_ENV);
        crate::sandbox::child::spawn_sandboxed(cmd, policy)
    }
}

#[cfg(unix)]
mod unix {
    use mur_common::agent::McpServerEntry;
    use mur_common::browser_state_key::{FD_ENV, is_installed_cli, keychain_ref, wants_state_key};
    use std::path::Path;
    use std::process::Command;

    /// The read end, already loaded with the key, kept open by the parent
    /// until the child has been spawned. There is no write end left to hold.
    pub(super) struct ArmedKey {
        #[cfg_attr(not(test), allow(dead_code))]
        read: std::os::fd::OwnedFd,
    }

    /// Arm `cmd` with the key when `entry` needs it and it was cached pre-seal.
    /// `None` otherwise — the child then falls back to the Keychain, which is
    /// what an unsandboxed child (or a key that was never cached) did before.
    /// Every child loses an inherited [`FD_ENV`] first: only the fd armed
    /// here may be named, never one from whoever launched the runtime.
    pub(super) fn arm(cmd: &mut Command, entry: &McpServerEntry) -> Option<ArmedKey> {
        cmd.env_remove(FD_ENV);
        if !wants_state_key(&entry.args) {
            return None;
        }
        let install_dir = mur_common::mcp_first_party::current_install_dir();
        if !trusted(Path::new(cmd.get_program()), install_dir.as_deref()) {
            tracing::warn!(
                server = %entry.name,
                program = %Path::new(cmd.get_program()).display(),
                install_dir = ?install_dir,
                "not handing the browser state key: this live-browser entry does not run \
                 the mur CLI from MUR's install dir"
            );
            return None;
        }
        arm_with(cmd, entry, keychain_ref().resolve_preseal_cached())
    }

    /// The file the spawn execs (already resolved by the caller) must be the
    /// installed `mur` CLI. An argv shaped like a live browser is not proof.
    fn trusted(program: &Path, install_dir: Option<&Path>) -> bool {
        install_dir.is_some_and(|dir| is_installed_cli(program, dir))
    }

    /// [`arm`] with the cached value passed in, so tests need not touch the
    /// process-wide pre-seal cache.
    fn arm_with(
        cmd: &mut Command,
        entry: &McpServerEntry,
        value: Option<secrecy::SecretString>,
    ) -> Option<ArmedKey> {
        use secrecy::ExposeSecret;
        use std::io::Write;
        use std::os::fd::{AsRawFd, OwnedFd};
        let Some(value) = value else {
            tracing::warn!(
                server = %entry.name,
                "browser state key was not read before the sandbox sealed; the live \
                 browser will try the Keychain itself, which a sealed child cannot reach"
            );
            return None;
        };
        let (read, write): (OwnedFd, OwnedFd) = match std::io::pipe() {
            Ok((r, w)) => (r.into(), w.into()),
            Err(e) => {
                tracing::warn!(server = %entry.name, error = %e, "could not create the state-key pipe");
                return None;
            }
        };
        // Write and close before the spawn (see the module doc). Newline-
        // terminated so the reader stops without waiting for EOF.
        let mut w = std::fs::File::from(write);
        let wrote = w
            .write_all(value.expose_secret().as_bytes())
            .and_then(|()| w.write_all(b"\n"));
        drop(w);
        if let Err(e) = wrote {
            tracing::warn!(server = %entry.name, error = %e, "could not load the state-key pipe");
            return None;
        }
        let fd = read.as_raw_fd();
        cmd.env(FD_ENV, fd.to_string());
        // SAFETY: async-signal-safe libc calls only, on an fd this process owns
        // and keeps open across the fork (`read` lives in the returned value).
        unsafe {
            use std::os::unix::process::CommandExt;
            cmd.pre_exec(move || {
                let flags = libc::fcntl(fd, libc::F_GETFD);
                if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        Some(ArmedKey { read })
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::io::Read;

        #[test]
        fn a_non_browser_entry_is_left_alone() {
            let mut cmd = Command::new("true");
            let entry = McpServerEntry {
                name: "x".into(),
                command: "x".into(),
                args: vec!["serve".into()],
                ..Default::default()
            };
            assert!(arm(&mut cmd, &entry).is_none());
            assert!(cmd.get_envs().all(|(k, v)| k != FD_ENV || v.is_none()));
        }

        #[test]
        fn a_key_never_cached_pre_seal_arms_nothing() {
            let mut cmd = Command::new("true");
            let entry = McpServerEntry {
                name: "browser".into(),
                command: "mur".into(),
                args: ["browser", "record", "--mode", "live", "--profile", "g"]
                    .map(String::from)
                    .to_vec(),
                ..Default::default()
            };
            assert!(arm_with(&mut cmd, &entry, None).is_none());
            assert!(cmd.get_envs().all(|(k, v)| k != FD_ENV || v.is_none()));
        }

        /// The real handover: a child reads the key from the fd named in its env.
        #[test]
        fn the_child_reads_the_key_from_the_inherited_fd() {
            let entry = McpServerEntry {
                name: "browser".into(),
                command: "sh".into(),
                args: ["browser", "record", "--mode", "live", "--profile", "g"]
                    .map(String::from)
                    .to_vec(),
                ..Default::default()
            };
            let mut cmd = Command::new("sh");
            cmd.arg("-c")
                .arg(format!("cat <&${FD_ENV}"))
                .stdout(std::process::Stdio::piped());
            let value = secrecy::SecretString::from("AGE-SECRET-KEY-1TESTVALUE");
            let armed = arm_with(&mut cmd, &entry, Some(value)).expect("armed");
            let mut child = cmd.spawn().unwrap();
            drop(armed);
            let mut out = String::new();
            child
                .stdout
                .take()
                .unwrap()
                .read_to_string(&mut out)
                .unwrap();
            assert!(child.wait().unwrap().success());
            assert_eq!(out, "AGE-SECRET-KEY-1TESTVALUE\n");
        }

        /// Both ends of the contract: the fd this side arms is one the
        /// browser's own reader accepts and reads the key from. The reader is
        /// called in-process on the read end, standing in for the child.
        #[test]
        fn the_browser_reader_accepts_what_the_runtime_arms() {
            use std::os::fd::IntoRawFd;
            let entry = McpServerEntry {
                name: "browser".into(),
                command: "mur".into(),
                args: ["browser", "record", "--mode", "live", "--profile", "g"]
                    .map(String::from)
                    .to_vec(),
                ..Default::default()
            };
            let mut cmd = Command::new("true");
            let value = secrecy::SecretString::from("AGE-SECRET-KEY-1CONTRACT");
            let armed = arm_with(&mut cmd, &entry, Some(value)).expect("armed");
            let named = cmd
                .get_envs()
                .find(|(k, _)| *k == FD_ENV)
                .and_then(|(_, v)| v)
                .map(|v| v.to_string_lossy().into_owned())
                .expect("fd env set");
            assert_eq!(FD_ENV, mur_browser::state::STATE_KEY_FD_ENV);
            // Hand the read end over the way the child would hold it. The key
            // is already in the pipe and no write end remains anywhere.
            let read_fd = armed.read.into_raw_fd();
            assert_eq!(named, read_fd.to_string());
            assert_eq!(
                mur_browser::state::read_handed_fd(&named).unwrap(),
                "AGE-SECRET-KEY-1CONTRACT"
            );
        }

        /// Item 1 of the #2369 review: a matching argv run by anything but
        /// the installed `mur` CLI is never handed the key.
        #[test]
        fn only_the_installed_cli_is_trusted() {
            let dir = tempfile::tempdir().unwrap();
            let install = dir.path().canonicalize().unwrap();
            let mur = install.join("mur");
            std::fs::write(&mur, "").unwrap();
            assert!(trusted(&mur, Some(&install)));
            let addon = install.join("some-addon");
            std::fs::write(&addon, "").unwrap();
            assert!(!trusted(&addon, Some(&install)));
            // No install dir resolved: refuse rather than guess.
            assert!(!trusted(&mur, None));
        }

        #[test]
        fn an_inherited_fd_env_is_never_passed_on() {
            let mut cmd = Command::new("true");
            cmd.env(FD_ENV, "7");
            let entry = McpServerEntry {
                name: "x".into(),
                command: "x".into(),
                args: vec!["serve".into()],
                ..Default::default()
            };
            assert!(arm(&mut cmd, &entry).is_none());
            assert!(cmd.get_envs().all(|(k, v)| k != FD_ENV || v.is_none()));
        }
    }
}

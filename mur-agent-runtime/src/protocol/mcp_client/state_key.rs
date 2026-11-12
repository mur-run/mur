//! The browser state key, handed to a live-browser MCP child over a pipe
//! (#2369). See `mur_common::browser_state_key` for why it exists.
//!
//! The key is read once, before the seal (`supervisor::seal`), into the
//! pre-seal secret cache. At spawn, a matching child gets the read end of a
//! fresh pipe as an inherited fd; the key is written and the write end closed
//! right after the spawn, so the child reads it to EOF. Every other fd this
//! process opens is close-on-exec, so no other child ever holds the pipe.

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
        let key = unix::arm(&mut cmd, entry);
        let child = crate::sandbox::child::spawn_sandboxed(cmd, policy)?;
        if let Some(k) = key {
            k.deliver();
        }
        Ok(child)
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
    use mur_common::browser_state_key::{FD_ENV, keychain_ref, wants_state_key};
    use std::process::Command;

    /// The write end, kept by the parent until the child has been spawned.
    pub(super) struct PendingKey {
        write: std::os::fd::OwnedFd,
        read: std::os::fd::OwnedFd,
        value: secrecy::SecretString,
    }

    /// Arm `cmd` with the key when `entry` needs it and it was cached pre-seal.
    /// `None` otherwise — the child then falls back to the Keychain, which is
    /// what an unsandboxed child (or a key that was never cached) did before.
    /// Every child loses an inherited [`FD_ENV`] first: only the fd armed
    /// here may be named, never one from whoever launched the runtime.
    pub(super) fn arm(cmd: &mut Command, entry: &McpServerEntry) -> Option<PendingKey> {
        cmd.env_remove(FD_ENV);
        if !wants_state_key(&entry.args) {
            return None;
        }
        arm_with(cmd, entry, keychain_ref().resolve_preseal_cached())
    }

    /// [`arm`] with the cached value passed in, so tests need not touch the
    /// process-wide pre-seal cache.
    fn arm_with(
        cmd: &mut Command,
        entry: &McpServerEntry,
        value: Option<secrecy::SecretString>,
    ) -> Option<PendingKey> {
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
        Some(PendingKey { write, read, value })
    }

    impl PendingKey {
        /// After the spawn: drop our read end and write the key. The key is far
        /// below the pipe buffer, so the write never blocks on the child.
        pub(super) fn deliver(self) {
            use secrecy::ExposeSecret;
            use std::io::Write;
            drop(self.read);
            let mut w = std::fs::File::from(self.write);
            if let Err(e) = w.write_all(self.value.expose_secret().as_bytes()) {
                tracing::warn!(error = %e, "could not hand the browser state key to its MCP child");
            }
        }
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
            let pending = arm_with(&mut cmd, &entry, Some(value)).expect("armed");
            let mut child = cmd.spawn().unwrap();
            pending.deliver();
            let mut out = String::new();
            child
                .stdout
                .take()
                .unwrap()
                .read_to_string(&mut out)
                .unwrap();
            assert!(child.wait().unwrap().success());
            assert_eq!(out, "AGE-SECRET-KEY-1TESTVALUE");
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
            let pending = arm_with(&mut cmd, &entry, Some(value)).expect("armed");
            let named = cmd
                .get_envs()
                .find(|(k, _)| *k == FD_ENV)
                .and_then(|(_, v)| v)
                .map(|v| v.to_string_lossy().into_owned())
                .expect("fd env set");
            assert_eq!(FD_ENV, mur_browser::state::STATE_KEY_FD_ENV);
            // Take the read end out of `pending` the way the child would hold
            // it; `deliver` then writes the key and closes the write end.
            let PendingKey { write, read, value } = pending;
            let read_fd = read.into_raw_fd();
            assert_eq!(named, read_fd.to_string());
            PendingKey {
                write,
                read: std::fs::File::open("/dev/null").unwrap().into(),
                value,
            }
            .deliver();
            assert_eq!(
                mur_browser::state::read_handed_fd(&named).unwrap(),
                "AGE-SECRET-KEY-1CONTRACT"
            );
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

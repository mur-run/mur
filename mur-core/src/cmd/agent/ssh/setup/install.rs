//! Public-key install (SSH setup spec, "Public-key install"): append the
//! agent's dedicated public key to `authorized_keys` on one host, over a
//! connection made with the user's own keys, outside the agent's sandbox.
//!
//! Nothing the agent or the user typed reaches a remote shell: the remote
//! command is one constant, the user is an argument to the local `ssh`, and
//! the key line goes in on stdin (K6).

use std::ffi::OsString;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use mur_ssh_request::{RequestHost, SSH_BINARY, validate_user};

use super::dedicated;

/// `ssh -o ConnectTimeout`, in seconds.
pub const CONNECT_TIMEOUT_SECS: u64 = 10;
/// The whole run: connect, authenticate, one short remote command.
const RUN_TIMEOUT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(50);
/// Lines of a failed run's stderr kept for the card.
const ERR_TAIL_LINES: usize = 6;
/// What [`REMOTE_SCRIPT`] prints when it appended the line.
pub const MARK_ADDED: &str = "mur-key-added";
/// What [`REMOTE_SCRIPT`] prints when the line was already there.
pub const MARK_PRESENT: &str = "mur-key-present";
/// Stands in for the system-wide `known_hosts`: only the approved keys count.
const NULL_DEVICE: &str = "/dev/null";

/// Reads one line from stdin; appends it to `authorized_keys` unless an
/// identical line is there. Prints which. A last line with no newline gets
/// one first, so the appended key never joins the key before it.
///
/// It has no single quote and no `!`, so it survives being passed through a
/// login shell that is not `sh`.
pub const REMOTE_SCRIPT: &str = "umask 077; f=\"$HOME/.ssh/authorized_keys\"; \
mkdir -p \"$HOME/.ssh\" && touch \"$f\" && IFS= read -r k && \
if grep -qxF -- \"$k\" \"$f\"; then echo mur-key-present; \
else if [ -n \"$(tail -c 1 \"$f\")\" ]; then echo >> \"$f\"; fi; \
printf \"%s\\n\" \"$k\" >> \"$f\" && echo mur-key-added; fi";

/// The single remote argument. Byte-equal for every host and user.
pub fn remote_command() -> String {
    format!("exec sh -c '{REMOTE_SCRIPT}'")
}

pub struct InstallRequest<'a> {
    pub host: &'a RequestHost,
    pub user: &'a str,
    pub public_line: &'a str,
    /// Host keys the user approved on this card, as `known_hosts` lines.
    pub known_hosts: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Installed {
    Added,
    AlreadyPresent,
}

pub trait Installer {
    fn install(&self, r: &InstallRequest) -> Result<Installed>;
}

/// The real one: the user's own `ssh`.
pub struct SshInstaller {
    /// Where the one-run `known_hosts` file goes: the agent's key directory
    /// under `secrets/`. Not `<agent_home>/ssh/`, which the agent can write,
    /// while this connection carries the user's own keys.
    scratch: Option<PathBuf>,
}

impl SshInstaller {
    pub fn for_agent(mur_home: &Path, agent: &str) -> Self {
        Self {
            scratch: dedicated::key_dir(mur_home, agent),
        }
    }
}

impl Installer for SshInstaller {
    fn install(&self, r: &InstallRequest) -> Result<Installed> {
        check(r)?;
        let kh = known_hosts_file(self.scratch.as_deref(), r.known_hosts)?;
        let mut cmd = Command::new(SSH_BINARY);
        cmd.args(install_argv(r, kh.path())?);
        let out = run(cmd, Some(r.public_line))?;
        parse_result(
            out.status.code(),
            &String::from_utf8_lossy(&out.stdout),
            &String::from_utf8_lossy(&out.stderr),
        )
    }
}

/// Refuse before any process runs, whatever the caller already checked.
pub fn check(r: &InstallRequest) -> Result<()> {
    validate_user(r.user).map_err(|e| anyhow!("{e}"))?;
    if r.public_line.chars().any(char::is_control) || !dedicated::is_key_line(r.public_line) {
        bail!("the public key is not one key line; nothing was sent");
    }
    if r.known_hosts.trim().is_empty() {
        bail!(
            "no scanned host key was approved for this host, so there is nothing to check the server against; nothing was sent"
        );
    }
    Ok(())
}

/// The approved host keys in a private file only this run reads.
pub(super) fn known_hosts_file(
    scratch: Option<&Path>,
    lines: &str,
) -> Result<tempfile::NamedTempFile> {
    let dir = scratch.context("this agent has no key directory to work in")?;
    let mut f = tempfile::NamedTempFile::new_in(dir)
        .with_context(|| format!("create a temporary file in {}", dir.display()))?;
    f.write_all(lines.as_bytes())?;
    if !lines.ends_with('\n') {
        f.write_all(b"\n")?;
    }
    f.flush()?;
    Ok(f)
}

/// The host-key options both connections use: only the keys the user approved
/// on this card are trusted, and nothing is ever asked.
pub(super) fn checked_host_opts(known_hosts_file: &Path) -> Result<Vec<OsString>> {
    let shown = known_hosts_file.to_string_lossy();
    // `ssh` splits an option value on spaces unless it is quoted, and has no
    // escape for a quote inside one.
    if shown.contains('"') {
        bail!("the MUR home path contains a double quote, which ssh options cannot carry");
    }
    Ok([
        "-T".to_string(),
        "-o".into(),
        "BatchMode=yes".into(),
        "-o".into(),
        "StrictHostKeyChecking=yes".into(),
        "-o".into(),
        format!("UserKnownHostsFile=\"{shown}\""),
        "-o".into(),
        format!("GlobalKnownHostsFile={NULL_DEVICE}"),
        "-o".into(),
        format!("ConnectTimeout={CONNECT_TIMEOUT_SECS}"),
    ]
    .into_iter()
    .map(OsString::from)
    .collect())
}

/// `-p <port> -l <user> -- <host> <remote>`: the user only after `-l`, the
/// host only after `--`.
pub(super) fn target_args(host: &RequestHost, user: &str, remote: &str) -> Vec<OsString> {
    [
        "-p".to_string(),
        host.port().to_string(),
        "-l".into(),
        user.to_string(),
        "--".into(),
        host.connect_host().to_string(),
        remote.to_string(),
    ]
    .into_iter()
    .map(OsString::from)
    .collect()
}

pub fn install_argv(r: &InstallRequest, known_hosts_file: &Path) -> Result<Vec<OsString>> {
    let mut argv = checked_host_opts(known_hosts_file)?;
    argv.extend(target_args(r.host, r.user, &remote_command()));
    Ok(argv)
}

/// Exit 0 with one of the two marks is the only success.
pub fn parse_result(code: Option<i32>, stdout: &str, stderr: &str) -> Result<Installed> {
    if code == Some(0) {
        if stdout.lines().any(|l| l.trim() == MARK_ADDED) {
            return Ok(Installed::Added);
        }
        if stdout.lines().any(|l| l.trim() == MARK_PRESENT) {
            return Ok(Installed::AlreadyPresent);
        }
    }
    let status = match code {
        Some(c) => format!("exit {c}"),
        None => "killed by a signal".into(),
    };
    let tail = err_tail(stderr);
    if tail.is_empty() {
        bail!("ssh did not report the key as installed ({status})");
    }
    bail!("ssh did not report the key as installed ({status}): {tail}");
}

/// The last lines of `stderr`, the same rule as a failed quiet start.
pub(super) fn err_tail(stderr: &str) -> String {
    let tail: Vec<&str> = stderr.lines().rev().take(ERR_TAIL_LINES).collect();
    tail.into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// Run to completion with stdin either null or `line` plus a newline. Killed
/// after [`RUN_TIMEOUT`]. The output is a mark or a few lines of error, far
/// below a pipe buffer, so waiting before reading cannot block the child.
pub(super) fn run(mut cmd: Command, line: Option<&str>) -> Result<Output> {
    cmd.stdin(if line.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .with_context(|| format!("could not run {SSH_BINARY}"))?;
    if let (Some(line), Some(mut stdin)) = (line, child.stdin.take()) {
        // A child that exits before reading closes the pipe; its exit status
        // is the error worth reporting, not this write.
        let _ = stdin.write_all(line.as_bytes());
        let _ = stdin.write_all(b"\n");
    }
    let deadline = Instant::now() + RUN_TIMEOUT;
    while child.try_wait()?.is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!(
                "{SSH_BINARY} did not finish within {}s",
                RUN_TIMEOUT.as_secs()
            );
        }
        std::thread::sleep(POLL);
    }
    Ok(child.wait_with_output()?)
}

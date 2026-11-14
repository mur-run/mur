//! The advisory test (SSH setup spec, "Public-key install"): does the server
//! accept the agent's dedicated key? Run by the user's own `ssh`, outside the
//! agent's sandbox, so it says nothing about the agent's own reach. It writes
//! nothing, records no decision and no signing mark.

use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

use mur_ssh_request::{RequestHost, SSH_BINARY, validate_user};

use super::dedicated;
use super::install::{checked_host_opts, err_tail, known_hosts_file, run, target_args};

/// What `ssh` exits with when it could not connect or log in. Any other exit
/// is the remote command's own, so the server let the key in.
pub const SSH_OWN_FAILURE: i32 = 255;
/// The remote command: does nothing, and exits 0 on a host with a shell.
pub const REMOTE_NOOP: &str = "true";
/// Said with every result, so nobody reads it as the agent's own test.
pub const ADVISORY_NOTE: &str = "advisory: run outside the agent's sandbox";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdvisoryResult {
    Accepted,
    /// Refused, unreachable, or the test could not run; the text says which.
    NotAccepted(String),
}

impl AdvisoryResult {
    #[allow(dead_code)] // The clients land in slice 6c.
    pub fn text(&self) -> String {
        match self {
            Self::Accepted => format!("the server accepted the key ({ADVISORY_NOTE})"),
            Self::NotAccepted(why) => {
                format!("not accepted or unreachable: {why} ({ADVISORY_NOTE})")
            }
        }
    }
}

/// `IdentitiesOnly` with `-i`: only the dedicated key is offered, never
/// another key the user's ssh-agent holds.
pub fn advisory_argv(
    private: &Path,
    host: &RequestHost,
    user: &str,
    known_hosts_file: &Path,
) -> anyhow::Result<Vec<OsString>> {
    let mut argv = checked_host_opts(known_hosts_file)?;
    argv.extend(["-o", "IdentitiesOnly=yes", "-i"].map(OsString::from));
    argv.push(private.as_os_str().to_os_string());
    argv.extend(target_args(host, user, REMOTE_NOOP));
    Ok(argv)
}

/// The rule: 255 is ssh's own failure; anything else means the key got in.
pub fn classify(code: Option<i32>, stderr: &str) -> AdvisoryResult {
    match code {
        Some(SSH_OWN_FAILURE) => AdvisoryResult::NotAccepted(err_tail(stderr)),
        Some(_) => AdvisoryResult::Accepted,
        None => AdvisoryResult::NotAccepted("ssh was killed by a signal".into()),
    }
}

pub fn advisory_test(
    mur_home: &Path,
    agent: &str,
    host: &RequestHost,
    user: &str,
    known_hosts: &str,
) -> AdvisoryResult {
    let no = |why: String| AdvisoryResult::NotAccepted(why);
    if let Err(e) = validate_user(user) {
        return no(e.to_string());
    }
    let key = match dedicated::stored(mur_home, agent) {
        Ok(Some(k)) => k,
        Ok(None) => return no("this agent has no dedicated key".into()),
        Err(e) => return no(e),
    };
    let scratch = dedicated::key_dir(mur_home, agent);
    let run_it = || -> anyhow::Result<AdvisoryResult> {
        let kh = known_hosts_file(scratch.as_deref(), known_hosts)?;
        let mut cmd = Command::new(SSH_BINARY);
        cmd.args(advisory_argv(&key.private, host, user, kh.path())?);
        let out = run(cmd, None)?;
        Ok(classify(
            out.status.code(),
            &String::from_utf8_lossy(&out.stderr),
        ))
    };
    run_it().unwrap_or_else(|e| no(format!("{e:#}")))
}

//! Which keys the public-key install connection may offer (plan K7): the
//! user's own, never a MUR dedicated key.
//!
//! Left to itself `ssh` offers every key the user's ssh-agent holds, and past
//! the server's `MaxAuthTries` the host refuses with "Too many authentication
//! failures" before the right key is tried. So the install runs with
//! `IdentitiesOnly=yes` and one `-i` per key this module keeps. Which agent
//! and which files are "the user's" comes from `ssh -G`
//! ([`super::ssh_config`]), not from a guess.
//!
//! Dedicated keys now live in a runtime-private ssh-agent (plan K8), not the
//! user's, and start removes any an older MUR loaded there (K1). Filtering
//! them out here is a second line of defence for a key left behind, e.g. by
//! an agent that has not restarted since.

use std::collections::HashSet;
use std::ffi::OsString;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use mur_ssh_request::{KEY_COMMENT_PREFIX, SSH_ADD_BIN};

use super::dedicated;
use super::ssh_config::{AgentSocket, Expand, Resolved, existing_files};

/// The environment variable `ssh-add` finds its agent by.
const AUTH_SOCK_ENV: &str = "SSH_AUTH_SOCK";
/// `ssh-add -L` exit status when the agent holds no key.
const SSH_ADD_NO_IDENTITIES: i32 = 1;
/// Prefix of one run's private directory of `-i` files in the key directory.
pub const RUN_DIR_PREFIX: &str = ".ids-";
/// A run directory older than this was left by a run that died; the next
/// install sweeps it. Far past the install's own timeout.
pub const RUN_DIR_STALE_AFTER: Duration = Duration::from_secs(10 * 60);

/// What the user's ssh-agent said when asked for its keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentListing {
    /// `ssh-add -L` succeeded: one public line per key.
    Keys(String),
    /// The agent runs but holds no key (`ssh-add -L` exit 1).
    Empty,
    /// No agent to ask, or asking failed. Only identity files are used.
    Unavailable,
}

/// Ask the agent `ssh` itself would use for this host for its keys: the
/// inherited `SSH_AUTH_SOCK`, or the `IdentityAgent` from the user's config.
pub fn list_agent(socket: &AgentSocket) -> AgentListing {
    let mut cmd = Command::new(SSH_ADD_BIN);
    cmd.arg("-L");
    match socket {
        AgentSocket::Disabled => return AgentListing::Unavailable,
        AgentSocket::Path(p) => {
            cmd.env(AUTH_SOCK_ENV, p);
        }
        AgentSocket::Inherited => {}
    }
    let Ok(out) = super::install::run_tool(cmd, SSH_ADD_BIN) else {
        return AgentListing::Unavailable;
    };
    match out.status.code() {
        Some(0) => AgentListing::Keys(String::from_utf8_lossy(&out.stdout).into_owned()),
        Some(SSH_ADD_NO_IDENTITIES) => AgentListing::Empty,
        _ => AgentListing::Unavailable,
    }
}

/// The base64 key blob of a `<type> <base64> [comment]` line.
fn blob_of(line: &str) -> Option<&str> {
    line.split_whitespace().nth(1)
}

/// Blobs of every dedicated key in the key store, whichever agent owns it.
/// Read only: a missing `.pub` is not rebuilt here, since other agents' key
/// directories are not this run's to write. Such a key is still caught by
/// the comment check in [`own_agent_keys`].
pub fn dedicated_blobs(mur_home: &Path) -> HashSet<String> {
    let Some(Ok(entries)) = dedicated::keys_root(mur_home).map(std::fs::read_dir) else {
        return HashSet::new();
    };
    entries
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| !name.starts_with('.'))
        .filter_map(|agent| dedicated::stored_public(mur_home, &agent))
        .filter_map(|line| blob_of(&line).map(str::to_string))
        .collect()
}

/// The agent's keys minus every dedicated one, matched by blob or by the
/// comment MUR writes. The blob match holds even when an agent rewrites
/// comments.
pub fn own_agent_keys(listing: &AgentListing, dedicated: &HashSet<String>) -> Vec<String> {
    let AgentListing::Keys(text) = listing else {
        return Vec::new();
    };
    text.lines()
        .map(str::trim)
        .filter(|l| {
            let Some(blob) = blob_of(l) else {
                return false;
            };
            let comment = dedicated::comment_of(l);
            !dedicated.contains(blob) && !comment.starts_with(KEY_COMMENT_PREFIX)
        })
        .map(str::to_string)
        .collect()
}

/// How the install connection picks its keys.
pub enum Offer {
    /// `IdentitiesOnly=yes` and exactly these `-i` files.
    Only(Identities),
    /// Nothing of the user's could be found, or `ssh -G` could not say:
    /// leave the choice to `ssh`, as before K7. With many dedicated keys
    /// loaded this can still hit `MaxAuthTries`, but it never refuses a
    /// setup that used to work.
    SshDefault,
}

impl Offer {
    /// The `-i` files, or `None` for [`Offer::SshDefault`].
    pub fn files(&self) -> Option<&[PathBuf]> {
        match self {
            Offer::Only(ids) => Some(ids.files()),
            Offer::SshDefault => None,
        }
    }

    /// Pick from what `ssh -G` resolved; `resolved` is `None` when it could
    /// not be asked.
    pub fn choose(
        scratch: Option<&Path>,
        resolved: Option<&Resolved>,
        expand: &Expand,
        mur_home: &Path,
    ) -> Result<Self> {
        let Some(r) = resolved else {
            return Ok(Offer::SshDefault);
        };
        let files = existing_files(r, expand);
        let own = own_agent_keys(&list_agent(&r.agent), &dedicated_blobs(mur_home));
        Ok(match Identities::prepare(scratch, &files, &own)? {
            Some(ids) => Offer::Only(ids),
            None => {
                tracing::warn!("no SSH key of the user's found; leaving the choice to ssh");
                Offer::SshDefault
            }
        })
    }
}

/// The `-i` files for one install run. Agent keys are written as `.pub`
/// files (`ssh` signs those through the agent) in a private directory that
/// is removed when this is dropped; one left by a killed run is swept by
/// the next.
pub struct Identities {
    files: Vec<PathBuf>,
    _run_dir: Option<tempfile::TempDir>,
}

impl Identities {
    /// Identity files first, in the order `ssh -G` gave them (a per-host
    /// `IdentityFile` comes before the defaults, as in a plain `ssh`), then
    /// the agent's keys. `None` when there is nothing to offer.
    pub fn prepare(
        scratch: Option<&Path>,
        files: &[PathBuf],
        agent_keys: &[String],
    ) -> Result<Option<Self>> {
        if files.is_empty() && agent_keys.is_empty() {
            return Ok(None);
        }
        let mut out = files.to_vec();
        let mut run_dir = None;
        if !agent_keys.is_empty() {
            let dir = scratch.context("this agent has no key directory to work in")?;
            sweep_stale(dir, RUN_DIR_STALE_AFTER);
            let tmp = tempfile::Builder::new()
                .prefix(RUN_DIR_PREFIX)
                .tempdir_in(dir)
                .with_context(|| format!("create a temporary directory in {}", dir.display()))?;
            for (n, line) in agent_keys.iter().enumerate() {
                let path = tmp.path().join(format!("agent-{n}.pub"));
                let mut f = std::fs::File::create(&path)
                    .with_context(|| format!("write {}", path.display()))?;
                writeln!(f, "{line}")?;
                out.push(path);
            }
            run_dir = Some(tmp);
        }
        Ok(Some(Self {
            files: out,
            _run_dir: run_dir,
        }))
    }

    pub fn files(&self) -> &[PathBuf] {
        &self.files
    }
}

/// Remove run directories in `dir` older than `min_age`. A link, or a
/// directory with an unreadable or future mtime, is left alone.
pub fn sweep_stale(dir: &Path, min_age: Duration) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let now = SystemTime::now();
    for e in entries.flatten() {
        let ours = e
            .file_name()
            .to_str()
            .is_some_and(|n| n.starts_with(RUN_DIR_PREFIX));
        // `DirEntry::metadata` does not follow a link.
        let Ok(meta) = e.metadata() else { continue };
        if !ours || !meta.is_dir() {
            continue;
        }
        let age = meta
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .unwrap_or(Duration::ZERO);
        if age >= min_age {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

/// `-o IdentitiesOnly=yes` once, then `-i <file>` per identity. `-i` takes
/// its value as its own argument, so a path with spaces needs no quoting.
pub fn identity_args(files: &[PathBuf]) -> Vec<OsString> {
    let mut argv: Vec<OsString> = vec!["-o".into(), "IdentitiesOnly=yes".into()];
    for f in files {
        argv.push("-i".into());
        argv.push(f.as_os_str().to_owned());
    }
    argv
}

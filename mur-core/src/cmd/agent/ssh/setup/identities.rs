//! Which keys the public-key install connection may offer (plan K7): the
//! user's own, never a MUR dedicated key.
//!
//! Every dedicated key stays loaded in the user's ssh-agent (K1). Left to
//! itself `ssh` offers them all, and past the server's `MaxAuthTries` the host
//! refuses with "Too many authentication failures" before the user's key is
//! tried. So the install runs with `IdentitiesOnly=yes` and one `-i` per key
//! this module keeps.

use std::collections::HashSet;
use std::ffi::OsString;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use mur_ssh_request::{KEY_COMMENT_PREFIX, SSH_ADD_BIN};

use super::dedicated;

/// `ssh`'s own default identity files, in `~/.ssh`. Once any `-i` is given it
/// stops adding these, so the ones that exist are passed explicitly.
pub const DEFAULT_IDENTITY_FILES: &[&str] = &[
    "id_rsa",
    "id_ecdsa",
    "id_ecdsa_sk",
    "id_ed25519",
    "id_ed25519_sk",
];
/// `ssh-add -L` exit status when the agent holds no key.
const SSH_ADD_NO_IDENTITIES: i32 = 1;
/// Printed when nothing is left to log in with.
pub const NO_OWN_KEY: &str = "no SSH key of yours to log in with; add one to ssh-agent \
                              (`ssh-add`) or create ~/.ssh/id_ed25519; nothing was sent";

/// What the user's ssh-agent said when asked for its keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentListing {
    /// `ssh-add -L` succeeded: one public line per key.
    Keys(String),
    /// The agent runs but holds no key (`ssh-add -L` exit 1).
    Empty,
    /// No agent to ask, or asking failed. Only on-disk defaults are used.
    Unavailable,
}

/// Ask the user's ssh-agent (the inherited `SSH_AUTH_SOCK`) for its keys.
pub fn list_agent() -> AgentListing {
    let mut cmd = Command::new(SSH_ADD_BIN);
    cmd.arg("-L");
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
/// A directory whose key is unusable contributes nothing; the comment check
/// in [`own_agent_keys`] still catches its key.
pub fn dedicated_blobs(mur_home: &Path) -> HashSet<String> {
    let Some(Ok(entries)) = dedicated::keys_root(mur_home).map(std::fs::read_dir) else {
        return HashSet::new();
    };
    entries
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter_map(|agent| dedicated::stored(mur_home, &agent).ok().flatten())
        .filter_map(|k| blob_of(&k.public_line).map(str::to_string))
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

/// The default identity files in `ssh_dir` that exist.
pub fn default_identity_files(ssh_dir: &Path) -> Vec<PathBuf> {
    DEFAULT_IDENTITY_FILES
        .iter()
        .map(|f| ssh_dir.join(f))
        .filter(|p| p.is_file())
        .collect()
}

/// The `-i` files for one install run. Agent keys are written as `.pub`
/// files (`ssh` signs those through the agent); they are removed when this
/// is dropped, with the run's `known_hosts` file.
pub struct Identities {
    files: Vec<PathBuf>,
    _temps: Vec<tempfile::NamedTempFile>,
}

impl Identities {
    /// Refuses with [`NO_OWN_KEY`] when there is nothing to offer, before any
    /// connection is made.
    pub fn prepare(
        scratch: Option<&Path>,
        agent_keys: &[String],
        defaults: Vec<PathBuf>,
    ) -> Result<Self> {
        if agent_keys.is_empty() && defaults.is_empty() {
            bail!("{NO_OWN_KEY}");
        }
        let mut temps = Vec::with_capacity(agent_keys.len());
        if !agent_keys.is_empty() {
            let dir = scratch.context("this agent has no key directory to work in")?;
            for line in agent_keys {
                let mut f = tempfile::Builder::new()
                    .suffix(".pub")
                    .tempfile_in(dir)
                    .with_context(|| format!("create a temporary file in {}", dir.display()))?;
                writeln!(f, "{line}")?;
                f.flush()?;
                temps.push(f);
            }
        }
        let mut files: Vec<PathBuf> = temps.iter().map(|t| t.path().to_path_buf()).collect();
        files.extend(defaults);
        Ok(Self {
            files,
            _temps: temps,
        })
    }

    pub fn files(&self) -> &[PathBuf] {
        &self.files
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

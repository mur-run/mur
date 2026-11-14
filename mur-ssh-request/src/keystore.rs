//! An agent's dedicated SSH key (SSH setup journey, slice 6).
//!
//! The key lives at `<mur_home>/secrets/ssh/<agent>/id_ed25519`. `secrets/`
//! is denied read AND write to every sealed agent (`mur-agent-runtime`,
//! `sandbox/launch_chain.rs`, `protects_credential`), so the agent uses the
//! key only through the filtering ssh-agent proxy and never holds the bytes.
//!
//! A key file grants nothing by itself: the profile's `entitlements.ssh` is
//! the authority, and every caller that loads a key checks the grant first.
//!
//! Callers must not put the private path, the public line or the fingerprint
//! in anything an agent reads (a tool reply, a prompt section, a hand-back).

use std::fs;
use std::io::{self, Read as _, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use mur_common::agent::fingerprint_of_base64;
use mur_common::hitl::approval_token::{SECRETS_DIR, create_private_dir};
use mur_common::{AgentNameError, validate_agent_name};

use crate::{KEYGEN_BIN, SSH_ADD_BIN};

/// `<mur_home>/secrets/ssh/<agent>/`.
pub const KEYS_SUBDIR: &str = "ssh";
/// File name of the private key inside the agent's key directory.
pub const KEY_FILE: &str = "id_ed25519";
/// The one key type generated; `ssh-keygen -t` takes it as is.
pub const KEY_TYPE: &str = "ed25519";
/// Suffix `ssh-keygen` gives the public half.
pub const PUB_SUFFIX: &str = ".pub";
/// Private temporary directory of a generation in progress.
pub const PARTIAL_PREFIX: &str = ".gen-";
/// Key comment: `mur-agent-<agent>`. Display only.
pub const COMMENT_PREFIX: &str = "mur-agent-";
/// `ssh-add` / `ssh-keygen` are local and prompt for nothing here.
pub const TOOL_TIMEOUT: Duration = Duration::from_secs(10);
/// A public key line is far below this; anything larger is not one.
pub const MAX_PUB_BYTES: u64 = 4 * 1024;

/// Prefix OpenSSH puts before the key type on a public key line.
const PUB_TYPE_PREFIX: &str = "ssh-";
/// Mode bits that would let anyone but the owner reach the private key.
const GROUP_OTHER_BITS: u32 = 0o077;
/// How often a running tool is polled for exit.
const POLL: Duration = Duration::from_millis(20);

#[derive(Debug, thiserror::Error)]
pub enum KeystoreError {
    #[error("invalid agent name: {0}")]
    Name(#[from] AgentNameError),
    #[error("{0}")]
    Io(#[from] io::Error),
    /// The file is not a key this store wrote. Nothing is repaired or
    /// removed automatically; the message names the file to delete.
    #[error("{} is not a usable dedicated key ({reason}); delete it to make a new one", path.display())]
    Unusable { path: PathBuf, reason: &'static str },
    #[error("could not run {tool}: {source}")]
    Spawn {
        tool: &'static str,
        source: io::Error,
    },
    /// `stderr` is kept out of the message: it can name the private path.
    #[error("{tool} failed{}", code.map(|c| format!(" (exit {c})")).unwrap_or_default())]
    ToolFailed {
        tool: &'static str,
        code: Option<i32>,
        stderr: String,
    },
    #[error("{tool} did not finish within {}s", TOOL_TIMEOUT.as_secs())]
    Timeout { tool: &'static str },
}

/// A dedicated key that passed every check in [`stored`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredKey {
    pub private: PathBuf,
    /// One line: `<type> <base64> [comment]`.
    pub public_line: String,
    pub fingerprint: String,
}

/// What [`unload_and_delete`] found and did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Removal {
    /// The agent had no key directory.
    Absent,
    /// The directory is gone. `unloaded` is false when ssh-agent could not be
    /// told to drop the key (not running, or the key was unusable).
    Removed { unloaded: bool },
}

/// The agent's key directory. Checks the name; creates nothing.
pub fn key_dir(mur_home: &Path, agent: &str) -> Result<PathBuf, KeystoreError> {
    validate_agent_name(agent)?;
    Ok(mur_home.join(SECRETS_DIR).join(KEYS_SUBDIR).join(agent))
}

/// The agent's key, or `Ok(None)` when it has none.
///
/// A missing public half is rebuilt from the private key. Anything else that
/// is off is [`KeystoreError::Unusable`]: a link at the key's name, a key
/// group or others can reach, a public line that is not one key of
/// [`KEY_TYPE`].
pub fn stored(mur_home: &Path, agent: &str) -> Result<Option<StoredKey>, KeystoreError> {
    let dir = key_dir(mur_home, agent)?;
    let private = dir.join(KEY_FILE);
    let meta = match fs::symlink_metadata(&private) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if !meta.file_type().is_file() {
        return Err(unusable(&private, "not a regular file"));
    }
    if meta.permissions().mode() & GROUP_OTHER_BITS != 0 {
        return Err(unusable(&private, "readable by group or others"));
    }
    let public = pub_path(&private);
    let line = match read_pub(&public)? {
        Some(l) => l,
        None => rebuild_pub(&private, &public)?,
    };
    let fingerprint = parse_pub(&line).ok_or_else(|| unusable(&public, "not one public key"))?;
    Ok(Some(StoredKey {
        private,
        public_line: line,
        fingerprint,
    }))
}

/// The agent's key, generating it first if it has none. Never overwrites:
/// two callers racing both end up holding the winner's key.
pub fn generate(mur_home: &Path, agent: &str) -> Result<StoredKey, KeystoreError> {
    let dir = key_dir(mur_home, agent)?;
    let secrets = mur_home.join(SECRETS_DIR);
    create_private_dir(&secrets)?;
    create_private_dir(&secrets.join(KEYS_SUBDIR))?;
    create_private_dir(&dir)?;
    if let Some(key) = stored(mur_home, agent)? {
        return Ok(key);
    }
    // 0700, removed on drop: no reader sees a half-written key.
    let tmp = tempfile::Builder::new()
        .prefix(PARTIAL_PREFIX)
        .tempdir_in(&dir)?;
    let tmp_private = tmp.path().join(KEY_FILE);
    let mut cmd = Command::new(KEYGEN_BIN);
    cmd.arg("-q")
        .args(["-t", KEY_TYPE])
        .args(["-N", ""])
        .arg("-C")
        .arg(format!("{COMMENT_PREFIX}{agent}"))
        .arg("-f")
        .arg(&tmp_private);
    run_ok(cmd, KEYGEN_BIN)?;
    let private = dir.join(KEY_FILE);
    // `link(2)` fails if the name exists: the commit point.
    match fs::hard_link(&tmp_private, &private) {
        Ok(()) => {
            // Not fatal: `stored` rebuilds a missing public half.
            let _ = fs::hard_link(pub_path(&tmp_private), pub_path(&private));
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into()),
    }
    drop(tmp);
    stored(mur_home, agent)?.ok_or_else(|| {
        KeystoreError::Io(io::Error::new(
            io::ErrorKind::NotFound,
            "the dedicated key vanished right after it was generated",
        ))
    })
}

/// Remove what an interrupted [`generate`] left behind. Never touches a key.
/// Do not call it while another process may be generating for this agent.
pub fn sweep_partial(mur_home: &Path, agent: &str) {
    let Ok(dir) = key_dir(mur_home, agent) else {
        return;
    };
    let Ok(entries) = fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let is_partial = entry
            .file_name()
            .to_str()
            .is_some_and(|n| n.starts_with(PARTIAL_PREFIX));
        // `file_type` does not follow a link, so a link is left alone.
        if is_partial && entry.file_type().is_ok_and(|t| t.is_dir()) {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

/// Add the key to the user's ssh-agent. Adding a key twice is not an error.
pub fn load(key: &StoredKey) -> Result<(), KeystoreError> {
    let mut cmd = Command::new(SSH_ADD_BIN);
    cmd.arg("-q").arg(&key.private);
    run_ok(cmd, SSH_ADD_BIN).map(drop)
}

/// Drop the key from ssh-agent and delete the agent's key directory.
///
/// The directory is deleted even when ssh-agent cannot be reached: the file is
/// what outlives a reboot.
pub fn unload_and_delete(mur_home: &Path, agent: &str) -> Result<Removal, KeystoreError> {
    let dir = key_dir(mur_home, agent)?;
    match fs::symlink_metadata(&dir) {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Removal::Absent),
        Err(e) => return Err(e.into()),
    }
    let unloaded = match stored(mur_home, agent) {
        Ok(Some(key)) => {
            let mut cmd = Command::new(SSH_ADD_BIN);
            cmd.arg("-q").arg("-d").arg(&key.private);
            run_ok(cmd, SSH_ADD_BIN).is_ok()
        }
        _ => false,
    };
    // Does not follow links: a link inside is removed, not its target.
    fs::remove_dir_all(&dir)?;
    Ok(Removal::Removed { unloaded })
}

fn unusable(path: &Path, reason: &'static str) -> KeystoreError {
    KeystoreError::Unusable {
        path: path.to_path_buf(),
        reason,
    }
}

fn pub_path(private: &Path) -> PathBuf {
    let mut s = private.as_os_str().to_os_string();
    s.push(PUB_SUFFIX);
    PathBuf::from(s)
}

/// The trimmed public line, or `None` when the file is missing.
fn read_pub(public: &Path) -> Result<Option<String>, KeystoreError> {
    let meta = match fs::symlink_metadata(public) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if !meta.file_type().is_file() {
        return Err(unusable(public, "not a regular file"));
    }
    let mut buf = String::new();
    fs::File::open(public)?
        .take(MAX_PUB_BYTES + 1)
        .read_to_string(&mut buf)
        .map_err(|_| unusable(public, "not text"))?;
    if buf.len() as u64 > MAX_PUB_BYTES {
        return Err(unusable(public, "too large"));
    }
    Ok(Some(buf.trim().to_string()))
}

/// Fingerprint of a `<type> <base64> [comment]` line of [`KEY_TYPE`].
fn parse_pub(line: &str) -> Option<String> {
    if line.contains(['\n', '\r']) {
        return None;
    }
    let mut parts = line.split_whitespace();
    let kind = parts.next()?;
    if kind.strip_prefix(PUB_TYPE_PREFIX) != Some(KEY_TYPE) {
        return None;
    }
    fingerprint_of_base64(parts.next()?)
}

/// Derive the public line from the private key and store it beside it.
fn rebuild_pub(private: &Path, public: &Path) -> Result<String, KeystoreError> {
    let mut cmd = Command::new(KEYGEN_BIN);
    cmd.arg("-y").arg("-f").arg(private);
    let out = run_ok(cmd, KEYGEN_BIN)?;
    let line = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if parse_pub(&line).is_none() {
        return Err(unusable(private, "no public key could be derived"));
    }
    let dir = public.parent().unwrap_or(Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    writeln!(tmp, "{line}")?;
    match tmp.persist_noclobber(public) {
        Ok(_) => Ok(line),
        // Someone else rebuilt it first: theirs is the one on disk.
        Err(e) if e.error.kind() == io::ErrorKind::AlreadyExists => {
            read_pub(public)?.ok_or_else(|| unusable(public, "vanished while it was read"))
        }
        Err(e) => Err(e.error.into()),
    }
}

/// Run a local OpenSSH tool with no stdin, under [`TOOL_TIMEOUT`].
fn run_ok(mut cmd: Command, tool: &'static str) -> Result<Output, KeystoreError> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| KeystoreError::Spawn { tool, source })?;
    let deadline = Instant::now() + TOOL_TIMEOUT;
    // These tools print at most a line or two, far below a pipe buffer, so
    // waiting before reading cannot block the child.
    while child.try_wait()?.is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(KeystoreError::Timeout { tool });
        }
        std::thread::sleep(POLL);
    }
    let out = child.wait_with_output()?;
    if out.status.success() {
        Ok(out)
    } else {
        Err(KeystoreError::ToolFailed {
            tool,
            code: out.status.code(),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        })
    }
}

#[cfg(test)]
#[path = "keystore_tests.rs"]
mod tests;

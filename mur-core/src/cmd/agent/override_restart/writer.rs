//! The writer side of the override file: lock, orphan sweep, publish,
//! withdraw. Pure file operations over one `dir`
//! (`<mur_home>/secrets/startup-overrides`); the orchestrator decides what
//! each outcome means for the user.
//!
//! Spec: `2026-10-25-startup-permission-overrides-design.md` §Names and
//! owners, §Ordering.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use fs2::FileExt;
use mur_common::agent_name::validate_agent_name;
use mur_common::startup_override::claim::{self, ClaimAs, Claimed, live_path};
use mur_common::startup_override::{NONCE_HEX_LEN, NONCE_PREFIX, OverrideFile};

const LOCK_SUFFIX: &str = ".lock";
const TMP_SUFFIX: &str = ".tmp";
const NONCE_BYTES: usize = NONCE_HEX_LEN / 2;

/// Holds `<agent>.lock` exclusively. Dropping it releases the `flock`; the
/// kernel releases it too if the process dies. The lock file is never removed.
#[derive(Debug)]
pub(crate) struct WriterLock {
    _file: File,
}

#[derive(Debug)]
pub(crate) enum LockError {
    /// Another CLI or Hub writer holds the lock for this agent.
    Busy,
    Io(io::Error),
}

/// `flock(LOCK_EX | LOCK_NB)` on `<dir>/<agent>.lock`. Never blocks.
pub(crate) fn lock(dir: &Path, agent: &str) -> Result<WriterLock, LockError> {
    check_name(agent).map_err(LockError::Io)?;
    create_private_dir(dir).map_err(LockError::Io)?;
    let mut opts = OpenOptions::new();
    opts.create(true).read(true).write(true).truncate(false);
    private_file_mode(&mut opts);
    let file = opts
        .open(dir.join(format!("{agent}{LOCK_SUFFIX}")))
        .map_err(LockError::Io)?;
    match file.try_lock_exclusive() {
        Ok(()) => Ok(WriterLock { _file: file }),
        Err(e) if e.raw_os_error() == fs2::lock_contended_error().raw_os_error() => {
            Err(LockError::Busy)
        }
        Err(e) => Err(LockError::Io(e)),
    }
}

/// `ovr-` + 32 lowercase hex digits from the OS RNG.
pub(crate) fn new_nonce() -> io::Result<String> {
    use std::fmt::Write as _;
    let mut buf = [0u8; NONCE_BYTES];
    getrandom::getrandom(&mut buf).map_err(io::Error::other)?;
    let mut s = String::with_capacity(NONCE_PREFIX.len() + NONCE_HEX_LEN);
    s.push_str(NONCE_PREFIX);
    for b in buf {
        let _ = write!(s, "{b:02x}");
    }
    Ok(s)
}

/// What the orphan sweep found under the lock.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Swept {
    /// No live file (never published, or a boot consumed it meanwhile).
    Clear,
    /// A live file was withdrawn and its copy unlinked. `nonce` is `None`
    /// when the copy could not be read or carried no nonce.
    Discarded { nonce: Option<String> },
}

/// Step 2. Withdraws any live file, whatever its age, and never puts it
/// back. `Err` is "withdraw failed": the orphan stays live and the caller
/// must publish nothing.
pub(crate) fn sweep(dir: &Path, agent: &str) -> io::Result<Swept> {
    match claim::claim(dir, agent, ClaimAs::Withdraw) {
        Claimed::Absent => Ok(Swept::Clear),
        Claimed::Failed(e) => Err(e),
        Claimed::Took(path) => {
            let nonce = read_nonce(&path);
            remove_quietly(&path);
            Ok(Swept::Discarded { nonce })
        }
    }
}

#[derive(Debug)]
pub(crate) enum PublishError {
    /// `link` hit `EEXIST`: a live file appeared after the sweep.
    Exists,
    /// Failed before the link: nothing is live.
    Io(io::Error),
    /// The link succeeded, so `<agent>.json` is live, but the directory
    /// `fsync` failed. The caller owns the file and must Cleanup.
    Unsynced(io::Error),
}

impl std::fmt::Display for PublishError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Exists => f.write_str("another override file appeared"),
            Self::Io(e) => write!(f, "{e}"),
            Self::Unsynced(e) => write!(f, "could not sync the override directory: {e}"),
        }
    }
}

/// Step 3. `<agent>.<nonce>.tmp` → `fsync` → `link` to `<agent>.json` (never
/// replaces) → unlink the tmp → `fsync` the directory. The tmp name is gone
/// on every return; the live file exists on `Ok` and on `Unsynced` only.
pub(crate) fn publish(dir: &Path, agent: &str, file: &OverrideFile) -> Result<(), PublishError> {
    check_name(agent).map_err(PublishError::Io)?;
    create_private_dir(dir).map_err(PublishError::Io)?;
    let body = serde_json::to_vec(file).map_err(|e| PublishError::Io(io::Error::other(e)))?;
    let tmp = tmp_path(dir, agent, &file.nonce);
    let linked =
        write_tmp(&tmp, &body).and_then(|()| std::fs::hard_link(&tmp, live_path(dir, agent)));
    remove_quietly(&tmp);
    match linked {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Err(PublishError::Exists),
        Err(e) => return Err(PublishError::Io(e)),
    }
    sync_published_dir(dir).map_err(PublishError::Unsynced)
}

#[cfg(test)]
type DirSyncHook = Box<dyn FnMut(&Path) -> io::Result<()>>;

#[cfg(test)]
thread_local! {
    /// Runs in place of the directory `fsync` after the link, so tests can
    /// fail it (and act on the live file first).
    pub(super) static DIR_SYNC_HOOK: std::cell::RefCell<Option<DirSyncHook>> =
        const { std::cell::RefCell::new(None) };
}

fn sync_published_dir(dir: &Path) -> io::Result<()> {
    #[cfg(test)]
    if let Some(r) = DIR_SYNC_HOOK.with(|h| h.borrow_mut().as_mut().map(|f| f(dir))) {
        return r;
    }
    sync_dir(dir)
}

/// What a Cleanup withdraw found.
#[derive(Debug)]
pub(crate) enum Withdrawn {
    /// We took our own file back and unlinked it: no boot ever saw it.
    Ours,
    /// We took a file whose nonce is not ours. It is left in place as
    /// `<agent>.withdrawn.json`. Cannot happen while the lock is held.
    Foreign { found: Option<String> },
    /// `ENOENT`: a boot consumed the file. Classify the lock file next.
    Consumed,
    /// Any other error: the file is left live.
    Failed(io::Error),
}

/// Step 6. Claim, then read the withdrawn copy and unlink it only if the
/// nonce is ours.
pub(crate) fn withdraw(dir: &Path, agent: &str, ours: &str) -> Withdrawn {
    match claim::claim(dir, agent, ClaimAs::Withdraw) {
        Claimed::Absent => Withdrawn::Consumed,
        Claimed::Failed(e) => Withdrawn::Failed(e),
        Claimed::Took(path) => match read_nonce(&path) {
            Some(n) if n == ours => {
                remove_quietly(&path);
                Withdrawn::Ours
            }
            found => Withdrawn::Foreign { found },
        },
    }
}

fn tmp_path(dir: &Path, agent: &str, nonce: &str) -> PathBuf {
    dir.join(format!("{agent}.{nonce}{TMP_SUFFIX}"))
}

fn write_tmp(path: &Path, body: &[u8]) -> io::Result<()> {
    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    private_file_mode(&mut opts);
    let mut f = opts.open(path)?;
    f.write_all(body)?;
    f.sync_all()
}

/// Only the nonce is read, loosely: a copy the runtime would reject as
/// malformed still has to be discarded.
fn read_nonce(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    v.get("nonce")?.as_str().map(str::to_owned)
}

/// A leftover tmp or withdrawn copy is read only by the writer, and the next
/// claim's `rename` replaces it, so a failed unlink is logged, not fatal.
fn remove_quietly(path: &Path) {
    if let Err(e) = std::fs::remove_file(path)
        && e.kind() != io::ErrorKind::NotFound
    {
        tracing::warn!(path = %path.display(), error = %e, "could not remove override scratch file");
    }
}

fn check_name(agent: &str) -> io::Result<()> {
    validate_agent_name(agent).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))
}

fn create_private_dir(dir: &Path) -> io::Result<()> {
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    b.create(dir)
}

fn private_file_mode(opts: &mut OpenOptions) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    #[cfg(not(unix))]
    let _ = opts;
}

#[cfg(unix)]
fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

/// Windows cannot open a directory for `FlushFileBuffers` without backup
/// semantics; NTFS journals the link itself.
#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
#[path = "writer_tests.rs"]
mod tests;

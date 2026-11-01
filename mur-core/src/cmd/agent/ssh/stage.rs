//! Stage and install `<agent home>/ssh/known_hosts` without following links
//! the agent planted.
//!
//! The agent's sandbox may write anywhere under its home, including `ssh/`,
//! and this CLI runs as the user. So nothing below `agent_home` is trusted:
//!
//! - `ssh/` is opened once, `O_NOFOLLOW | O_DIRECTORY`, and every later step
//!   is relative to that descriptor. A symlinked `ssh/` is refused, and
//!   swapping the directory afterwards cannot redirect the write, because the
//!   descriptor still names the directory that was opened.
//! - The staged file gets a fresh random name and is created `O_CREAT |
//!   O_EXCL | O_NOFOLLOW`, so a name the agent pre-created (file or symlink)
//!   fails instead of being written through.
//! - Install is `renameat`, which replaces a symlink at `known_hosts` rather
//!   than writing to its target.
//!
//! `agent_home` itself is trusted: its parent `<mur_home>/agents` is denied to
//! every agent, so the agent cannot replace its own home.

use std::path::{Path, PathBuf};

#[cfg(unix)]
use anyhow::Context;
use anyhow::Result;

/// Prefix of the staged file; a random suffix follows.
const STAGED_PREFIX: &str = "known_hosts.mur-staged.";
/// Random bytes in the staged name (hex-encoded, so twice as many chars).
#[cfg(unix)]
const STAGED_RANDOM_BYTES: usize = 8;
/// `known_hosts` holds public host keys only (0644); ssh does not require it
/// private.
#[cfg(unix)]
fn known_hosts_mode() -> rustix::fs::Mode {
    use rustix::fs::Mode;
    Mode::RUSR | Mode::WUSR | Mode::RGRP | Mode::ROTH
}

/// `ssh/` when this module has to create it (0700).
#[cfg(unix)]
fn ssh_dir_mode() -> rustix::fs::Mode {
    rustix::fs::Mode::RWXU
}

/// A staged `known_hosts`. Dropping it without [`Staged::install`] removes it.
pub struct Staged {
    #[cfg(unix)]
    dir: rustix::fd::OwnedFd,
    #[cfg(unix)]
    name: String,
    #[cfg(not(unix))]
    file: Option<tempfile::NamedTempFile>,
    shown: PathBuf,
}

#[cfg(unix)]
pub fn stage(agent_home: &Path, content: &str) -> Result<Staged> {
    use std::io::Write;

    use rustix::fs::{Mode, OFlags};

    use mur_common::agent::{KNOWN_HOSTS, SSH_DIR};

    let dir_flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
    let home = rustix::fs::open(agent_home, dir_flags, Mode::empty())
        .with_context(|| format!("open {}", agent_home.display()))?;
    let ssh_shown = agent_home.join(SSH_DIR);
    let nofollow = dir_flags | OFlags::NOFOLLOW;
    let dir = match rustix::fs::openat(&home, SSH_DIR, nofollow, Mode::empty()) {
        Ok(fd) => fd,
        Err(rustix::io::Errno::NOENT) => {
            match rustix::fs::mkdirat(&home, SSH_DIR, ssh_dir_mode()) {
                Ok(()) | Err(rustix::io::Errno::EXIST) => {}
                Err(e) => {
                    return Err(e).with_context(|| format!("create {}", ssh_shown.display()));
                }
            }
            open_ssh_dir(&home, nofollow, &ssh_shown)?
        }
        Err(e) => return Err(refused(e, &ssh_shown)),
    };

    let name = format!("{STAGED_PREFIX}{}", random_hex()?);
    let shown = ssh_shown.join(&name);
    let fd = rustix::fs::openat(
        &dir,
        name.as_str(),
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        known_hosts_mode(),
    )
    .with_context(|| format!("create {}", shown.display()))?;
    // From here on the staged file exists, so Drop must be able to remove it.
    let staged = Staged {
        dir,
        name,
        shown: ssh_shown.join(KNOWN_HOSTS),
    };
    let mut file = std::fs::File::from(fd);
    file.write_all(content.as_bytes())
        .and_then(|()| file.sync_all())
        .with_context(|| format!("write {}", shown.display()))?;
    Ok(staged)
}

#[cfg(unix)]
fn open_ssh_dir(
    home: &rustix::fd::OwnedFd,
    flags: rustix::fs::OFlags,
    shown: &Path,
) -> Result<rustix::fd::OwnedFd> {
    rustix::fs::openat(
        home,
        mur_common::agent::SSH_DIR,
        flags,
        rustix::fs::Mode::empty(),
    )
    .map_err(|e| refused(e, shown))
}

/// `ELOOP` / `ENOTDIR` mean the agent put something other than a directory
/// at `ssh/`; say so rather than print a bare errno.
#[cfg(unix)]
fn refused(e: rustix::io::Errno, shown: &Path) -> anyhow::Error {
    match e {
        rustix::io::Errno::LOOP | rustix::io::Errno::NOTDIR => anyhow::anyhow!(
            "{} is not a plain directory (a symlink or a file). The agent can write there, so \
             MUR will not follow it. Remove it and re-run.",
            shown.display()
        ),
        other => anyhow::Error::from(other).context(format!("open {}", shown.display())),
    }
}

#[cfg(unix)]
fn random_hex() -> Result<String> {
    use std::fmt::Write as _;
    let mut buf = [0u8; STAGED_RANDOM_BYTES];
    getrandom::getrandom(&mut buf).context("read the OS random source")?;
    let mut s = String::with_capacity(STAGED_RANDOM_BYTES * 2);
    for b in buf {
        let _ = write!(s, "{b:02x}");
    }
    Ok(s)
}

/// No sandbox boundary exists here (the ssh proxy does not start on
/// Windows), so a temp file with a random name is enough; the symlink check
/// on `ssh/` is best effort.
#[cfg(not(unix))]
pub fn stage(agent_home: &Path, content: &str) -> Result<Staged> {
    use std::io::Write;

    use anyhow::{Context, bail};
    use mur_common::agent::{SSH_DIR, known_hosts_path};

    let dir = agent_home.join(SSH_DIR);
    if std::fs::symlink_metadata(&dir).is_ok_and(|m| m.file_type().is_symlink()) {
        bail!("{} is a symlink; MUR will not follow it", dir.display());
    }
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let mut file = tempfile::Builder::new()
        .prefix(STAGED_PREFIX)
        .tempfile_in(&dir)
        .with_context(|| format!("stage in {}", dir.display()))?;
    file.write_all(content.as_bytes())
        .and_then(|()| file.as_file().sync_all())
        .context("write staged known_hosts")?;
    Ok(Staged {
        file: Some(file),
        shown: known_hosts_path(agent_home),
    })
}

impl Staged {
    /// Move the staged file over `known_hosts`. On error the staged file is
    /// still removed by Drop.
    #[cfg(unix)]
    pub fn install(mut self) -> Result<()> {
        rustix::fs::renameat(
            &self.dir,
            self.name.as_str(),
            &self.dir,
            mur_common::agent::KNOWN_HOSTS,
        )
        .with_context(|| format!("install {}", self.shown.display()))?;
        self.name.clear();
        Ok(())
    }

    #[cfg(not(unix))]
    pub fn install(mut self) -> Result<()> {
        use anyhow::Context;
        let file = self.file.take().expect("staged file present until install");
        file.persist(&self.shown)
            .map_err(|e| e.error)
            .with_context(|| format!("install {}", self.shown.display()))?;
        Ok(())
    }
}

#[cfg(unix)]
impl Drop for Staged {
    fn drop(&mut self) {
        if !self.name.is_empty() {
            let _ =
                rustix::fs::unlinkat(&self.dir, self.name.as_str(), rustix::fs::AtFlags::empty());
        }
    }
}

/// Staged files left by a process that died mid-write, for tests.
#[cfg(test)]
pub(super) const STAGED_PREFIX_FOR_TESTS: &str = STAGED_PREFIX;

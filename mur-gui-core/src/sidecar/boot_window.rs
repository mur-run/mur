//! Hub Stop in the boot window: the target when there is no `running.lock` yet.
//!
//! A booting runtime takes the `running.sentinel` flock and writes its pid
//! into the sentinel before it seals the startup override and writes
//! `running.lock`. Until then `lock_owner_pid` sees nothing, so a Hub Stop
//! would signal no one. `sentinel_target` falls back to the sentinel's pid,
//! and only when that pid is provably the runtime:
//!
//! 1. No sentinel → nothing to stop (it is never created here).
//! 2. The flock is free → nobody owns the agent: truncate it, signal nothing.
//! 3. The flock is held → the pid it names, if alive **and** its executable
//!    is the MUR runtime. A SIGKILLed runtime skips `release()` and leaves its
//!    pid behind; a new holder that has flocked but not yet rewritten the file
//!    would expose that stale pid, which may now be an unrelated process. The
//!    executable check is what keeps that process from being signalled.

use std::io::Read;
use std::path::{Path, PathBuf};

use fs2::FileExt;
use mur_common::lock_file::{SENTINEL_FILE, pid_alive};

/// The pid to stop for `slug` when its runtime is still booting, if any.
pub(super) fn sentinel_target(mur_home: &Path, slug: &str) -> Option<u32> {
    let path = mur_home.join("agents").join(slug).join(SENTINEL_FILE);
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(false)
        .open(path)
        .ok()?;
    if file.try_lock_exclusive().is_ok() {
        // Free: whatever pid it names is no runtime's. Erase it while we hold
        // the flock, then let the handle drop.
        let _ = file.set_len(0);
        return None;
    }
    let mut text = String::new();
    file.read_to_string(&mut text).ok()?;
    let pid = text.trim().parse::<u32>().ok()?;
    (pid_alive(pid) && is_runtime_pid(pid, slug)).then_some(pid)
}

/// Whether `pid`'s executable is the MUR runtime: the binary the Hub would
/// launch, or the agent's `mur_agent_<slug>` symlink. Unreadable → `false`.
fn is_runtime_pid(pid: u32, slug: &str) -> bool {
    let Some(exe) = pid_exe(pid).and_then(|p| p.canonicalize().ok()) else {
        return false;
    };
    runtime_candidates(slug)
        .into_iter()
        .filter_map(|p| p.canonicalize().ok())
        .any(|p| p == exe)
}

fn runtime_candidates(slug: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(p) = super::find_runtime_binary() {
        out.push(p);
    }
    if let Some(dir) = mur_common::agent_bin::bin_dir() {
        out.push(dir.join(mur_common::agent_bin::link_name(slug)));
    }
    out
}

/// The executable path of a live process.
#[cfg(target_os = "macos")]
fn pid_exe(pid: u32) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;

    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: the buffer is PROC_PIDPATHINFO_MAXSIZE bytes and its length is
    // passed alongside; proc_pidpath writes at most that many bytes.
    let n = unsafe {
        libc::proc_pidpath(
            pid as libc::c_int,
            buf.as_mut_ptr().cast::<libc::c_void>(),
            buf.len() as u32,
        )
    };
    if n <= 0 {
        return None;
    }
    buf.truncate(n as usize);
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(&buf)))
}

/// The executable path of a live process.
#[cfg(target_os = "linux")]
fn pid_exe(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/exe")).ok()
}

/// Not portable (FreeBSD has neither `proc_pidpath` nor `/proc`; Windows is
/// unsupported): never identify the runtime, so never signal.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn pid_exe(_pid: u32) -> Option<PathBuf> {
    None
}

#[cfg(all(test, unix))]
#[path = "boot_window_tests.rs"]
mod tests;

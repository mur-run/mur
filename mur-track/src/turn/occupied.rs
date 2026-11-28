//! Which turn tracks a live process is still working in (#2639).
//!
//! The reclaimer's ownership and age checks read files git and the runtime
//! write; neither proves nobody is inside the track right now. A shell, a
//! build, or a sub-agent whose working directory is a track would be left
//! standing in a deleted directory. Before reclaiming, ask the OS for every
//! visible process's current directory and keep any track one of them sits
//! in.
//!
//! Only the working directory is checked, not open files: it is one cheap
//! query per process, and it is what a process "in" a track has. Processes
//! the OS will not describe (another user's, without privilege) are not
//! seen. On a platform with no implementation the list is empty, so the
//! reclaimer behaves as it did before this check existed.

use std::path::{Path, PathBuf};

/// Current working directories of every process this user may inspect,
/// excluding our own (a sweep run from inside a track must not keep it
/// alive on its own account — it is about to leave).
pub(super) fn process_cwds() -> Vec<PathBuf> {
    let me = std::process::id();
    imp::cwds(me)
}

/// Is `track` (or any directory below it) some process's working directory?
/// `track` is canonicalized first because the OS reports resolved paths.
pub(super) fn is_occupied(track: &Path, cwds: &[PathBuf]) -> bool {
    let track = super::canonicalize(track).unwrap_or_else(|_| track.to_path_buf());
    cwds.iter().any(|c| c.starts_with(&track))
}

#[cfg(target_os = "linux")]
mod imp {
    use std::path::PathBuf;

    pub(super) fn cwds(me: u32) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
            .filter(|&pid| pid != me)
            .filter_map(|pid| std::fs::read_link(format!("/proc/{pid}/cwd")).ok())
            .collect()
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::CStr;
    use std::os::unix::ffi::OsStrExt;
    use std::path::PathBuf;

    /// Slack added to the pid buffer: processes started between sizing the
    /// buffer and filling it.
    const PID_SLACK: usize = 64;

    pub(super) fn cwds(me: u32) -> Vec<PathBuf> {
        pids()
            .into_iter()
            .filter(|&pid| pid > 0 && pid as u32 != me)
            .filter_map(cwd_of)
            .collect()
    }

    fn pids() -> Vec<libc::pid_t> {
        // SAFETY: a null buffer asks only for the count.
        let n = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
        if n <= 0 {
            return Vec::new();
        }
        let mut buf: Vec<libc::pid_t> = vec![0; n as usize + PID_SLACK];
        let bytes = (buf.len() * size_of::<libc::pid_t>()) as libc::c_int;
        // SAFETY: `buf` holds `bytes` writable bytes.
        let got = unsafe { libc::proc_listallpids(buf.as_mut_ptr().cast(), bytes) };
        if got <= 0 {
            return Vec::new();
        }
        buf.truncate((got as usize).min(buf.len()));
        buf
    }

    fn cwd_of(pid: libc::pid_t) -> Option<PathBuf> {
        // SAFETY: an all-zero proc_vnodepathinfo is a valid value.
        let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
        let size = size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
        // SAFETY: `info` is `size` writable bytes of the flavor's struct.
        let got = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDVNODEPATHINFO,
                0,
                (&raw mut info).cast(),
                size,
            )
        };
        if got != size {
            return None;
        }
        let raw = &info.pvi_cdir.vip_path;
        // SAFETY: `raw` is MAXPATHLEN initialized bytes; a path with no NUL
        // in them is rejected below rather than read past.
        let bytes: &[u8] =
            unsafe { std::slice::from_raw_parts(raw.as_ptr().cast(), size_of_val(raw)) };
        let path = CStr::from_bytes_until_nul(bytes).ok()?.to_bytes();
        (!path.is_empty()).then(|| PathBuf::from(std::ffi::OsStr::from_bytes(path)))
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod imp {
    use std::path::PathBuf;

    pub(super) fn cwds(_me: u32) -> Vec<PathBuf> {
        Vec::new()
    }
}

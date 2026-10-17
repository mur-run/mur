//! Raise the soft open-file limit at startup.
//!
//! macOS launchd hands processes a soft `RLIMIT_NOFILE` of 256. An agent
//! that drives a browser (Playwright MCP + Chromium) alongside other MCP
//! servers exhausts that quickly, and every later `spawn` then fails with
//! `EMFILE` ("Too many open files"). Go has raised this automatically since
//! 1.19; we do the same: lift the soft limit toward the hard limit, never
//! touching the hard limit itself.

/// Ceiling for the soft limit. macOS rejects `setrlimit(RLIMIT_NOFILE)`
/// above `OPEN_MAX` (10240) even when the hard limit reads "unlimited".
#[cfg(unix)]
const SOFT_NOFILE_CEILING: libc::rlim_t = 10_240;

/// Best effort: returns the (old, new) soft limit when it was raised.
#[cfg(unix)]
pub fn raise_nofile_soft_limit() -> Option<(u64, u64)> {
    let mut lim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: getrlimit writes into a valid, owned rlimit struct.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) } != 0 {
        return None;
    }
    let target = lim.rlim_max.min(SOFT_NOFILE_CEILING);
    if lim.rlim_cur >= target {
        return None;
    }
    let old = lim.rlim_cur;
    lim.rlim_cur = target;
    // SAFETY: setrlimit reads a valid rlimit struct; only the soft limit changes.
    if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &lim) } != 0 {
        return None;
    }
    Some((old, target))
}

#[cfg(not(unix))]
pub fn raise_nofile_soft_limit() -> Option<(u64, u64)> {
    None
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn soft_limit_is_at_least_target_after_raise() {
        let _ = raise_nofile_soft_limit();
        let mut lim = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        assert_eq!(unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) }, 0);
        assert!(lim.rlim_cur >= lim.rlim_max.min(SOFT_NOFILE_CEILING));
        // Idempotent: a second call has nothing to raise.
        assert_eq!(raise_nofile_soft_limit(), None);
    }
}

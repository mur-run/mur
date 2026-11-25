//! Named pause points for tests that need a process stopped at a known place
//! (the fleet shared-area lifecycle interleavings: a purge paused after its
//! rename, a launch paused after it opened the sentinel, ...).
//!
//! With the cargo feature `test-pause`, [`pause_point`] blocks while the file
//! `$MUR_TEST_PAUSE_DIR/<name>` exists. Without it the function is an empty
//! inline and the variable is never read, so a shipped binary cannot be
//! stalled by its environment while it holds the lifecycle lock. The feature
//! is off by default and enabled by no release profile; the CI `test-pause`
//! job builds with it, and `scripts/check-no-test-pause.sh` fails the release
//! if a shipped binary carries [`BUILD_MARKER`].

/// Directory holding one file per pause point that should block.
pub const PAUSE_DIR_ENV: &str = "MUR_TEST_PAUSE_DIR";

/// Bytes embedded in any binary built with the `test-pause` feature (and only
/// then). `scripts/check-no-test-pause.sh` searches shipped binaries for them.
pub const BUILD_MARKER: &[u8] = b"MUR_BUILT_WITH_TEST_PAUSE_FEATURE";

/// How often a blocked pause point re-checks its file.
#[cfg(feature = "test-pause")]
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(10);

/// `pause_point` reads it, so any binary that calls a pause point carries the
/// marker (`#[used]` alone does not survive the linker).
#[cfg(feature = "test-pause")]
#[used]
static EMBEDDED_MARKER: [u8; BUILD_MARKER.len()] = {
    let mut out = [0u8; BUILD_MARKER.len()];
    let mut i = 0;
    while i < out.len() {
        out[i] = BUILD_MARKER[i];
        i += 1;
    }
    out
};

/// A pause name is one plain file name: it must not be able to leave the pause
/// directory or name a hidden file.
#[cfg(feature = "test-pause")]
fn valid_pause_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// Block while `$MUR_TEST_PAUSE_DIR/<name>` exists. Returns at once when the
/// variable is unset, the name is not a plain file name, or the file is absent.
#[cfg(feature = "test-pause")]
pub fn pause_point(name: &str) {
    std::hint::black_box(&EMBEDDED_MARKER);
    let Some(dir) = std::env::var_os(PAUSE_DIR_ENV) else {
        return;
    };
    if !valid_pause_name(name) {
        return;
    }
    let gate = std::path::Path::new(&dir).join(name);
    while gate.exists() {
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Without the `test-pause` feature a pause point is nothing: it does not read
/// the environment and cannot block.
#[cfg(not(feature = "test-pause"))]
#[inline(always)]
pub fn pause_point(_name: &str) {}

#[cfg(test)]
mod tests;

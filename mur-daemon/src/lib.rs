//! Library surface of `mur-daemon`, so integration tests can reach modules
//! the `murmurd` binary wires up. Keep this to modules with a tested public
//! API; everything else stays private to the binary.

/// Unix-only, like `mur-git-broker` itself.
#[cfg(unix)]
pub mod git_push_broker;

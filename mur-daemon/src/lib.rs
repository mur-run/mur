//! Library surface of `mur-daemon`, so integration tests can reach modules
//! the `murmurd` binary wires up. Keep this to modules with a tested public
//! API; everything else stays private to the binary.

/// Unix-only, like `mur-git-broker` itself.
#[cfg(unix)]
pub mod git_push_broker;

/// #2116 cross-router fleet dispatch. In the library so the #2139
/// end-to-end test can drive one real sweep against a sealed agent's request.
pub mod fleet_dispatch;

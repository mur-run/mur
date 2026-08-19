use super::SandboxPolicy;
use std::io;
use std::process::{Child, Command};

/// Spawn `cmd` under the given sandbox policy.
///
/// On Linux and macOS the function builds a `birdcage::Birdcage` cage and
/// registers all policy exceptions (ExecuteAndRead, Read, WriteAndRead,
/// Networking, FullEnvironment).  The actual spawn, however, falls back to
/// `cmd.spawn()` for two reasons:
///
/// 1. **Linux**: `Sandbox::spawn()` asserts the calling process is
///    single-threaded.  The supervisor runs a multi-threaded tokio runtime,
///    so the assert would fire.  Children inherit the supervisor's own
///    Landlock / seccomp restrictions (B1 Tasks 2–3).
///
/// 2. **macOS**: `cage.spawn` would call `sandbox_init()` on the *calling*
///    process, re-initialising the policy `sandbox::apply` already applied to
///    the supervisor — undefined behaviour. So the spawn is a plain
///    `cmd.spawn()`, and the child is confined by INHERITANCE.
///
/// Children are confined on both platforms, by the supervisor's own policy:
/// Landlock/seccomp are inherited on Linux, and a macOS seatbelt sandbox is
/// inherited across `fork` + `exec` (the mechanism `sandbox-exec(1)` is built
/// on — it sandboxes itself, then execs). Verified empirically:
///
/// ```text
/// $ sandbox-exec -p '(version 1)(allow default)(deny network-outbound)' \
///     /bin/sh -c 'curl -s -m5 -o /dev/null -w "%{http_code}" https://example.com; echo'
/// 000          # blocked, through sh's fork AND curl's exec
/// $ /bin/sh -c 'curl ... ; echo'
/// 200          # same shape, no sandbox
/// ```
///
/// Ordering makes this hold here: `sandbox::apply` seals the supervisor
/// (`supervisor.rs`) before the MCP pool is built, and the pool spawns lazily
/// on first tool use — every MCP server starts after the seal.
///
/// **What is actually missing is per-child policy.** A child cannot be given a
/// NARROWER cage than the agent itself, because that needs a second
/// `sandbox_init` in the child — hence the pre-fork single-threaded launcher
/// tracked as a follow-up, after which `cage.spawn(birdcage_cmd)` below can be
/// activated. Until then the granularity is per-agent, not per-server; the
/// confinement itself is real.
///
/// An earlier version of this doc said macOS children were "unconfined". That
/// was wrong, and the error escaped into `mur agent perm show`, `mur agent
/// doctor` and `docs/architecture/mcp-supply-chain.md` before the empirical
/// check above was run. Do not restate it without re-running that check.
pub fn spawn_sandboxed(cmd: Command, policy: &SandboxPolicy) -> io::Result<Child> {
    spawn_impl(cmd, policy)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn spawn_impl(mut cmd: Command, policy: &SandboxPolicy) -> io::Result<Child> {
    use birdcage::{Birdcage, Exception, Sandbox};

    let mut cage = Birdcage::new();
    for path in &policy.fs_exec {
        let _ = cage.add_exception(Exception::ExecuteAndRead(path.clone()));
    }
    for path in &policy.fs_read {
        let _ = cage.add_exception(Exception::Read(path.clone()));
    }
    for path in &policy.fs_write {
        let _ = cage.add_exception(Exception::WriteAndRead(path.clone()));
    }
    if policy.net_allow_ports.is_some() || policy.net_allow_hosts.is_some() {
        let _ = cage.add_exception(Exception::Networking);
    }
    let _ = cage.add_exception(Exception::FullEnvironment);

    // cage.spawn(birdcage_cmd) would enforce the policy above, but requires
    // a dedicated single-threaded pre-fork process (see module docs).
    // For now the cage is built to document intent.
    //
    // The child IS confined — by inheritance, on both platforms: Landlock and
    // seccomp on Linux, and a macOS seatbelt sandbox across fork+exec (see the
    // module docs for the empirical check). What the dropped cage would have
    // added is a NARROWER, per-child policy, which needs the pre-fork launcher.
    drop(cage);
    cmd.spawn()
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn spawn_impl(mut cmd: Command, _policy: &SandboxPolicy) -> io::Result<Child> {
    cmd.spawn()
}

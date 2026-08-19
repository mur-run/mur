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
/// 2. **macOS**: SBPL (`sandbox_init`/`sandbox_init_with_parameters`) is NOT
///    inherited across `exec`.  Unlike Landlock on Linux, the SBPL policy
///    applied to the supervisor is not propagated to spawned children.
///    **macOS children spawned here are therefore unconfined.**  `cage.spawn`
///    is not used because it calls `sandbox_init()` on the *calling* process
///    (re-initialising the already-applied supervisor policy, which is
///    undefined behaviour).  A dedicated pre-fork single-threaded launcher
///    subprocess is required to confine macOS children at spawn time; that
///    work is tracked as a follow-up.
///
/// When the supervisor adopts a pre-fork single-threaded launcher subprocess
/// the `cage.spawn(birdcage_cmd)` call below can be activated.
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
    // What actually confines the child differs by platform, and the
    // difference is NOT cosmetic:
    //
    // - Linux: Landlock and seccomp ARE inherited across `exec`, so the
    //   child really does run under the supervisor's policy.
    // - macOS: SBPL is NOT inherited across `exec`. The child runs with the
    //   user's full privileges — this policy does not reach it at all.
    //
    // An earlier version of this comment claimed "the supervisor's inherited
    // restrictions (Landlock/SBPL) cover the child at the kernel level",
    // which is true on Linux and false on macOS. Sitting three lines from
    // `drop(cage)`, it was exactly where a reader stops and concludes this is
    // fine. Stating both halves is the point.
    drop(cage);
    cmd.spawn()
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn spawn_impl(mut cmd: Command, _policy: &SandboxPolicy) -> io::Result<Child> {
    cmd.spawn()
}

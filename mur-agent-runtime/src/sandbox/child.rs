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
/// 2. **macOS**: `Sandbox::spawn()` calls `sandbox_init()` which applies
///    the SBPL to the *calling* process (not only the child).  The
///    supervisor has already applied its own SBPL via `sandbox_init_with_
///    parameters` (B1 Task 3).  A second `sandbox_init` call is undefined
///    behaviour and typically returns an error.
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
    // For now the cage is built to document intent; the supervisor's
    // inherited restrictions (Landlock/SBPL) cover the child at the kernel level.
    drop(cage);
    cmd.spawn()
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn spawn_impl(mut cmd: Command, _policy: &SandboxPolicy) -> io::Result<Child> {
    cmd.spawn()
}

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
/// `sandbox_init` in the child. The follow-up was recorded as "a pre-fork
/// single-threaded launcher", after which `cage.spawn(birdcage_cmd)` below
/// could be activated.
///
/// **On macOS that launcher cannot work, and the reason is the kernel's, not
/// the runtime's.** A second `sandbox_apply` inside an already-sandboxed
/// process is refused outright as soon as either policy restricts anything.
/// Measured 2026-09-17, the same `sandbox-exec` shape the empirical check
/// above uses:
///
/// ```text
/// # outer permissive, inner restricts a write
/// $ sandbox-exec -p '(version 1)(allow default)' /bin/sh -c \
///     'sandbox-exec -p "(version 1)(allow default)(deny file-write* (subpath \"/tmp/x\"))" true'
/// sandbox-exec: sandbox_apply: Operation not permitted
///
/// # outer restricts the network, inner permissive
/// $ sandbox-exec -p '(version 1)(allow default)(deny network-outbound)' /bin/sh -c \
///     'sandbox-exec -p "(version 1)(allow default)" true'
/// sandbox-exec: sandbox_apply: Operation not permitted
///
/// # both fully permissive — the only nesting that is allowed, and it
/// # restricts nothing, so it buys nothing
/// $ sandbox-exec -p '(version 1)(allow default)' /bin/sh -c \
///     'sandbox-exec -p "(version 1)(allow default)" /bin/echo ok'
/// ok
/// ```
///
/// Single-threadedness was never the macOS obstacle; nesting is. A launcher
/// would be a fresh process, and a fresh process inside this sandbox is still
/// refused a second apply. So per-child narrowing on macOS needs a different
/// mechanism entirely — confinement applied by an ancestor that was never
/// sealed, or a primitive other than Seatbelt — and not a launcher.
///
/// **Linux does differ, and it was measured.** On a 5.15 kernel with
/// `landlock` in `/sys/kernel/security/lsm`, a process that has already
/// called `landlock_restrict_self` may call it again, and the second call is
/// accepted:
///
/// ```text
/// layer 1: allow writes only under /tmp/nest/a
///     write /tmp/nest/a/one              ALLOWED
///     write /tmp/nest/b/one              Permission denied
/// layer 2 (nested): allow writes only under /tmp/nest/b
///   layer 2 apply: ACCEPTED
///     write /tmp/nest/a/two              Permission denied
///     write /tmp/nest/b/two              Permission denied
/// ```
///
/// So the single-threaded pre-fork launcher IS the answer on Linux, and the
/// asymmetry with macOS is real: there the second apply is refused outright,
/// here it is accepted.
///
/// One property of that acceptance shapes what a launcher may do. Stacking
/// **intersects**, it does not replace: after layer 2 neither directory is
/// writable, because each layer forbids what the other granted. A child's
/// policy can therefore only ever be a subset of its parent's — a launcher
/// cannot grant a child something the agent itself lacks. That is exactly
/// what the CLI-spawn shaping wants ("tighter than the agent, not equal to
/// it"), but it is a constraint rather than a coincidence, and a design that
/// assumed replacement would be wrong here.
///
/// The probe used raw syscalls: that kernel ships no `linux/landlock.h`.
/// Getting the access bits right mattered — the first run used `1ULL << 3`
/// for `WRITE_FILE` (that is `READ_DIR`) and omitted `MAKE_REG`, so layer 1
/// restricted nothing and every write was allowed. A stacking result read off
/// that run would have measured nothing.
///
/// Until then the granularity is per-agent, not per-server; the confinement
/// itself is real.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::policy::SandboxPolicy;
    use std::path::PathBuf;

    /// The sandbox actually refuses a write it was not granted.
    ///
    /// Every other test in this subsystem asserts the *profile text* — that
    /// the generated SBPL says `deny`. None of them run anything. That is the
    /// gap the CLI-spawn design names when it insists a sandbox be "verified,
    /// not merely applied": a policy that is constructed and never exercised
    /// is the same class of claim as `agy --sandbox`, which read like a
    /// boundary and, measured, restricted nothing.
    ///
    /// So this one spawns a real process and looks at what happened on disk.
    ///
    /// **It fails today, which is why it is ignored.** `spawn_impl` above
    /// builds the `birdcage` exceptions from the policy and then drops the
    /// cage: the enforcing form needs a single-threaded pre-fork process that
    /// does not exist yet. A child is confined by *inheriting* the parent's
    /// sandbox, which is real, but it is the runtime's own policy rather than
    /// a narrower one chosen per child.
    ///
    /// This is the acceptance test for that narrowing. It should start
    /// passing the day the pre-fork launcher lands, and it is deliberately
    /// left as a failing check rather than deleted or inverted — a test
    /// asserting "the sandbox does not confine" would be read one day as a
    /// statement of intent.
    ///
    /// Run it with `cargo test -p mur-agent-runtime -- --ignored`.
    #[test]
    #[ignore = "per-child enforcement is unwired: spawn_impl drops the cage"]
    fn a_write_outside_the_grant_does_not_land() {
        let tmp = std::env::temp_dir().join(format!("mur-sbx-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).expect("tmpdir");
        let granted = tmp.join("granted");
        let denied = tmp.join("denied");
        std::fs::create_dir_all(&granted).expect("granted dir");
        // BOTH directories exist. Without this the denied write fails with
        // ENOENT whether or not a sandbox is present, and the test passes
        // while measuring nothing — checked by spawning unsandboxed, which
        // must make it fail.
        std::fs::create_dir_all(&denied).expect("denied dir");

        let policy = SandboxPolicy {
            // Only the granted subdirectory is writable. `denied` is a
            // sibling, so nothing about it is covered.
            fs_write: vec![granted.clone()],
            fs_read: vec![tmp.clone(), PathBuf::from("/usr"), PathBuf::from("/bin")],
            fs_exec: vec![PathBuf::from("/bin")],
            ..Default::default()
        };

        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg(format!(
            "echo ok > {} ; echo nope > {}",
            granted.join("f").display(),
            denied.join("f").display()
        ));
        let mut child = match spawn_sandboxed(cmd, &policy) {
            Ok(c) => c,
            // A platform without an implementation must not silently pass.
            Err(e) => panic!("spawn_sandboxed failed: {e}"),
        };
        let _ = child.wait();

        assert!(
            granted.join("f").exists(),
            "the granted write did not land — the policy is denying everything, \
             which would make the assertion below vacuous"
        );
        assert!(
            !denied.join("f").exists(),
            "the sandbox allowed a write it never granted: {}",
            denied.join("f").display()
        );
        std::fs::remove_dir_all(&tmp).ok();
    }
}

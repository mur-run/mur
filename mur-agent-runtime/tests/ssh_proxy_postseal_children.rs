//! ssh-agent proxy, option A, Verifications 2 and 3 — what a child of the
//! sealed supervisor can reach of the supervisor's proxy descriptors.
//!
//! The test binary re-executes itself as a stand-in supervisor: it dials a
//! fake upstream before the seal (the held stream), seals with the real
//! `apply_macos`, then binds the proxy listener after the seal (as the design
//! does). From there:
//!
//! - **2** — a confined child (this binary again, exec-allowed by exact
//!   path-literal) tries `task_for_pid` on the supervisor (the route to its
//!   fds) and `proc_pidfdinfo` on the held fd (metadata only). The same child
//!   is also run *before* the seal as a control, so the record says whether
//!   the seal or the OS is what blocks it. Asserted: `task_for_pid` fails
//!   after the seal. `proc_pidfdinfo` is recorded, not asserted — it leaks
//!   socket metadata, never the descriptor.
//! - **3** — a `/bin/bash` child (the bash tool's shape) reports whether the
//!   held stream and the listener are open in it. A deliberately inheritable
//!   dup of the held stream is the harness control: bash must see *that* one,
//!   or "closed" would mean nothing.
//!
//! Skips loudly from a sealed shell (#1697); fails under CI.
#![cfg(target_os = "macos")]

use mur_agent_runtime::sandbox::SandboxPolicy;
use mur_agent_runtime::sandbox::macos::apply_macos;
use std::os::fd::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

/// Role of a re-executed copy of this binary.
const ROLE_ENV: &str = "MUR_TEST_SSH_POSTSEAL_ROLE";
const ROLE_SUPERVISOR: &str = "supervisor";
const ROLE_PEER: &str = "peer";
const UPSTREAM_ENV: &str = "MUR_TEST_SSH_POSTSEAL_UPSTREAM";
const HOME_ENV: &str = "MUR_TEST_SSH_POSTSEAL_HOME";
const VARIANT_ENV: &str = "MUR_TEST_SSH_POSTSEAL_VARIANT";
/// Peer inputs: supervisor pid and the held fd number.
const TARGET_PID_ENV: &str = "MUR_TEST_SSH_POSTSEAL_TARGET_PID";
const TARGET_FD_ENV: &str = "MUR_TEST_SSH_POSTSEAL_TARGET_FD";
/// Bash inputs: fd numbers to probe.
const FD_HELD_ENV: &str = "HELD_FD";
const FD_LISTENER_ENV: &str = "LISTENER_FD";
const FD_CONTROL_ENV: &str = "CONTROL_FD";

const SUPERVISOR_PREFIX: &str = "POSTSEAL-RESULT";
const PEER_PREFIX: &str = "POSTSEAL-PEER";
const CHILD_TEST: &str = "child_entry";
/// Proxy listener file name inside the stand-in agent home.
const LISTENER_NAME: &str = "ssh-proxy.sock";
const BASH: &str = "/bin/bash";
/// `<sys/proc_info.h>`: PROC_PIDFDSOCKETINFO (not exported by `libc`).
const PROC_PIDFDSOCKETINFO: libc::c_int = 3;
/// Larger than `struct socket_fdinfo` on every macOS release.
const FDINFO_BUF: usize = 4096;
const CI_ENV_VAR: &str = "CI";
/// Same reason as `ssh_bypass_probe.rs`: stay out of the "true Off" arm.
const LOOPBACK_PORT: u16 = 1;
/// Lets the upstream thread register accepts before the parent reads them.
const SETTLE: Duration = Duration::from_millis(100);

/// Prints, for each named fd, `open:<type>` or `closed`. `stat -f %HT` on
/// `/dev/fd/N` names the type ("Socket"), so a reused low fd number of
/// bash's own would not read as our socket.
const BASH_SCRIPT: &str = r#"
probe() { if [ -e "/dev/fd/$1" ]; then printf 'open:%s' "$(/usr/bin/stat -Lf %HT "/dev/fd/$1" 2>/dev/null | tr -d ' ')"; else printf closed; fi; }
echo "held=$(probe "$HELD_FD") listener=$(probe "$LISTENER_FD") control=$(probe "$CONTROL_FD")"
"#;

fn in_ci() -> bool {
    std::env::var_os(CI_ENV_VAR).is_some_and(|v| !v.is_empty() && v != "false" && v != "0")
}

fn sealed(test: &str) -> bool {
    let sealed = mur_agent_runtime::sandbox::current_process_sealed();
    if sealed {
        assert!(
            !in_ci(),
            "{test}: process is sandboxed under CI; runners are expected unsealed (#1697)"
        );
        eprintln!("SKIP {test}: already sandboxed, the child cannot seal itself (#1697)");
    }
    sealed
}

#[derive(Clone, Copy, Debug)]
enum Variant {
    Restricted,
    RestrictedWithEgressProxy,
}

impl Variant {
    fn name(self) -> &'static str {
        match self {
            Self::Restricted => "restricted",
            Self::RestrictedWithEgressProxy => "restricted_egress_proxy",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        [Self::Restricted, Self::RestrictedWithEgressProxy]
            .into_iter()
            .find(|v| v.name() == s)
    }

    /// The agent's restricted seal, plus: the stand-in home is writable (the
    /// real agent home always is) and this binary is exec-allowed by exact
    /// path so it can play the confined peer for Verification 2.
    fn policy(self, home: PathBuf, exe: PathBuf) -> SandboxPolicy {
        let mut p = SandboxPolicy {
            net_allow_ports: Some(vec![]),
            net_allow_loopback_ports: vec![LOOPBACK_PORT],
            net_loopback_allowed: true,
            fs_write: vec![home],
            spawn_allowed_paths: vec![exe],
            ..Default::default()
        };
        if matches!(self, Self::RestrictedWithEgressProxy) {
            p.allow_in_process_proxy_upstream();
        }
        p
    }
}

fn errno() -> String {
    let e = std::io::Error::last_os_error();
    match e.raw_os_error() {
        Some(libc::EPERM) => "EPERM".into(),
        Some(libc::EACCES) => "EACCES".into(),
        Some(libc::ESRCH) => "ESRCH".into(),
        Some(n) => format!("errno{n}"),
        None => "unknown".into(),
    }
}

/// Peer role: try to reach the supervisor's held fd. Prints one line.
fn peer() {
    let pid: libc::pid_t = std::env::var(TARGET_PID_ENV).unwrap().parse().unwrap();
    let fd: libc::c_int = std::env::var(TARGET_FD_ENV).unwrap().parse().unwrap();

    let mut port: libc::mach_port_t = 0;
    // SAFETY: plain FFI with a valid out-pointer.
    #[allow(deprecated)]
    let kr = unsafe { libc::task_for_pid(libc::mach_task_self(), pid, &mut port) };
    let task = if kr == libc::KERN_SUCCESS {
        "granted".to_owned()
    } else {
        format!("denied:kr{kr}")
    };

    let mut buf = vec![0u8; FDINFO_BUF];
    // SAFETY: buffer is valid for `FDINFO_BUF` bytes.
    let n = unsafe {
        libc::proc_pidfdinfo(
            pid,
            fd,
            PROC_PIDFDSOCKETINFO,
            buf.as_mut_ptr().cast(),
            FDINFO_BUF as libc::c_int,
        )
    };
    let info = if n > 0 {
        format!("readable:{n}")
    } else {
        format!("denied:{}", errno())
    };
    println!("{PEER_PREFIX} task_for_pid={task} pidfdinfo={info}");
}

/// Runs this binary as the peer against `pid`/`fd`; returns its result line.
fn spawn_peer(exe: &PathBuf, fd: libc::c_int) -> String {
    let out = Command::new(exe)
        .args([CHILD_TEST, "--exact", "--nocapture", "--test-threads=1"])
        .env(ROLE_ENV, ROLE_PEER)
        .env(TARGET_PID_ENV, std::process::id().to_string())
        .env(TARGET_FD_ENV, fd.to_string())
        .output();
    let out = match out {
        Ok(o) => o,
        Err(e) => return format!("spawn_failed:{}", e.raw_os_error().unwrap_or(0)),
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.split_once(PEER_PREFIX).map(|(_, r)| r.trim().to_owned()))
        .unwrap_or_else(|| format!("no_output:{:?}", out.status))
}

/// Pulls `key=value` out of a peer line, keyed under `prefix`.
fn tag(line: &str, prefix: &str) -> String {
    line.split_whitespace()
        .map(|t| format!("{prefix}_{t}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Supervisor role: dial, seal, bind, then measure 2 and 3.
fn supervisor() {
    let upstream = PathBuf::from(std::env::var(UPSTREAM_ENV).unwrap());
    let home = PathBuf::from(std::env::var(HOME_ENV).unwrap());
    let variant = Variant::parse(&std::env::var(VARIANT_ENV).unwrap()).unwrap();
    let exe = std::fs::canonicalize(std::env::current_exe().unwrap()).unwrap();

    // Pre-seal dial, as `prepare_and_seal` would.
    let held = UnixStream::connect(&upstream).expect("pre-seal connect");
    let held_fd = held.as_raw_fd();

    // Harness control for 3: an inheritable dup (no FD_CLOEXEC).
    // SAFETY: duplicating an fd we own.
    let control_fd = unsafe { libc::fcntl(held_fd, libc::F_DUPFD, 0) };
    assert!(control_fd >= 0, "dup control fd: {}", errno());

    let before = spawn_peer(&exe, held_fd);

    let status = apply_macos(&variant.policy(home.clone(), exe.clone())).expect("apply_macos");
    let enforcing = status.enforcing && mur_agent_runtime::sandbox::current_process_sealed();

    // Post-seal bind in the agent home, as the design does.
    let listener = UnixListener::bind(home.join(LISTENER_NAME)).expect("post-seal bind");
    let listener_fd = listener.as_raw_fd();

    let after = spawn_peer(&exe, held_fd);

    let bash = Command::new(BASH)
        .arg("-c")
        .arg(BASH_SCRIPT)
        .env(FD_HELD_ENV, held_fd.to_string())
        .env(FD_LISTENER_ENV, listener_fd.to_string())
        .env(FD_CONTROL_ENV, control_fd.to_string())
        .output()
        .expect("spawn bash");
    let bash_line = String::from_utf8_lossy(&bash.stdout).trim().to_owned();

    println!(
        "{SUPERVISOR_PREFIX} enforcing={enforcing} {} {} {bash_line}",
        tag(&before, "unsealed"),
        tag(&after, "sealed")
    );
    drop((held, listener));
}

/// Re-exec target. A no-op unless the parent set `ROLE_ENV`.
#[test]
fn child_entry() {
    match std::env::var(ROLE_ENV).as_deref() {
        Ok(ROLE_SUPERVISOR) => supervisor(),
        Ok(ROLE_PEER) => peer(),
        _ => {}
    }
}

fn run_supervisor(variant: Variant) -> String {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let up_dir = root.join("upstream");
    let home = root.join("home");
    std::fs::create_dir(&up_dir).unwrap();
    std::fs::create_dir(&home).unwrap();
    let sock = up_dir.join("agent.sock");
    let listener = UnixListener::bind(&sock).unwrap();
    std::thread::spawn(move || {
        // Hold accepted streams open for the life of the test.
        // Not `.collect()`: `Incoming::size_hint` is `(usize::MAX, None)`, so
        // collecting overflows capacity right after the first accept.
        let mut conns = Vec::new();
        for conn in listener.incoming() {
            conns.push(conn);
        }
    });

    let out = Command::new(std::env::current_exe().unwrap())
        .args([CHILD_TEST, "--exact", "--nocapture", "--test-threads=1"])
        .env(ROLE_ENV, ROLE_SUPERVISOR)
        .env(UPSTREAM_ENV, &sock)
        .env(HOME_ENV, &home)
        .env(VARIANT_ENV, variant.name())
        .output()
        .unwrap();
    std::thread::sleep(SETTLE);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout
        .lines()
        .find_map(|l| {
            l.split_once(SUPERVISOR_PREFIX)
                .map(|(_, r)| r.trim().to_owned())
        })
        .unwrap_or_else(|| {
            panic!(
                "{variant:?}: supervisor printed no result; status={:?}\nstdout:\n{stdout}\nstderr:\n{}",
                out.status,
                String::from_utf8_lossy(&out.stderr)
            )
        });
    eprintln!("O7 verification 2+3 [{}]: {line}", variant.name());
    line
}

fn field(line: &str, key: &str) -> String {
    line.split_whitespace()
        .find_map(|t| t.strip_prefix(&format!("{key}=")).map(str::to_owned))
        .unwrap_or_default()
}

fn assert_children_cannot_reach(variant: Variant, test: &str) {
    if sealed(test) {
        return;
    }
    let line = run_supervisor(variant);
    assert_eq!(
        field(&line, "enforcing"),
        "true",
        "{variant:?}: seal not enforcing; nothing measured"
    );

    // 3 — harness first: the inheritable control must be visible.
    assert!(
        field(&line, "control").starts_with("open"),
        "{variant:?}: bash did not see the inheritable control fd; 3 is not measuring"
    );
    assert_eq!(
        field(&line, "held"),
        "closed",
        "{variant:?}: 3 — held upstream stream leaked into bash"
    );
    assert_eq!(
        field(&line, "listener"),
        "closed",
        "{variant:?}: 3 — proxy listener leaked into bash"
    );

    // 2 — the descriptor route. Metadata (pidfdinfo) is recorded only.
    let sealed_task = field(&line, "sealed_task_for_pid");
    assert!(
        sealed_task.starts_with("denied"),
        "{variant:?}: 2 — confined child got the supervisor's task port ({sealed_task})"
    );
}

#[test]
fn verification_2_3_restricted() {
    assert_children_cannot_reach(Variant::Restricted, "verification_2_3_restricted");
}

#[test]
fn verification_2_3_restricted_with_egress_proxy() {
    assert_children_cannot_reach(
        Variant::RestrictedWithEgressProxy,
        "verification_2_3_restricted_with_egress_proxy",
    );
}

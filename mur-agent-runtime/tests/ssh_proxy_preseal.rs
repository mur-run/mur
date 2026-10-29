//! ssh-agent proxy, option A, Verification 1 (spec: "macOS: a pre-seal
//! connection survives the seal"). Two conditions, recorded separately:
//!
//! - **1a** — on a stream connected *before* `sandbox_init`, a `write` of an
//!   agent-protocol request and a `read` of its reply succeed after the seal.
//! - **1b** — after the seal, a *new* `connect()` to the same upstream fails
//!   with `EPERM`/`EACCES`, and the upstream never accepts it.
//!
//! A holds only if both pass. 1a pass + 1b fail is a sandbox hole, not a
//! to-do, so each is asserted on its own and never folded into one verdict.
//!
//! The test binary re-executes itself as the child, because the seal cannot be
//! undone and must not land on the test harness. Like `ssh_bypass_probe.rs`,
//! it skips loudly from a sealed shell (#1697) and fails under CI.
#![cfg(target_os = "macos")]

use mur_agent_runtime::sandbox::SandboxPolicy;
use mur_agent_runtime::sandbox::macos::{apply_macos, build_sbpl_profile};
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// Child-mode switch: when set, `child_entry` runs the measurement on itself.
const CHILD_ENV: &str = "MUR_TEST_SSH_PRESEAL_CHILD";
/// Upstream socket path handed to the child.
const UPSTREAM_ENV: &str = "MUR_TEST_SSH_PRESEAL_UPSTREAM";
/// Which profile variant the child seals itself with.
const VARIANT_ENV: &str = "MUR_TEST_SSH_PRESEAL_VARIANT";
/// Prefix of the child's single result line on stdout.
const RESULT_PREFIX: &str = "PRESEAL-RESULT";
/// Libtest name of the child entry point (re-exec target).
const CHILD_TEST: &str = "child_entry";

/// ssh-agent protocol (draft-miller-ssh-agent): request identities / answer.
const SSH_AGENTC_REQUEST_IDENTITIES: u8 = 11;
const SSH_AGENT_IDENTITIES_ANSWER: u8 = 12;

/// Bounds every socket read/write so a hung stream fails the item instead
/// of the test.
const IO_TIMEOUT: Duration = Duration::from_secs(3);

/// Same convention as `ssh_bypass_probe.rs`.
const CI_ENV_VAR: &str = "CI";
/// Arbitrary loopback port; keeps the profile out of the "true Off" arm, which
/// emits no unix-socket allow rules (same reason as `ssh_bypass_probe.rs`).
const LOOPBACK_PORT: u16 = 1;

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

/// The profile shapes the supervisor actually seals with.
#[derive(Clone, Copy, Debug)]
enum Variant {
    /// Restricted agent, no in-process egress proxy.
    Restricted,
    /// Restricted agent whose self profile is widened for the in-process
    /// egress proxy (`allow_in_process_proxy_upstream`) — the supervisor's
    /// real seal when MCP children use the loopback proxy.
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

    fn policy(self) -> SandboxPolicy {
        let mut p = SandboxPolicy {
            net_allow_ports: Some(vec![]),
            net_allow_loopback_ports: vec![LOOPBACK_PORT],
            net_loopback_allowed: true,
            ..Default::default()
        };
        if matches!(self, Self::RestrictedWithEgressProxy) {
            p.allow_in_process_proxy_upstream();
        }
        p
    }
}

/// One ssh-agent frame: u32 big-endian length, then the body.
fn frame(body: &[u8]) -> Vec<u8> {
    let mut v = (body.len() as u32).to_be_bytes().to_vec();
    v.extend_from_slice(body);
    v
}

fn read_frame(s: &mut UnixStream) -> std::io::Result<Vec<u8>> {
    let mut len = [0u8; 4];
    s.read_exact(&mut len)?;
    let mut body = vec![0u8; u32::from_be_bytes(len) as usize];
    s.read_exact(&mut body)?;
    Ok(body)
}

/// Fake upstream: answers every REQUEST_IDENTITIES with an empty list and
/// counts accepted connections, so 1b is checked from both sides.
fn fake_upstream() -> (tempfile::TempDir, PathBuf, Arc<AtomicUsize>) {
    let dir = tempfile::tempdir().unwrap();
    // The kernel matches the resolved path; /var -> /private/var on macOS.
    let sock = std::fs::canonicalize(dir.path())
        .unwrap()
        .join("agent.sock");
    let listener = UnixListener::bind(&sock).unwrap();
    let accepted = Arc::new(AtomicUsize::new(0));
    let count = accepted.clone();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut s) = conn else { continue };
            count.fetch_add(1, Ordering::SeqCst);
            std::thread::spawn(move || {
                while let Ok(req) = read_frame(&mut s) {
                    if req.first() == Some(&SSH_AGENTC_REQUEST_IDENTITIES) {
                        let mut ans = vec![SSH_AGENT_IDENTITIES_ANSWER];
                        ans.extend_from_slice(&0u32.to_be_bytes());
                        if s.write_all(&frame(&ans)).is_err() {
                            break;
                        }
                    }
                }
            });
        }
    });
    (dir, sock, accepted)
}

fn errno_str(e: &std::io::Error) -> String {
    match e.raw_os_error() {
        Some(libc::EPERM) => "EPERM".into(),
        Some(libc::EACCES) => "EACCES".into(),
        Some(n) => format!("errno{n}"),
        None => format!("{:?}", e.kind()),
    }
}

/// 1a on the held stream: one request, one well-formed answer.
fn measure_1a(s: &mut UnixStream) -> String {
    let req = frame(&[SSH_AGENTC_REQUEST_IDENTITIES]);
    if let Err(e) = s.write_all(&req) {
        return format!("fail:write:{}", errno_str(&e));
    }
    match read_frame(s) {
        Ok(b) if b.first() == Some(&SSH_AGENT_IDENTITIES_ANSWER) => "pass".into(),
        Ok(b) => format!("fail:bad_reply:{b:?}"),
        Err(e) => format!("fail:read:{}", errno_str(&e)),
    }
}

/// 1b: a fresh connect must be refused with EPERM/EACCES.
fn measure_1b(upstream: &Path) -> String {
    match UnixStream::connect(upstream) {
        Ok(_) => "fail:connected".into(),
        Err(e) if matches!(e.raw_os_error(), Some(libc::EPERM | libc::EACCES)) => {
            format!("pass:{}", errno_str(&e))
        }
        Err(e) => format!("fail:{}", errno_str(&e)),
    }
}

/// Re-exec target. A no-op unless the parent set `CHILD_ENV`.
#[test]
fn child_entry() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    let upstream = PathBuf::from(std::env::var(UPSTREAM_ENV).unwrap());
    let variant = Variant::parse(&std::env::var(VARIANT_ENV).unwrap()).unwrap();

    // Pre-seal: the supervisor would dial the upstream here.
    let mut held = UnixStream::connect(&upstream).expect("pre-seal connect");
    held.set_read_timeout(Some(IO_TIMEOUT)).unwrap();
    held.set_write_timeout(Some(IO_TIMEOUT)).unwrap();

    let status = apply_macos(&variant.policy()).expect("apply_macos");
    let enforcing = status.enforcing && mur_agent_runtime::sandbox::current_process_sealed();

    let r1a = measure_1a(&mut held);
    let r1b = measure_1b(&upstream);
    println!("{RESULT_PREFIX} enforcing={enforcing} 1a={r1a} 1b={r1b}");
}

struct Measured {
    enforcing: bool,
    r1a: String,
    r1b: String,
    accepted: usize,
}

fn run_child(variant: Variant) -> Measured {
    let (_dir, sock, accepted) = fake_upstream();
    let profile = build_sbpl_profile(&variant.policy());
    // Guard the setup: the upstream must sit outside every unix-socket allow
    // rule, or a 1b "pass" would be measuring something else.
    assert!(
        !profile.contains(&sock.parent().unwrap().display().to_string()),
        "upstream dir is named in the profile; 1b would not measure the deny:\n{profile}"
    );
    let out = Command::new(std::env::current_exe().unwrap())
        .args([CHILD_TEST, "--exact", "--nocapture", "--test-threads=1"])
        .env(CHILD_ENV, "1")
        .env(UPSTREAM_ENV, &sock)
        .env(VARIANT_ENV, variant.name())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout
        .lines()
        .find_map(|l| {
            l.split_once(RESULT_PREFIX)
                .map(|(_, r)| r.trim().to_owned())
        })
        .unwrap_or_else(|| {
            panic!(
                "{variant:?}: child printed no result; status={:?}\nstdout:\n{stdout}\nstderr:\n{}",
                out.status,
                String::from_utf8_lossy(&out.stderr)
            )
        });
    let field = |k: &str| {
        line.split_whitespace()
            .find_map(|t| t.strip_prefix(&format!("{k}=")).map(str::to_owned))
            .unwrap_or_default()
    };
    // Let the server thread register a late accept, if 1b leaked one.
    std::thread::sleep(Duration::from_millis(100));
    let m = Measured {
        enforcing: field("enforcing") == "true",
        r1a: field("1a"),
        r1b: field("1b"),
        accepted: accepted.load(Ordering::SeqCst),
    };
    eprintln!(
        "O7 verification 1 [{}]: enforcing={} 1a={} 1b={} upstream_accepts={}",
        variant.name(),
        m.enforcing,
        m.r1a,
        m.r1b,
        m.accepted
    );
    m
}

fn assert_a_holds(variant: Variant, test: &str) {
    if sealed(test) {
        return;
    }
    let m = run_child(variant);
    assert!(
        m.enforcing,
        "{variant:?}: child seal not enforcing; nothing measured"
    );
    // Recorded separately: each has its own assertion and message.
    assert_eq!(
        m.r1a, "pass",
        "{variant:?}: 1a (held stream after seal) failed"
    );
    assert!(
        m.r1b.starts_with("pass"),
        "{variant:?}: 1b (new connect after seal) not denied: {} — sandbox finding",
        m.r1b
    );
    // Exactly one accept: the pre-seal dial. A second means 1b leaked.
    assert_eq!(
        m.accepted, 1,
        "{variant:?}: upstream accepted a post-seal connect"
    );
}

#[test]
fn verification_1_restricted() {
    assert_a_holds(Variant::Restricted, "verification_1_restricted");
}

#[test]
fn verification_1_restricted_with_egress_proxy() {
    assert_a_holds(
        Variant::RestrictedWithEgressProxy,
        "verification_1_restricted_with_egress_proxy",
    );
}

//! Kernel probe: does Landlock gate `connect()` to a pathname unix socket?
//!
//! The ssh-agent proxy design's bypass detection classifies every Linux
//! agent as audit-only on the premise that it does NOT: Landlock FS rights
//! cover opening the socket *file*, not connecting to it, and the scope
//! rights (ABI 6) cover only abstract sockets. This test pins that premise
//! on a real kernel. If it ever fails on the connect assertion, a kernel
//! started gating the connect and the Linux row of the detection table must
//! be revisited, not this test loosened.
//!
//! `restrict_self` is irreversible, so the sandboxed half runs in a child
//! re-exec of this test binary, selected by an env var.

use super::apply_linux;
use crate::sandbox::SandboxPolicy;
use std::io::Read;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::process::Command;

const CHILD_ENV: &str = "MUR_LANDLOCK_UNIX_PROBE_DIR";

const EXIT_CONNECT_OK: i32 = 10;
const EXIT_CONNECT_DENIED: i32 = 11;
const EXIT_NOT_ENFORCING: i32 = 12;
const EXIT_CONTROL_NOT_DENIED: i32 = 13;
const EXIT_SETUP_FAILED: i32 = 14;

const SOCKET_NAME: &str = "agent.sock";
const CONTROL_FILE: &str = "control.txt";

fn child(dir: &Path) -> ! {
    // Strictest shape a Linux agent gets: no FS grant covering `dir`, and
    // outbound TCP handled with no allowed port.
    let policy = SandboxPolicy {
        net_allow_ports: Some(Vec::new()),
        ..SandboxPolicy::default()
    };
    let status = match apply_linux(&policy) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("probe: apply_linux failed: {e:#}");
            std::process::exit(EXIT_SETUP_FAILED);
        }
    };
    if !status.enforcing {
        eprintln!("probe: Landlock not enforcing on this kernel");
        std::process::exit(EXIT_NOT_ENFORCING);
    }
    // Control: the sandbox must actually deny FS access to `dir`, or a
    // successful connect below would prove nothing.
    let mut buf = String::new();
    if std::fs::File::open(dir.join(CONTROL_FILE))
        .and_then(|mut f| f.read_to_string(&mut buf))
        .is_ok()
    {
        eprintln!("probe: control read was NOT denied");
        std::process::exit(EXIT_CONTROL_NOT_DENIED);
    }
    match UnixStream::connect(dir.join(SOCKET_NAME)) {
        Ok(_) => {
            eprintln!("probe: connect() to pathname unix socket SUCCEEDED");
            std::process::exit(EXIT_CONNECT_OK);
        }
        Err(e) => {
            eprintln!("probe: connect() denied: {e}");
            std::process::exit(EXIT_CONNECT_DENIED);
        }
    }
}

#[test]
fn landlock_does_not_gate_connect_to_a_pathname_unix_socket() {
    if let Some(dir) = std::env::var_os(CHILD_ENV) {
        child(Path::new(&dir));
    }

    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join(CONTROL_FILE), "x").unwrap();
    let _listener = UnixListener::bind(tmp.path().join(SOCKET_NAME)).unwrap();

    let name = concat!(
        module_path!(),
        "::landlock_does_not_gate_connect_to_a_pathname_unix_socket"
    );
    let name = name.split_once("::").map_or(name, |(_, rest)| rest);
    let out = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture", "--test-threads=1"])
        .env(CHILD_ENV, tmp.path())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let code = out.status.code();
    eprintln!("child exit={code:?}\n{stderr}");

    assert_ne!(
        code,
        Some(EXIT_NOT_ENFORCING),
        "kernel has no Landlock; probe proves nothing"
    );
    assert_ne!(
        code,
        Some(EXIT_CONTROL_NOT_DENIED),
        "FS denial control failed"
    );
    assert_ne!(code, Some(EXIT_SETUP_FAILED), "sandbox setup failed");
    assert_eq!(
        code,
        Some(EXIT_CONNECT_OK),
        "Landlock now gates pathname unix connect(); revisit the Linux row of the \
         ssh-agent proxy bypass-detection table"
    );
}

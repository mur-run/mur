//! #2343 on a real kernel: under `processes.spawn.mode: strict`, Landlock
//! must let an allowlisted binary run and refuse every other one, including
//! a binary inside a READABLE dir (`/usr`) and one inside a WRITABLE dir.
//! Landlock rights are additive, so a read or write rule that still carried
//! `Execute` would reopen those — which is what this pins.
//!
//! `restrict_self` is irreversible, so the sandboxed half runs in a child
//! re-exec of this test binary, selected by an env var (same shape as
//! `unix_connect_probe`).

use super::apply_linux;
use crate::sandbox::SandboxPolicy;
use mur_common::agent::{Entitlements, SpawnMode};
use std::path::{Path, PathBuf};
use std::process::Command;

const CHILD_ENV: &str = "MUR_LANDLOCK_STRICT_EXEC_DIR";

const EXIT_OK: i32 = 0;
const EXIT_NOT_ENFORCING: i32 = 20;
const EXIT_SETUP_FAILED: i32 = 21;
const EXIT_ALLOWED_DENIED: i32 = 22;
const EXIT_READABLE_RAN: i32 = 23;
const EXIT_WRITABLE_RAN: i32 = 24;
const EXIT_CONTROL_READ_DENIED: i32 = 25;

/// Allowlisted: must run. Unlisted, but under the `/usr` read grant: must not.
const ALLOWED: &str = "true";
const UNLISTED: &str = "id";
const COPIED: &str = "copied-binary";

fn system_binary(name: &str) -> PathBuf {
    ["/usr/bin", "/bin"]
        .iter()
        .map(|d| Path::new(d).join(name))
        .find(|p| p.exists())
        .unwrap_or_else(|| panic!("no {name} on this host"))
}

fn strict_policy(dir: &Path) -> SandboxPolicy {
    let yaml = format!(
        "network: {{inbound: {{ports: []}}, outbound: {{mode: unrestricted, allow_hosts: [], allow_ports: [], protocols: [tcp]}}}}\n\
         filesystem: {{read: [\"/usr\"], write: [\"{w}\"], deny: []}}\n\
         processes: {{spawn: {{mode: strict, allowed: [\"{ALLOWED}\"], allowed_dirs: []}}}}\n",
        w = dir.join("work").display(),
    );
    let ent: Entitlements = serde_yaml_ng::from_str(&yaml).expect("entitlements yaml");
    assert_eq!(ent.processes.spawn.mode, SpawnMode::Strict);
    SandboxPolicy::from_entitlements(&ent, &dir.join("agent"))
}

fn runs(path: &Path) -> bool {
    Command::new(path)
        .output()
        .is_ok_and(|o| o.status.success())
}

fn child(dir: &Path) -> ! {
    let policy = strict_policy(dir);
    let copied = dir.join("work").join(COPIED);
    let status = match apply_linux(&policy) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("probe: apply_linux failed: {e:#}");
            std::process::exit(EXIT_SETUP_FAILED);
        }
    };
    if !status.enforcing {
        std::process::exit(EXIT_NOT_ENFORCING);
    }
    // Control: the write grant is live (the file is readable), so a refused
    // exec below is the missing `Execute`, not a missing grant.
    if std::fs::read(&copied).is_err() {
        std::process::exit(EXIT_CONTROL_READ_DENIED);
    }
    if !runs(&system_binary(ALLOWED)) {
        std::process::exit(EXIT_ALLOWED_DENIED);
    }
    if runs(&system_binary(UNLISTED)) {
        std::process::exit(EXIT_READABLE_RAN);
    }
    if runs(&copied) {
        std::process::exit(EXIT_WRITABLE_RAN);
    }
    std::process::exit(EXIT_OK);
}

#[test]
fn strict_landlock_runs_only_allowlisted_binaries() {
    if let Some(dir) = std::env::var_os(CHILD_ENV) {
        child(Path::new(&dir));
    }

    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::create_dir_all(tmp.path().join("agent")).unwrap();
    std::fs::copy(system_binary(UNLISTED), work.join(COPIED)).unwrap();

    // Negative control outside the sandbox: both refused binaries do run
    // unsandboxed, so a refusal in the child is the sandbox's doing.
    assert!(runs(&system_binary(UNLISTED)));
    assert!(runs(&work.join(COPIED)));

    let name = concat!(
        module_path!(),
        "::strict_landlock_runs_only_allowlisted_binaries"
    );
    let name = name.split_once("::").map_or(name, |(_, rest)| rest);
    let out = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture", "--test-threads=1"])
        .env(CHILD_ENV, tmp.path())
        .output()
        .unwrap();
    let code = out.status.code();
    eprintln!(
        "child exit={code:?}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let why = match code {
        Some(EXIT_OK) => return,
        Some(EXIT_NOT_ENFORCING) => "kernel has no Landlock; probe proves nothing",
        Some(EXIT_SETUP_FAILED) => "sandbox setup failed",
        Some(EXIT_CONTROL_READ_DENIED) => "write-grant control failed",
        Some(EXIT_ALLOWED_DENIED) => "allowlisted binary was refused",
        Some(EXIT_READABLE_RAN) => "unlisted binary under a read grant ran",
        Some(EXIT_WRITABLE_RAN) => "binary under a write grant ran",
        _ => "child exited unexpectedly",
    };
    panic!("{why}");
}

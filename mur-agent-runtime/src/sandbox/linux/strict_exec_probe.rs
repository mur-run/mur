//! #2343 on a real kernel: under `processes.spawn.mode: strict`, Landlock
//! must let an allowlisted binary run and refuse every other one, including
//! a binary inside a READABLE dir (`/usr`) and one inside a WRITABLE dir.
//! Landlock rights are additive, so a read or write rule that still carried
//! `Execute` would reopen those — which is what this pins.
//!
//! It also pins the `mur` CLI carve-out (#2317) against that change: a read
//! grant holding a denied `mur` file is split around it, and the split must
//! not hand `Execute` back to that file's siblings.
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
const EXIT_CARVED_SIBLING_RAN: i32 = 26;

/// Allowlisted: must run. Unlisted, but under the `/usr` read grant: must not.
const ALLOWED: &str = "true";
const UNLISTED: &str = "id";
const COPIED: &str = "copied-binary";
/// Read-only dir holding a file named like the `mur` CLI (so the carve-out
/// splits this grant) next to an unlisted binary that must stay refused.
const CARVED_DIR: &str = "ro";
const CARVED_SIBLING: &str = "sibling-binary";

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
         filesystem: {{read: [\"/usr\", \"{r}\"], write: [\"{w}\"], deny: []}}\n\
         processes: {{spawn: {{mode: strict, allowed: [\"{ALLOWED}\"], allowed_dirs: []}}}}\n",
        w = dir.join("work").display(),
        r = dir.join(CARVED_DIR).display(),
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
    let sibling = dir.join(CARVED_DIR).join(CARVED_SIBLING);
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
    if std::fs::read(&copied).is_err() || std::fs::read(&sibling).is_err() {
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
    if runs(&sibling) {
        std::process::exit(EXIT_CARVED_SIBLING_RAN);
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
    let carved = tmp.path().join(CARVED_DIR);
    std::fs::create_dir_all(&carved).unwrap();
    std::fs::copy(system_binary(UNLISTED), carved.join(CARVED_SIBLING)).unwrap();
    std::fs::copy(system_binary(ALLOWED), carved.join("mur")).unwrap();
    // The scenario needs the carve-out to actually fire on this grant.
    let deny = strict_policy(tmp.path()).landlock_exec_denies();
    assert!(
        deny.iter().any(|d| d.starts_with(&carved)),
        "carve-out did not fire: {deny:?}"
    );

    // Negative control outside the sandbox: both refused binaries do run
    // unsandboxed, so a refusal in the child is the sandbox's doing.
    assert!(runs(&system_binary(UNLISTED)));
    assert!(runs(&work.join(COPIED)));
    assert!(runs(&carved.join(CARVED_SIBLING)));

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
        Some(EXIT_CARVED_SIBLING_RAN) => "carve-out re-granted Execute to a read-only sibling",
        _ => "child exited unexpectedly",
    };
    panic!("{why}");
}

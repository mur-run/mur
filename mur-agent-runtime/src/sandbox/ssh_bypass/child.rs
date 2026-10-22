//! Child side of the ssh-agent bypass probe (`<runtime> ssh-bypass-probe
//! <upstream>`, profile on stdin). It installs the agent's own SBPL profile on
//! itself, dials the upstream once, and reports through its exit code — see
//! [`super::probe`] for the protocol. Nothing else runs in this process.

use super::probe::{EXIT_SANDBOX_FAILED, PROBE_SUBCOMMAND, connect_only};
use std::io::Read;
use std::path::Path;

/// When `argv` names the probe subcommand, run it and exit; otherwise return.
/// Called from the supervisor entrypoint before `--help` and every side
/// effect (process group, sockets), like `mcp-shim`.
pub fn exit_if_probe(argv: &[String]) {
    if argv.get(1).map(String::as_str) == Some(PROBE_SUBCOMMAND) {
        std::process::exit(run(argv));
    }
}

/// Run the probe child and return its exit code.
pub fn run(argv: &[String]) -> i32 {
    let Some(upstream) = argv.get(2) else {
        return EXIT_SANDBOX_FAILED;
    };
    let mut profile = String::new();
    if std::io::stdin().read_to_string(&mut profile).is_err() {
        return EXIT_SANDBOX_FAILED;
    }
    if apply(&profile).is_err() {
        return EXIT_SANDBOX_FAILED;
    }
    connect_only(Path::new(upstream))
}

#[cfg(target_os = "macos")]
fn apply(profile: &str) -> Result<(), String> {
    crate::sandbox::macos::init_profile(profile)
}

/// Only macOS has an SBPL profile to install; anywhere else the parent never
/// reaches the probe (the static table is audit-only there), and a child that
/// cannot confine itself must not report a connect result as if it had.
#[cfg(not(target_os = "macos"))]
fn apply(_profile: &str) -> Result<(), String> {
    Err("no SBPL backend on this platform".into())
}

//! `mur agent start` must not require the per-agent BusyBox symlink.
//!
//! Field report (2026-09-15): the `mur` concierge had no `mur_agent_mur` link —
//! it had always come up through `direct_respawn`, which falls back to the
//! canonical runtime via `stale::runtime_path_for`. `start` had no such
//! fallback and refused outright; `install-service` then wrote a launchd plist
//! naming that same missing link, so the documented recovery produced a service
//! that could not exec. Two spawn paths, two different answers about where the
//! runtime is.
#![cfg(unix)]

use std::process::Command;
use tempfile::TempDir;

/// An agent home is all `start` needs; deliberately created WITHOUT the
/// symlink `mur agent create` would have made.
fn seed_agent(mur_home: &std::path::Path, name: &str) {
    let dir = mur_home.join("agents").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("profile.yaml"),
        format!("name: {name}\nid: 019eafc8-0000-7000-8000-000000000001\n"),
    )
    .unwrap();
}

#[test]
fn start_falls_back_to_the_canonical_runtime() {
    let mur_home = TempDir::new().unwrap();
    let bin_dir = TempDir::new().unwrap();
    seed_agent(mur_home.path(), "agent_x");

    // A stub that exits at once: enough to prove `start` reached the spawn.
    let stub = bin_dir.path().join("mur-agent-runtime");
    std::fs::write(&stub, "#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let out = Command::new(env!("CARGO_BIN_EXE_mur"))
        .env("MUR_HOME", mur_home.path())
        .env("MUR_AGENT_BIN_DIR", bin_dir.path())
        .env("MUR_AGENT_RUNTIME_BIN", &stub)
        .args(["agent", "start", "agent_x"])
        .output()
        .expect("spawn mur agent start");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("no runtime symlink"),
        "start must fall back like direct_respawn does, not refuse: {stderr}"
    );
    // The stub exits immediately, so the run legitimately ends "exited during
    // startup" — reaching that verdict IS the spawn having happened, which is
    // what this asserts.
    assert!(
        stderr.contains("exited during startup")
            || stderr.contains("not confirmed")
            || out.status.success(),
        "expected a spawn attempt, got: {stderr}"
    );
}

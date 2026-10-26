//! #2116 cross-router dispatch under a REAL seal: a `fleet_run`-enabled agent
//! with a default profile (no extra write grants) can read the daemon
//! heartbeat and drop a signed request in the daemon's inbox. Before the
//! policy granted these, the write was denied and, on Linux, the heartbeat
//! read failed first as "daemon is not running".
//!
//! Re-exec pattern from `git_push_seal.rs`: a `#[ctor]` hook in the child
//! seals for agent `<home>/agents/mur` and drives the real `fleet_run` tool.
//! The fake home lives under `CARGO_TARGET_TMPDIR`, not the system temp dir,
//! whose macOS baseline is write-exempt and would make the test vacuous.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

use mur_agent_runtime::tools::ToolExecutor;
use mur_track::fleet_dispatch_request::{FLEET_DISPATCH_REQUEST_DIR, FleetDispatchRequest};

const CASE_ENV: &str = "MUR_TEST_FLEET_DISPATCH_SEAL_CASE";
const HOME_ENV: &str = "MUR_TEST_FLEET_DISPATCH_SEAL_HOME";
const REQUIRE_SANDBOX_ENV: &str = "MUR_TEST_REQUIRE_SANDBOX";
const EXIT_SANDBOX_REQUIRED: i32 = 4;
const AGENT: &str = "mur";
const SEALED_MARKER: &str = ".sealed";

const READY_MARKER: &str = ".ready";
const WAIT: std::time::Duration = std::time::Duration::from_secs(20);

/// The heartbeat the daemon writes: whole file, tmp + rename in its directory.
fn beat(home: &Path, at: chrono::DateTime<chrono::Utc>) {
    let path = mur_common::paths::daemon_heartbeat_path(home);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let tmp = path.with_extension("tmp");
    let body = serde_json::json!({"pid": 1, "heartbeat_at": at.to_rfc3339()});
    std::fs::write(&tmp, body.to_string()).unwrap();
    std::fs::rename(&tmp, &path).unwrap();
}

/// Agent `mur` allowed to run `council`, which `orchestrator` routes. The
/// request inbox is NOT pre-created — the policy must create and grant it.
fn fresh_home() -> PathBuf {
    let home = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("fleet_dispatch_seal")
        .join(format!("submit-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join("agents").join(AGENT)).unwrap();
    std::fs::write(
        home.join("config.yaml"),
        format!("fleet_run:\n  agents: [{AGENT}]\n  fleets: [council]\n"),
    )
    .unwrap();
    let fleet = home.join("fleets").join("council");
    std::fs::create_dir_all(&fleet).unwrap();
    std::fs::write(
        fleet.join("fleet.yaml"),
        "name: council\nrouter: orchestrator\ngoal: g\nchannel_id: fleet-council\nmembers: [a, b]\n",
    )
    .unwrap();
    // Stale at seal time: the agent must see the FRESH beat the daemon
    // renames in after the seal — a new inode a file grant would miss.
    beat(&home, chrono::Utc::now() - chrono::Duration::hours(1));
    home
}

#[test]
fn sealed_agent_submits_a_cross_router_dispatch_request() {
    let home = fresh_home();
    let agent_home = home.join("agents").join(AGENT);
    let mut child = Command::new(std::env::current_exe().unwrap())
        .env(CASE_ENV, "submit")
        .env(HOME_ENV, &home)
        .env_remove("MUR_AGENT_SKIP_SANDBOX")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + WAIT;
    while !agent_home.join(READY_MARKER).exists() {
        if child.try_wait().unwrap().is_some() || std::time::Instant::now() > deadline {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    beat(&home, chrono::Utc::now());
    let out = child.wait_with_output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "sealed submit failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    if !agent_home.join(SEALED_MARKER).exists() {
        return; // sandbox not enforcing here; the child printed SKIP
    }
    let reqs: Vec<_> = std::fs::read_dir(home.join(FLEET_DISPATCH_REQUEST_DIR))
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "yaml"))
        .collect();
    assert_eq!(reqs.len(), 1, "{reqs:?}");
    let req: FleetDispatchRequest =
        serde_yaml_ng::from_str(&std::fs::read_to_string(&reqs[0]).unwrap()).unwrap();
    assert_eq!(req.dispatch.requested_by, AGENT);
    assert_eq!(req.dispatch.fleet, "council");
}

fn skip_or_fail_unenforced(why: &dyn std::fmt::Debug) -> ! {
    if std::env::var_os(REQUIRE_SANDBOX_ENV).is_some_and(|v| v == "1") {
        eprintln!("FAIL: {REQUIRE_SANDBOX_ENV}=1 but sandbox not enforcing: {why:?}");
        std::process::exit(EXIT_SANDBOX_REQUIRED);
    }
    eprintln!("SKIP: sandbox not enforcing: {why:?}");
    std::process::exit(0);
}

fn fail(msg: &str) -> ! {
    eprintln!("ERROR: {msg}");
    std::process::exit(1);
}

#[ctor::ctor]
fn fleet_dispatch_seal_subprocess_main() {
    if std::env::var_os(CASE_ENV).is_none() {
        return;
    }
    let home = PathBuf::from(std::env::var_os(HOME_ENV).expect("home env"));
    let agent_home = home.join("agents").join(AGENT);
    // Generated before the seal, as the runtime loads its key: `keys/` is
    // denied after.
    let identity = mur_common::identity::AgentIdentity::generate();
    let profile = mur_common::agent::AgentProfile::default_for_tests();
    match mur_agent_runtime::sandbox::apply(&profile.entitlements, &agent_home, &[], &[], &[]) {
        Ok(s) if s.enforcing => {}
        other => skip_or_fail_unenforced(&other),
    }
    // The heartbeat is the daemon's to write, never the agent's.
    if std::fs::write(mur_common::paths::daemon_heartbeat_path(&home), b"{}").is_ok() {
        fail("sealed agent could write the daemon heartbeat");
    }
    let _ = std::fs::write(agent_home.join(READY_MARKER), b"");

    let tool = mur_agent_runtime::tools::fleet_run::FleetRunTool {
        mur_home: home.clone(),
        agent_name: AGENT.into(),
        signing: Some(std::sync::Arc::new(identity)),
        key_version: 0,
        session_cwd: None,
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    // Retry until the parent's fresh beat lands; the last error is the verdict.
    let deadline = std::time::Instant::now() + WAIT;
    let last = loop {
        let r = rt.block_on(tool.execute(serde_json::json!({"fleet": "council", "goal": "g"})));
        match r {
            Ok(out) if out.text.contains("\"via\":\"daemon\"") => break None,
            Ok(out) => break Some(format!("unexpected reply: {}", out.text)),
            Err(e) if std::time::Instant::now() > deadline => break Some(format!("{e:?}")),
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(50)),
        }
    };
    if let Some(e) = last {
        fail(&format!("sealed submit refused: {e}"));
    }
    let _ = std::fs::write(agent_home.join(SEALED_MARKER), b"");
    std::process::exit(0);
}

//! #2139 — end to end proof for #2116's first acceptance criterion: a SEALED
//! agent dispatching a fleet routed by another agent produces channel events
//! that are signed, verify under `MUR_CHANNEL_REQUIRE_SIG`, and are never
//! signed with the requester's key.
//!
//! Three real processes, all re-execs of this test binary plus the real `mur`:
//!
//! 1. `submit` — seals as agent `scout` (default profile, real Landlock /
//!    Seatbelt), proves it cannot read another agent's private key, and
//!    calls the real `fleet_run` tool, which drops a signed request.
//! 2. `sweep` — the daemon half, unsealed, with `MUR_HOME` and
//!    `MUR_CHANNEL_REQUIRE_SIG=1` in its environment the way `murmurd` has
//!    them. It runs one real `fleet_dispatch::sweep`, which launches `mur`.
//! 3. the `mur fleet run --loop` child the sweep spawns — the run under test.
//!
//! No model is called: the fleet's kill-switch is engaged before dispatch, so
//! the loop passes the member write-grant gate, enters the loop, and stops at
//! iteration 0. Every channel write that path makes (two grant notes and the
//! stop event) goes through `append_as_writer`, the same writer every later
//! event uses. Router-actor events (`synth`) need a model and stay covered by
//! `channel_writer`'s `a_cross_router_run_outside_the_seal_signs_as_the_router`.
//!
//! The requester is `scout`, not `mur`: `System` events verify against
//! `ROUTER_AGENT` (`mur`), so a `mur` requester would make "not signed by the
//! requester" impossible to assert.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use mur_agent_runtime::tools::ToolExecutor;
use mur_common::channel::{ChannelActor, EventKind};
use mur_common::identity::AgentIdentity;
use mur_track::fleet_dispatch_request::{
    FLEET_DISPATCH_RECORD_FILE, FLEET_DISPATCH_REQUEST_DIR, FleetDispatchRecord,
    FleetDispatchStatus,
};

const CASE_ENV: &str = "MUR_TEST_CROSS_ROUTER_E2E_CASE";
const HOME_ENV: &str = "MUR_TEST_CROSS_ROUTER_E2E_HOME";
const WORK_ENV: &str = "MUR_TEST_CROSS_ROUTER_E2E_WORK";
const REQUIRE_SANDBOX_ENV: &str = "MUR_TEST_REQUIRE_SANDBOX";
const REQUIRE_SIG_ENV: &str = "MUR_CHANNEL_REQUIRE_SIG";
const EXIT_SANDBOX_REQUIRED: i32 = 4;

const REQUESTER: &str = "scout";
const ROUTER: &str = "orchestrator";
const FLEET: &str = "council";
const CHANNEL: &str = "fleet-council";
const MEMBERS: [&str; 2] = ["a", "b"];
const SEALED_MARKER: &str = ".sealed";
const RUN_LOG_FILE: &str = "fleet_run.log";
const UNSIGNED_WARNING: &str = "writing UNSIGNED";
const WAIT: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(100);

/// The fleet binary the daemon launches. `MUR_BIN` (what the daemon itself
/// honours) wins; else the workspace's `target/<profile>/mur`, which
/// `cargo nextest run --workspace` builds beside this test's `deps/`.
fn mur_bin() -> Option<PathBuf> {
    if let Some(v) = std::env::var_os("MUR_BIN") {
        return Some(PathBuf::from(v));
    }
    let exe = std::env::current_exe().ok()?;
    let profile_dir = exe.parent()?.parent()?;
    Some(profile_dir.join("mur")).filter(|p| p.is_file())
}

fn require_sandbox() -> bool {
    std::env::var_os(REQUIRE_SANDBOX_ENV).is_some_and(|v| v == "1")
}

fn save_identity(home: &Path, agent: &str) -> AgentIdentity {
    let dir = home.join("agents").join(agent);
    std::fs::create_dir_all(&dir).unwrap();
    let id = AgentIdentity::generate();
    id.save(&dir).unwrap();
    id
}

/// Requester `scout` may run `council`; `orchestrator` routes it; members get
/// a write grant on the work dir so the grant gate passes without a prompt.
/// The home is under `CARGO_TARGET_TMPDIR`: the system temp dir is
/// write-exempt in the macOS baseline and would make the seal vacuous.
fn fresh_home(work: &Path) -> PathBuf {
    let home = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("cross_router_dispatch_e2e")
        .join(format!("run-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(
        home.join("config.yaml"),
        format!("fleet_run:\n  agents: [{REQUESTER}]\n  fleets: [{FLEET}]\n"),
    )
    .unwrap();
    let fleet_dir = home.join("fleets").join(FLEET);
    std::fs::create_dir_all(&fleet_dir).unwrap();
    std::fs::write(
        fleet_dir.join("fleet.yaml"),
        format!(
            "name: {FLEET}\nrouter: {ROUTER}\ngoal: g\nchannel_id: {CHANNEL}\nmembers: [{}]\n",
            MEMBERS.join(", ")
        ),
    )
    .unwrap();
    // Engaged BEFORE dispatch: the run stops at iteration 0, model-free.
    std::fs::write(fleet_dir.join(".stopped"), "stopped\n").unwrap();
    let mut profile = mur_common::agent::AgentProfile::default_for_tests();
    profile.entitlements.filesystem.write = vec![work.display().to_string()];
    for m in MEMBERS {
        let dir = home.join("agents").join(m);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("profile.yaml"),
            serde_yaml_ng::to_string(&profile).unwrap(),
        )
        .unwrap();
    }
    home
}

fn beat(home: &Path) {
    mur_daemon::fleet_dispatch::write_heartbeat(home, chrono::Utc::now()).unwrap();
}

fn reexec(case: &str, home: &Path, work: &Path) -> Command {
    let mut c = Command::new(std::env::current_exe().unwrap());
    c.env(CASE_ENV, case)
        .env(HOME_ENV, home)
        .env(WORK_ENV, work)
        .env_remove("MUR_AGENT_SKIP_SANDBOX")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    c
}

fn run_ok(mut c: Command, what: &str) {
    let out = c.output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{what} failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Wait for the run's terminal stop event — the last write the child makes.
fn wait_for_stop(home: &Path, run_id: &str, log: &Path) -> Vec<mur_common::channel::ChannelEvent> {
    let svc = mur_channel::ChannelService::open(home).unwrap();
    let deadline = Instant::now() + WAIT;
    loop {
        let events = svc.load_events(CHANNEL).unwrap_or_default();
        let stopped = events.iter().any(|e| {
            e.kind == EventKind::StateChange
                && e.payload.get("run_id").and_then(|v| v.as_str()) == Some(run_id)
        });
        if stopped {
            return events;
        }
        assert!(
            Instant::now() < deadline,
            "no stop event for {run_id} within {WAIT:?}; child log:\n{}",
            std::fs::read_to_string(log).unwrap_or_default()
        );
        std::thread::sleep(POLL);
    }
}

#[test]
fn sealed_cross_router_dispatch_yields_verifiable_events_not_signed_by_the_requester() {
    let Some(mur) = mur_bin() else {
        assert!(
            !require_sandbox(),
            "{REQUIRE_SANDBOX_ENV}=1 but no `mur` binary beside this test; build the workspace"
        );
        eprintln!("SKIP: no `mur` binary; run `cargo build -p mur-core --bin mur` or set MUR_BIN");
        return;
    };
    // Outside every git checkout (temp is a git ceiling), so the routing
    // target is this directory and the members' grant covers it.
    let work = tempfile::tempdir().unwrap();
    let work = work.path().canonicalize().unwrap();
    let home = fresh_home(&work);
    let requester = save_identity(&home, REQUESTER);
    save_identity(&home, ROUTER);
    let writer = save_identity(&home, mur_core::channel_writer::ROUTER_AGENT);
    // The sealed child treats an unreadable key as isolation; that only means
    // something if the key is really there to be refused.
    for other in [ROUTER, mur_core::channel_writer::ROUTER_AGENT] {
        let key = home.join("keys").join(other).join("identity.key");
        assert!(key.is_file(), "expected private key at {}", key.display());
    }
    beat(&home);

    // 1. Sealed submission.
    run_ok(reexec("submit", &home, &work), "sealed submit");
    let agent_home = home.join("agents").join(REQUESTER);
    if !agent_home.join(SEALED_MARKER).exists() {
        assert!(
            !require_sandbox(),
            "{REQUIRE_SANDBOX_ENV}=1 but the sealed submit left no {SEALED_MARKER} marker"
        );
        return; // sandbox not enforcing here; the child printed SKIP
    }
    let reqs: Vec<PathBuf> = std::fs::read_dir(home.join(FLEET_DISPATCH_REQUEST_DIR))
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "yaml"))
        .collect();
    assert_eq!(reqs.len(), 1, "{reqs:?}");
    let run_id = reqs[0].file_stem().unwrap().to_string_lossy().into_owned();

    // 2. The daemon's sweep, which launches 3. the fleet child.
    let mut sweep = reexec("sweep", &home, &work);
    // Set the way murmurd's own environment carries them; the fleet child
    // the sweep launches inherits both.
    sweep
        .env("MUR_BIN", &mur)
        .env(mur_common::home::MUR_HOME_ENV, &home)
        .env(REQUIRE_SIG_ENV, "1");
    run_ok(sweep, "daemon sweep");

    let run_dir = home.join(mur_common::paths::RUNS).join(&run_id);
    let record: FleetDispatchRecord =
        serde_json::from_slice(&std::fs::read(run_dir.join(FLEET_DISPATCH_RECORD_FILE)).unwrap())
            .unwrap();
    assert_eq!(record.status, FleetDispatchStatus::Launched, "{record:?}");
    assert_eq!(record.dispatched_by, REQUESTER);
    assert_eq!(record.router.as_deref(), Some(ROUTER));

    let log = run_dir.join(RUN_LOG_FILE);
    let events = wait_for_stop(&home, &run_id, &log);
    assert!(events.len() >= 3, "grant notes + stop event: {events:#?}");
    let requester_pk = requester.verifying_key_bytes();
    let writer_pk = writer.verifying_key_bytes();
    for ev in &events {
        assert!(ev.sig.is_some(), "every event must be signed: {ev:#?}");
        assert!(
            mur_core::channel_verify::verify_event(&home, CHANNEL, ev, true),
            "must verify under {REQUIRE_SIG_ENV}: {ev:#?}"
        );
        assert!(
            !mur_channel::sign::verify_one(CHANNEL, ev, &requester_pk, true),
            "signed with the REQUESTER's key: {ev:#?}"
        );
        if matches!(ev.actor, ChannelActor::System) {
            assert!(
                mur_channel::sign::verify_one(CHANNEL, ev, &writer_pk, true),
                "a System event is signed by the trusted writer: {ev:#?}"
            );
        }
    }
    let log_text = std::fs::read_to_string(&log).unwrap();
    assert!(
        !log_text.contains(UNSIGNED_WARNING),
        "the child fell back to unsigned writes:\n{log_text}"
    );

    // The child's own record now answers the handle, and it is terminal. The
    // stop event lands in the channel before the run record is rewritten, so
    // poll the record rather than reading it once.
    let deadline = Instant::now() + WAIT;
    let status = loop {
        let status = mur_core::run_status::status_of(&home, &run_id)
            .unwrap()
            .expect("run recorded");
        if status.state != mur_core::run_status::State::Running || Instant::now() >= deadline {
            break status;
        }
        std::thread::sleep(POLL);
    };
    assert_eq!(
        status.state,
        mur_core::run_status::State::Stopped,
        "{status:#?}"
    );
}

fn skip_or_fail_unenforced(why: &dyn std::fmt::Debug) -> ! {
    if require_sandbox() {
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

/// Sealed `scout`: prove the key isolation, then submit through `fleet_run`.
fn submit(home: &Path, work: &Path) -> ! {
    let agent_home = home.join("agents").join(REQUESTER);
    // Loaded before the seal, as the runtime does: `keys/` is denied after.
    let identity = AgentIdentity::load(&agent_home).unwrap_or_else(|e| fail(&format!("{e}")));
    let profile = mur_common::agent::AgentProfile::default_for_tests();
    match mur_agent_runtime::sandbox::apply(&profile.entitlements, &agent_home, &[], &[], &[]) {
        Ok(s) if s.enforcing => {}
        other => skip_or_fail_unenforced(&other),
    }
    for other in [ROUTER, mur_core::channel_writer::ROUTER_AGENT] {
        let key = home.join("keys").join(other).join("identity.key");
        if std::fs::read(&key).is_ok() {
            fail(&format!("sealed agent read {}", key.display()));
        }
    }
    let tool = mur_agent_runtime::tools::fleet_run::FleetRunTool {
        mur_home: home.to_path_buf(),
        agent_name: REQUESTER.into(),
        signing: Some(std::sync::Arc::new(identity)),
        key_version: 0,
        session_cwd: None,
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let input = serde_json::json!({"fleet": FLEET, "goal": "g", "cwd": work});
    match rt.block_on(tool.execute(input)) {
        Ok(out) if out.text.contains("\"via\":\"daemon\"") => {}
        Ok(out) => fail(&format!("unexpected reply: {}", out.text)),
        Err(e) => fail(&format!("sealed submit refused: {e:?}")),
    }
    std::fs::write(agent_home.join(SEALED_MARKER), b"")
        .unwrap_or_else(|e| fail(&format!("write {SEALED_MARKER}: {e}")));
    std::process::exit(0);
}

/// The daemon half, with the environment `murmurd` would have.
fn sweep(home: &Path) -> ! {
    let cfg = mur_common::config::Config::try_load(&home.join("config.yaml"))
        .unwrap_or_else(|e| fail(&e));
    let mur = PathBuf::from(std::env::var_os("MUR_BIN").expect("MUR_BIN"));
    if let Err(e) =
        mur_daemon::fleet_dispatch::sweep(home, &home.join(FLEET_DISPATCH_REQUEST_DIR), &cfg, &mur)
    {
        fail(&format!("sweep: {e:#}"));
    }
    std::process::exit(0);
}

#[ctor::ctor]
fn cross_router_e2e_subprocess_main() {
    let Some(case) = std::env::var_os(CASE_ENV) else {
        return;
    };
    let home = PathBuf::from(std::env::var_os(HOME_ENV).expect("home env"));
    let work = PathBuf::from(std::env::var_os(WORK_ENV).expect("work env"));
    match case.to_str() {
        Some("submit") => submit(&home, &work),
        Some("sweep") => sweep(&home),
        _ => fail("unknown case"),
    }
}

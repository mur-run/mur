//! Crash safety of `ChannelService::append_and_anchor` under a real `SIGKILL`
//! (#1929, parent #1770).
//!
//! A child *process* anchors signed events in a tight loop; the parent kills
//! it with `SIGKILL` after a varying delay. A graceful exit would run every
//! flush and never open a crash window, so the kill has to come from outside
//! and be uncatchable. The child is this same test binary re-executed on
//! [`child_entry`], so no extra binary or crate is needed.
//!
//! There is deliberately no hook that parks the child between "line written"
//! and "head written": the anchored protocol exposes none, and adding a test
//! seam to production code would change what is being tested. Instead the
//! delay is swept across [`ROUNDS`] values, so the kill lands at different
//! points of the loop (before the marker, between marker and line, between
//! line and head, after the head). Which window a given round hits is not
//! controlled, and the test does not assert that any particular one was hit.
//!
//! What this proves, for whichever points the kill lands on: the log and head
//! are never left in a state `ChannelStore::load_events_with_damage` (the
//! check `mur channel verify` wraps) calls damage — no torn line, no broken
//! chain, no head mismatch; a log that is merely ahead of its head is not
//! damage — `events.lock` is released, and the next anchored append repairs
//! the head and clears the dirty marker. What it cannot prove: that `fsync`
//! survives a power cut. `SIGKILL` leaves the OS page cache intact, so this is
//! process durability, not media durability.
#![cfg(unix)]

use std::fs;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use mur_channel::ChannelService;
use mur_channel::anchor;
use mur_channel::store::DamageReport;
use mur_common::channel::{ChannelActor, EventKind};
use mur_common::identity::AgentIdentity;
use tempfile::TempDir;

const KV: u32 = 1;
/// Anchored appends the child completes before it signals readiness, so the
/// kill always lands on a channel that already has a head.
const WARMUP: usize = 3;
/// Kill delays are `round * DELAY_STEP_US` microseconds after readiness.
const ROUNDS: u64 = 24;
const DELAY_STEP_US: u64 = 450;
const READY_FILE: &str = "child-ready";
/// A child that nobody kills must not outlive the test run.
const CHILD_LIMIT: Duration = Duration::from_secs(120);
const READY_TIMEOUT: Duration = Duration::from_secs(60);
const SIGKILL: i32 = 9;

const ENV_ROLE: &str = "MUR_ANCHOR_CRASH_CHILD";
const ENV_HOME: &str = "MUR_ANCHOR_CRASH_HOME";
const ENV_CHANNEL: &str = "MUR_ANCHOR_CRASH_CHANNEL";
const ENV_SECRET: &str = "MUR_ANCHOR_CRASH_SECRET_HEX";

fn actor() -> ChannelActor {
    ChannelActor::Human { name: "me".into() }
}

fn anchor_one(svc: &ChannelService, ch: &str, id: &AgentIdentity, i: usize) {
    svc.append_and_anchor(
        ch,
        id,
        KV,
        Path::new(""),
        actor(),
        EventKind::Message,
        serde_json::json!({ "text": format!("event {i}") }),
        None,
    )
    .expect("anchored append");
}

fn damage(home: &Path, ch: &str, id: &AgentIdentity) -> DamageReport {
    mur_channel::ChannelStore::new(home)
        .load_events_with_damage(ch, &id.verifying_key_bytes(), false)
        .expect("load")
        .1
}

/// What the kill must never cause. A dirty marker and a head that lags the
/// log are the expected, recoverable leftovers, so they are not listed here.
fn assert_no_damage(d: &DamageReport, what: &str) {
    assert!(d.unparseable_lines.is_empty(), "{what}: torn line {d:?}");
    assert!(d.unverified.is_empty(), "{what}: unverified events {d:?}");
    assert!(d.chain_breaks.is_empty(), "{what}: chain break {d:?}");
    assert!(d.anchor_mismatch.is_none(), "{what}: head mismatch {d:?}");
}

/// The child half. A no-op unless the parent set the role, so a plain
/// `cargo test` run passes straight through it.
#[test]
fn child_entry() {
    if std::env::var(ENV_ROLE).is_err() {
        return;
    }
    let home = PathBuf::from(std::env::var(ENV_HOME).unwrap());
    let ch = std::env::var(ENV_CHANNEL).unwrap();
    let secret: [u8; 32] = hex::decode(std::env::var(ENV_SECRET).unwrap())
        .unwrap()
        .try_into()
        .unwrap();
    let id = AgentIdentity::from_secret_bytes(&secret);
    let svc = ChannelService::open(&home).unwrap();
    let start = Instant::now();
    let mut i = 0;
    while start.elapsed() < CHILD_LIMIT {
        anchor_one(&svc, &ch, &id, i);
        i += 1;
        if i == WARMUP {
            fs::write(home.join(READY_FILE), b"ready").unwrap();
        }
    }
    // Only reached if the parent never killed us.
    std::process::exit(3);
}

fn spawn_child(home: &Path, ch: &str, id: &AgentIdentity) -> Child {
    Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "child_entry", "--nocapture", "--test-threads=1"])
        .env(ENV_ROLE, "1")
        .env(ENV_HOME, home)
        .env(ENV_CHANNEL, ch)
        .env(ENV_SECRET, hex::encode(id.secret_bytes_for_handoff()))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn child")
}

fn wait_ready(home: &Path, child: &mut Child) {
    let ready = home.join(READY_FILE);
    let start = Instant::now();
    while !ready.exists() {
        assert!(
            child.try_wait().unwrap().is_none(),
            "child exited before it was ready"
        );
        assert!(start.elapsed() < READY_TIMEOUT, "child never became ready");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn sigkill_during_anchored_appends_leaves_no_damage() {
    let mut dirty_seen = 0;
    for round in 0..ROUNDS {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let svc = ChannelService::open(home).unwrap();
        let ch = svc.create_for_workflow("crash").unwrap().id;
        let id = AgentIdentity::generate();

        let mut child = spawn_child(home, &ch, &id);
        wait_ready(home, &mut child);
        std::thread::sleep(Duration::from_micros(round * DELAY_STEP_US));
        assert!(
            child.try_wait().unwrap().is_none(),
            "round {round}: child died on its own"
        );
        child.kill().expect("SIGKILL");
        let status = child.wait().unwrap();
        assert_eq!(
            status.signal(),
            Some(SIGKILL),
            "round {round}: not killed by SIGKILL: {status:?}"
        );

        // 1. Whatever point the kill landed on, nothing is damaged.
        let after_kill = damage(home, &ch, &id);
        assert_no_damage(&after_kill, &format!("round {round} after kill"));
        if anchor::is_dirty(home, &ch) {
            dirty_seen += 1;
        }

        // 2. The lock died with the process, and the next anchored append
        //    repairs the head and clears any marker the kill left behind.
        anchor_one(&svc, &ch, &id, usize::MAX);
        let repaired = damage(home, &ch, &id);
        assert_no_damage(&repaired, &format!("round {round} after repair"));
        assert!(
            repaired.is_clean(),
            "round {round}: still not clean after the next anchored append: {repaired:?}"
        );
        assert!(
            !anchor::is_dirty(home, &ch),
            "round {round}: dirty marker survived the next anchored append"
        );
    }
    // Informational: how many rounds the kill left the recoverable marker.
    println!("rounds with a dirty marker after the kill: {dirty_seen}/{ROUNDS}");
}

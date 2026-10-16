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
use std::sync::mpsc;
use std::time::{Duration, Instant};

use mur_channel::ChannelService;
use mur_channel::anchor;
use mur_channel::store::DamageReport;
use mur_common::channel::{ChannelActor, ChannelEvent, EventKind};
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
const READY_POLL_INTERVAL: Duration = Duration::from_millis(5);
/// How long the post-kill recovery append may wait for `events.lock`. The
/// kernel drops the dead child's lock, so this only trips on a real hang.
const RECOVERY_TIMEOUT: Duration = Duration::from_secs(30);
/// Exit code of a child that reached its time limit without being killed.
const CHILD_LIMIT_EXIT_CODE: i32 = 3;
/// Payload `i` of the recovery append, distinct from every child event.
const RECOVERY_MARKER: usize = usize::MAX;
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

fn load(home: &Path, ch: &str, id: &AgentIdentity) -> (Vec<ChannelEvent>, DamageReport) {
    mur_channel::ChannelStore::new(home)
        .load_events_with_damage(ch, &id.verifying_key_bytes(), false)
        .expect("load")
}

/// `(seq, text)` per event: what must survive a kill unchanged.
fn history(events: &[ChannelEvent]) -> Vec<(u64, String)> {
    events
        .iter()
        .map(|e| (e.seq, e.payload["text"].as_str().unwrap_or("").to_owned()))
        .collect()
}

/// Kills and reaps the child however the test leaves, including a failed
/// assertion, so a stuck child never outlives the run.
struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The recovery append on its own thread and its own service handle, bounded
/// by [`RECOVERY_TIMEOUT`], so a lock that never frees fails the test rather
/// than hanging it.
fn recover_with_timeout(home: &Path, ch: &str, id: &AgentIdentity) {
    let (home, ch) = (home.to_path_buf(), ch.to_owned());
    let secret = id.secret_bytes_for_handoff();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let id = AgentIdentity::from_secret_bytes(&secret);
        let svc = ChannelService::open(&home).expect("open");
        anchor_one(&svc, &ch, &id, RECOVERY_MARKER);
        let _ = tx.send(());
    });
    rx.recv_timeout(RECOVERY_TIMEOUT)
        .expect("recovery append did not finish: events.lock still held?");
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
    std::process::exit(CHILD_LIMIT_EXIT_CODE);
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
        std::thread::sleep(READY_POLL_INTERVAL);
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

        let mut guard = ChildGuard(spawn_child(home, &ch, &id));
        let child = &mut guard.0;
        wait_ready(home, child);
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

        // 1. Whatever point the kill landed on, nothing is damaged and the
        //    events acknowledged before the kill are still there, in order.
        let (killed_events, after_kill) = load(home, &ch, &id);
        assert_no_damage(&after_kill, &format!("round {round} after kill"));
        let before = history(&killed_events);
        assert!(
            before.len() >= WARMUP,
            "round {round}: lost warmup events: {before:?}"
        );
        for (i, (seq, text)) in before.iter().enumerate() {
            assert_eq!(
                text,
                &format!("event {i}"),
                "round {round}: history changed"
            );
            assert_eq!(*seq, before[0].0 + i as u64, "round {round}: seq gap");
        }
        if anchor::is_dirty(home, &ch) {
            dirty_seen += 1;
        }

        // 2. The lock died with the process, and the next anchored append
        //    repairs the head and clears any marker the kill left behind.
        recover_with_timeout(home, &ch, &id);
        let (repaired_events, repaired) = load(home, &ch, &id);
        assert_no_damage(&repaired, &format!("round {round} after repair"));
        let after = history(&repaired_events);
        assert_eq!(
            &after[..before.len()],
            &before[..],
            "round {round}: recovery rewrote history"
        );
        assert_eq!(
            after[before.len()..],
            [(
                before.last().unwrap().0 + 1,
                format!("event {RECOVERY_MARKER}")
            )],
            "round {round}: recovery event missing or duplicated"
        );
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

/// The state a kill between "line written" and "head written" leaves, built
/// deterministically: a dirty marker, plus a line the head does not cover.
/// The sweep above cannot promise it hit this window; this does.
#[test]
fn line_ahead_of_head_with_marker_is_not_damage_and_the_next_append_repairs_it() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    let svc = ChannelService::open(home).unwrap();
    let ch = svc.create_for_workflow("crash").unwrap().id;
    let id = AgentIdentity::generate();
    anchor_one(&svc, &ch, &id, 0);

    anchor::mark_dirty(home, &ch).unwrap();
    svc.append_signed(
        &ch,
        &id,
        KV,
        actor(),
        EventKind::Message,
        serde_json::json!({ "text": "event 1" }),
        None,
    )
    .unwrap();
    assert!(anchor::is_dirty(home, &ch));
    let (events, lagging) = load(home, &ch, &id);
    assert_no_damage(&lagging, "log ahead of head");
    assert_eq!(history(&events).len(), 2);

    recover_with_timeout(home, &ch, &id);
    let (events, repaired) = load(home, &ch, &id);
    assert!(repaired.is_clean(), "not clean after repair: {repaired:?}");
    assert!(!anchor::is_dirty(home, &ch));
    assert_eq!(history(&events).len(), 3);
}

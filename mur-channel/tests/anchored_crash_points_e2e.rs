//! Targeted `SIGKILL` crash points for the anchored append (#1872, parent
//! #1770).
//!
//! `anchored_crash_e2e.rs` sweeps the kill delay and so cannot promise it hit
//! any particular window. This file parks a real child process at an exact
//! point of the anchored protocol and kills it there with `SIGKILL`:
//!
//! - [`CrashPoint::AfterLine`]: the dirty marker is down and the line is in
//!   `events.jsonl`, but the head has not been advanced — the window the
//!   cross-process rule "the log may run ahead of the head, never the
//!   reverse" is about.
//! - [`CrashPoint::BeforeLine`]: the marker is down, nothing written — the
//!   deliberate false positive documented on `DamageReport::anchor_dirty`.
//!
//! The park points come from the store's public `SealHooks` (the same seam
//! `ChannelService::append_and_anchor` uses), driven from the test, so no
//! test-only switch is added to production code. The child mirrors the
//! production order — `mark_dirty` in `pre_write`, line written by the store
//! under `events.lock` — and parks instead of advancing the head.
//!
//! After the kill, the parent (a different process, on the real filesystem)
//! asserts the damage report is NOT clean (the marker is seen), that nothing
//! worse than a lagging anchor is reported, that the head on disk never
//! covers more than the log, and that the next anchored append catches the
//! head up so later appends verify clean. `SIGKILL` leaves the OS page cache
//! intact: this is process durability, not media durability.
#![cfg(unix)]

use std::fs;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use mur_channel::store::{DamageReport, PreSealInput, SealHooks, SealInput};
use mur_channel::{ChannelService, ChannelStore, anchor, head_io};
use mur_common::channel::{ChannelActor, ChannelEvent, EventKind};
use mur_common::identity::AgentIdentity;
use tempfile::TempDir;

const KV: u32 = 1;
/// Anchored appends that complete before the parked one, so the kill lands
/// on a channel that already has a head.
const WARMUP: usize = 3;
/// Clean anchored appends after the repair, each re-verified.
const LATER_APPENDS: usize = 3;
const PARKED_FILE: &str = "child-parked";
/// How long a parked child sleeps if nobody kills it.
const PARK_LIMIT: Duration = Duration::from_secs(120);
const PARK_TIMEOUT: Duration = Duration::from_secs(60);
const POLL_INTERVAL: Duration = Duration::from_millis(5);
/// Bound on the post-kill append; it only trips if `events.lock` never frees.
const RECOVERY_TIMEOUT: Duration = Duration::from_secs(30);
/// Exit code of a child that was never killed.
const UNKILLED_EXIT_CODE: i32 = 3;
const SIGKILL: i32 = 9;

const ENV_POINT: &str = "MUR_ANCHOR_POINT_CHILD";
const ENV_HOME: &str = "MUR_ANCHOR_POINT_HOME";
const ENV_CHANNEL: &str = "MUR_ANCHOR_POINT_CHANNEL";
const ENV_SECRET: &str = "MUR_ANCHOR_POINT_SECRET_HEX";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CrashPoint {
    BeforeLine,
    AfterLine,
}

impl CrashPoint {
    const fn tag(self) -> &'static str {
        match self {
            Self::BeforeLine => "before-line",
            Self::AfterLine => "after-line",
        }
    }

    fn from_tag(tag: &str) -> Self {
        [Self::BeforeLine, Self::AfterLine]
            .into_iter()
            .find(|p| p.tag() == tag)
            .unwrap_or_else(|| panic!("unknown crash point {tag:?}"))
    }
}

fn actor() -> ChannelActor {
    ChannelActor::Human { name: "me".into() }
}

fn text(i: usize) -> serde_json::Value {
    serde_json::json!({ "text": format!("event {i}") })
}

fn anchor_one(svc: &ChannelService, ch: &str, id: &AgentIdentity, i: usize) {
    svc.append_and_anchor(
        ch,
        id,
        KV,
        Path::new(""),
        actor(),
        EventKind::Message,
        text(i),
        None,
    )
    .expect("anchored append");
}

fn load(home: &Path, ch: &str, id: &AgentIdentity) -> (Vec<ChannelEvent>, DamageReport) {
    ChannelStore::new(home)
        .load_events_with_damage(ch, &id.verifying_key_bytes(), false)
        .expect("load")
}

fn log_lines(home: &Path, ch: &str) -> u64 {
    let path = ChannelStore::new(home).events_path(ch).unwrap();
    fs::read(path)
        .unwrap()
        .iter()
        .filter(|b| **b == b'\n')
        .count() as u64
}

fn head_lines(home: &Path, ch: &str) -> u64 {
    head_io::read_head(&anchor::heads_dir(home), ch)
        .expect("read head")
        .expect("a head exists after warmup")
        .lines
}

/// Park forever (bounded by [`PARK_LIMIT`]) after telling the parent where.
fn park(home: &Path) -> ! {
    fs::write(home.join(PARKED_FILE), b"parked").unwrap();
    std::thread::sleep(PARK_LIMIT);
    std::process::exit(UNKILLED_EXIT_CODE);
}

/// The child half: a no-op unless the parent set the crash point.
#[test]
fn child_entry() {
    let Ok(tag) = std::env::var(ENV_POINT) else {
        return;
    };
    let point = CrashPoint::from_tag(&tag);
    let home = PathBuf::from(std::env::var(ENV_HOME).unwrap());
    let ch = std::env::var(ENV_CHANNEL).unwrap();
    let secret: [u8; 32] = hex::decode(std::env::var(ENV_SECRET).unwrap())
        .unwrap()
        .try_into()
        .unwrap();
    let id = AgentIdentity::from_secret_bytes(&secret);
    let svc = ChannelService::open(&home).unwrap();
    for i in 0..WARMUP {
        anchor_one(&svc, &ch, &id, i);
    }

    // Same order as the production anchored append: marker under the lock,
    // then the line; park where the head would be advanced.
    let pre = |i: &PreSealInput<'_>| {
        anchor::mark_dirty(&home, i.channel_id)?;
        if point == CrashPoint::BeforeLine {
            park(&home);
        }
        Ok(())
    };
    let post = |_: &SealInput<'_>| -> anyhow::Result<()> { park(&home) };
    let hooks = SealHooks {
        pre_write: Some(&pre),
        post_write: Some(&post),
    };
    let _ = svc.store().append_signed_event_with(
        &ch,
        &id,
        KV,
        actor(),
        EventKind::Message,
        text(WARMUP),
        None,
        hooks,
    );
    unreachable!("the child parks inside the append");
}

/// Kills and reaps the child however the test leaves.
struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn_and_kill_at(point: CrashPoint, home: &Path, ch: &str, id: &AgentIdentity) {
    let child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "child_entry", "--nocapture", "--test-threads=1"])
        .env(ENV_POINT, point.tag())
        .env(ENV_HOME, home)
        .env(ENV_CHANNEL, ch)
        .env(ENV_SECRET, hex::encode(id.secret_bytes_for_handoff()))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn child");
    let mut guard = ChildGuard(child);
    let child = &mut guard.0;

    let parked = home.join(PARKED_FILE);
    let start = Instant::now();
    while !parked.exists() {
        assert!(
            child.try_wait().unwrap().is_none(),
            "{point:?}: child exited before parking"
        );
        assert!(start.elapsed() < PARK_TIMEOUT, "{point:?}: never parked");
        std::thread::sleep(POLL_INTERVAL);
    }
    child.kill().expect("SIGKILL");
    let status = child.wait().unwrap();
    assert_eq!(status.signal(), Some(SIGKILL), "{point:?}: {status:?}");
}

/// The post-kill anchored append on its own service handle, bounded so a
/// lock the dead child still held fails the test instead of hanging it.
fn anchor_with_timeout(home: &Path, ch: &str, id: &AgentIdentity, i: usize) {
    let (home, ch) = (home.to_path_buf(), ch.to_owned());
    let secret = id.secret_bytes_for_handoff();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let id = AgentIdentity::from_secret_bytes(&secret);
        let svc = ChannelService::open(&home).expect("open");
        anchor_one(&svc, &ch, &id, i);
        let _ = tx.send(());
    });
    rx.recv_timeout(RECOVERY_TIMEOUT)
        .expect("anchored append after the kill did not finish: events.lock still held?");
}

fn texts(events: &[ChannelEvent]) -> Vec<String> {
    events
        .iter()
        .map(|e| e.payload["text"].as_str().unwrap_or("").to_owned())
        .collect()
}

fn run(point: CrashPoint) {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    let ch = ChannelService::open(home)
        .unwrap()
        .create_for_workflow("crash-point")
        .unwrap()
        .id;
    let id = AgentIdentity::generate();

    spawn_and_kill_at(point, home, &ch, &id);

    // 3. After the kill: not clean, and the only thing reported is the
    //    lagging anchor — never a torn line, a broken chain, or a mismatch.
    let (events, report) = load(home, &ch, &id);
    assert!(
        !report.is_clean(),
        "{point:?}: kill went unseen: {report:?}"
    );
    assert!(
        report.anchor_dirty,
        "{point:?}: no dirty marker: {report:?}"
    );
    assert!(report.unparseable_lines.is_empty(), "{point:?}: {report:?}");
    assert!(report.unverified.is_empty(), "{point:?}: {report:?}");
    assert!(report.chain_breaks.is_empty(), "{point:?}: {report:?}");
    assert!(report.anchor_mismatch.is_none(), "{point:?}: {report:?}");

    let (log, head) = (log_lines(home, &ch), head_lines(home, &ch));
    assert!(head <= log, "{point:?}: head ({head}) ahead of log ({log})");
    let written = match point {
        CrashPoint::BeforeLine => WARMUP,
        CrashPoint::AfterLine => WARMUP + 1,
    };
    let expected: Vec<String> = (0..written).map(|i| format!("event {i}")).collect();
    assert_eq!(texts(&events), expected, "{point:?}: history");
    if point == CrashPoint::AfterLine {
        assert!(head < log, "{point:?}: head covers the unanchored line");
    }

    // 4. The next anchored append catches the head up; later ones verify.
    for n in 0..=LATER_APPENDS {
        let i = written + n;
        anchor_with_timeout(home, &ch, &id, i);
        let (events, report) = load(home, &ch, &id);
        assert!(report.is_clean(), "{point:?}: append {n}: {report:?}");
        assert!(!anchor::is_dirty(home, &ch), "{point:?}: append {n}");
        assert_eq!(head_lines(home, &ch), log_lines(home, &ch));
        assert_eq!(events.len(), i + 1, "{point:?}: append {n}");
        assert_eq!(texts(&events)[i], format!("event {i}"));
    }
}

#[test]
fn sigkill_after_line_before_head_is_detected_and_repaired() {
    run(CrashPoint::AfterLine);
}

#[test]
fn sigkill_after_marker_before_line_is_detected_and_repaired() {
    run(CrashPoint::BeforeLine);
}

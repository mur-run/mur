//! End-to-end crash safety of the signed head seal (#1872, parent #1770).
//!
//! The unit tests simulate crash states by hand-building files. This one makes
//! the crash real: a child *process* appends signed events through the real
//! `ChannelService` + `HeadSealer`, parks inside the seal hook, and the parent
//! `SIGKILL`s it. A graceful exit would run every flush and never open the
//! window, so the kill has to come from outside and be uncatchable.
//!
//! The child is this same test binary re-executed on [`child_entry`], so no
//! extra binary or crate is needed.
//!
//! What this proves: process death at the worst point leaves a log the
//! verifier does not call clean, releases `events.lock`, and the next router
//! write catches the head up. What it cannot prove: that `fsync` survives a
//! power cut. `SIGKILL` leaves the OS page cache intact, so this is process
//! durability, not media durability.
//!
//! Stack note: the `.dirty` marker of #1869 is not part of this stack. Here the
//! "log ahead of head" state is detected as an `Unsealed` tail line (spec 8.2),
//! not as a dirty marker, and the head itself stays `Intact` for the prefix it
//! covers. `mur channel verify` reports such a channel as `unverified`.
#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use mur_channel::ChannelService;
use mur_channel::head_io::read_head;
use mur_channel::seal::{HEADS_DIR_NAME, HeadSealer};
use mur_channel::service::SharedSealHook;
use mur_channel::verify::{ChannelReport, IntegrityState, LineClass, verify_channel};
use mur_common::channel::{ChannelActor, EventKind};
use mur_common::identity::AgentIdentity;
use std::os::unix::process::ExitStatusExt;
use tempfile::TempDir;

const KV: u32 = 1;
/// Events the child appends and seals before the one it dies on.
const SEALED_BEFORE_CRASH: usize = 3;
const READY_FILE: &str = "child-ready";
/// A parked child that nobody kills must not outlive the test run.
const PARK_LIMIT: Duration = Duration::from_secs(120);
const READY_TIMEOUT: Duration = Duration::from_secs(60);

const ENV_ROLE: &str = "MUR_SEAL_CRASH_CHILD";
const ENV_HOME: &str = "MUR_SEAL_CRASH_HOME";
const ENV_CHANNEL: &str = "MUR_SEAL_CRASH_CHANNEL";
const ENV_SECRET: &str = "MUR_SEAL_CRASH_SECRET_HEX";
const ENV_MODE: &str = "MUR_SEAL_CRASH_MODE";

/// Where in the final append the child is killed.
#[derive(Clone, Copy)]
enum Mode {
    /// The line is durable, the head is not yet written: the crash window.
    AfterLineBeforeHead,
    /// The head is written, then the child dies. Control: proves the harness
    /// tells "crashed in the window" apart from "crashed after sealing".
    AfterHead,
}

impl Mode {
    fn as_str(self) -> &'static str {
        match self {
            Mode::AfterLineBeforeHead => "after-line",
            Mode::AfterHead => "after-head",
        }
    }
}

fn payload(i: usize) -> serde_json::Value {
    serde_json::json!({ "text": format!("event {i}") })
}

fn append(svc: &ChannelService, id: &str, identity: &AgentIdentity, i: usize) {
    svc.append_signed(
        id,
        identity,
        KV,
        ChannelActor::Human { name: "me".into() },
        EventKind::Message,
        payload(i),
        None,
    )
    .expect("append");
}

fn log_lines(home: &Path, id: &str) -> usize {
    let p = mur_channel::ChannelStore::new(home).events_path(id);
    fs::read(p).unwrap().iter().filter(|b| **b == b'\n').count()
}

fn report(home: &Path, id: &str, identity: &AgentIdentity) -> ChannelReport {
    let key = identity.verifying_key_bytes();
    verify_channel(home, id, &|_| Some(key)).expect("verify")
}

fn count(r: &ChannelReport, class: LineClass) -> usize {
    r.lines.iter().filter(|l| l.class == class).count()
}

/// The child half. A no-op unless the parent set the role, so a plain
/// `cargo test` run passes straight through it.
#[test]
fn child_entry() {
    if std::env::var(ENV_ROLE).is_err() {
        return;
    }
    let home = PathBuf::from(std::env::var(ENV_HOME).unwrap());
    let id = std::env::var(ENV_CHANNEL).unwrap();
    let mode = std::env::var(ENV_MODE).unwrap();
    let secret: [u8; 32] = hex::decode(std::env::var(ENV_SECRET).unwrap())
        .unwrap()
        .try_into()
        .unwrap();
    let identity = Arc::new(AgentIdentity::from_secret_bytes(&secret));
    let sealer = Arc::new(HeadSealer::new(&home, identity.clone(), KV));

    // The hook runs under `events.lock`, after the line is written. Calls
    // 1..=SEALED_BEFORE_CRASH seal normally; the next one is where we die.
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let ready = home.join(READY_FILE);
    let hook: SharedSealHook = {
        let sealer = sealer.clone();
        Arc::new(move |input| {
            let n = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            if n <= SEALED_BEFORE_CRASH {
                return sealer.seal(input);
            }
            if mode == Mode::AfterHead.as_str() {
                sealer.seal(input)?;
            }
            fs::write(&ready, b"parked").unwrap();
            // Park holding the lock until the parent kills us. The limit only
            // stops an orphan if the parent died first.
            let start = Instant::now();
            while start.elapsed() < PARK_LIMIT {
                std::thread::sleep(Duration::from_millis(50));
            }
            std::process::exit(3);
        })
    };

    let svc = ChannelService::open(&home).unwrap().with_seal(hook);
    for i in 0..=SEALED_BEFORE_CRASH {
        append(&svc, &id, &identity, i);
    }
    // Reaching here means the child was never killed.
    std::process::exit(4);
}

struct Crashed {
    _tmp: TempDir,
    home: PathBuf,
    id: String,
    identity: AgentIdentity,
}

/// Run the child to its crash point and SIGKILL it.
fn crash_child(mode: Mode) -> Crashed {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().to_path_buf();
    let id = ChannelService::open(&home)
        .unwrap()
        .create_for_agent("crash-agent")
        .unwrap()
        .id;
    let identity = AgentIdentity::generate();

    let mut child: Child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "child_entry", "--nocapture", "--test-threads=1"])
        .env(ENV_ROLE, "1")
        .env(ENV_HOME, &home)
        .env(ENV_CHANNEL, &id)
        .env(ENV_MODE, mode.as_str())
        .env(ENV_SECRET, hex::encode(identity.secret_bytes_for_handoff()))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn child");

    let start = Instant::now();
    let ready = home.join(READY_FILE);
    while !ready.exists() {
        if let Some(status) = child.try_wait().unwrap() {
            panic!("child exited before reaching the crash point: {status}");
        }
        if start.elapsed() > READY_TIMEOUT {
            let _ = child.kill();
            panic!("child never reached the crash point");
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    // `Child::kill` is SIGKILL on unix: no handler, no flush, no destructor.
    child.kill().expect("kill child");
    let status = child.wait().unwrap();
    assert_eq!(
        status.signal(),
        Some(9),
        "child must die by SIGKILL, got {status}"
    );
    Crashed {
        _tmp: tmp,
        home,
        id,
        identity,
    }
}

#[test]
fn kill_between_line_and_head_leaves_the_log_ahead_of_the_head() {
    let c = crash_child(Mode::AfterLineBeforeHead);
    let total = SEALED_BEFORE_CRASH + 1;

    assert_eq!(log_lines(&c.home, &c.id), total, "the line is durable");
    let head = read_head(&c.home.join(HEADS_DIR_NAME), &c.id)
        .unwrap()
        .expect("earlier seals left a head");
    assert_eq!(
        head.lines, SEALED_BEFORE_CRASH as u64,
        "the head still describes the log as of the last completed seal"
    );

    let r = report(&c.home, &c.id, &c.identity);
    // The head is honest about the prefix it covers; the damage is the tail.
    assert_eq!(r.state, IntegrityState::Intact);
    assert_eq!(count(&r, LineClass::Sealed), SEALED_BEFORE_CRASH);
    assert_eq!(
        count(&r, LineClass::Unsealed),
        1,
        "the crashed line must not be reported as sealed: {:?}",
        r.lines
    );
    assert_eq!(r.lines.last().unwrap().class, LineClass::Unsealed);
}

#[test]
fn kill_after_the_head_is_written_is_fully_sealed() {
    let c = crash_child(Mode::AfterHead);
    let total = SEALED_BEFORE_CRASH + 1;

    assert_eq!(log_lines(&c.home, &c.id), total);
    let head = read_head(&c.home.join(HEADS_DIR_NAME), &c.id)
        .unwrap()
        .unwrap();
    assert_eq!(head.lines, total as u64);

    let r = report(&c.home, &c.id, &c.identity);
    assert_eq!(r.state, IntegrityState::Intact);
    assert_eq!(count(&r, LineClass::Sealed), total);
    assert_eq!(count(&r, LineClass::Unsealed), 0);
}

#[test]
fn next_router_write_catches_the_head_up_after_a_kill() {
    let c = crash_child(Mode::AfterLineBeforeHead);
    let total = SEALED_BEFORE_CRASH + 1;

    // A fresh writer, as after a restart. This also proves SIGKILL released
    // `events.lock`: a held flock would block this append forever.
    let identity = Arc::new(AgentIdentity::from_secret_bytes(
        &c.identity.secret_bytes_for_handoff(),
    ));
    let hook = HeadSealer::new(&c.home, identity.clone(), KV).hook();
    let svc = ChannelService::open(&c.home).unwrap().with_seal(hook);
    append(&svc, &c.id, &identity, 99);

    let head = read_head(&c.home.join(HEADS_DIR_NAME), &c.id)
        .unwrap()
        .unwrap();
    assert_eq!(
        head.lines,
        (total + 1) as u64,
        "head covers the old tail too"
    );

    let r = report(&c.home, &c.id, &identity);
    assert_eq!(r.state, IntegrityState::Intact);
    assert_eq!(count(&r, LineClass::Sealed), total + 1);
    assert_eq!(count(&r, LineClass::Unsealed), 0);
    assert!(!r.is_failure(), "{r:?}");
}

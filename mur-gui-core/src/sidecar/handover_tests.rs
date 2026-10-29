//! Actor hand-over with fake runtimes: a runtime here is a shell script set
//! through `MUR_AGENT_RUNTIME_BIN`, so every test spawns real processes.

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::time::{Duration, Instant};

use mur_common::lock_file::pid_alive;
use mur_common::test_env::EnvGuard;

use super::super::Supervisor;
use super::stop_child;

/// Short enough that a test sees an exit within a few hundred ms.
const POLL: Duration = Duration::from_millis(50);

fn fake_runtime(dir: &Path, body: &str) -> PathBuf {
    let p = dir.join("fake-runtime");
    std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

struct Rig {
    rt: tokio::runtime::Runtime,
    sup: Supervisor,
    home: tempfile::TempDir,
    bin: PathBuf,
    _env: EnvGuard,
}

fn rig(body: &str) -> Rig {
    let home = tempfile::tempdir().unwrap();
    let bin = fake_runtime(home.path(), body);
    let env = EnvGuard::set([("MUR_AGENT_RUNTIME_BIN", &bin)]);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let sup = {
        let _g = rt.enter();
        Supervisor::with_poll_interval(home.path().to_path_buf(), POLL)
    };
    Rig {
        rt,
        sup,
        home,
        bin,
        _env: env,
    }
}

impl Rig {
    fn start(&self, slug: &str) {
        self.rt.block_on(self.sup.start(slug)).unwrap();
    }
    fn take(&self, slug: &str) -> Option<Child> {
        self.rt.block_on(self.sup.take_child(slug))
    }
}

/// Waits for the fake runtime to touch `$0.ready`, which it does only after
/// installing its TERM trap. A fixed sleep raced the shell under load: SIGTERM
/// landed before the trap and the default action killed the child early.
fn wait_ready(bin: &Path) {
    let ready = PathBuf::from(format!("{}.ready", bin.display()));
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready.exists() {
        assert!(Instant::now() < deadline, "fake runtime never became ready");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn gone_within(pid: u32, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if !pid_alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    !pid_alive(pid)
}

#[test]
fn take_child_hands_over_and_status_still_answers() {
    let r = rig("exec sleep 30");
    r.start("a");
    let mut child = r.take("a").expect("the Hub started a");
    // The actor is free while the test thread holds the child.
    r.start("b");
    let _ = child.kill();
    let _ = child.wait();
    r.rt.block_on(r.sup.clone().shutdown());
}

#[test]
fn take_child_for_an_unknown_slug_is_none() {
    let r = rig("exec sleep 30");
    assert!(r.take("nobody").is_none());
}

#[test]
fn stop_child_sends_term_before_kill() {
    let r = rig(
        r#"trap 'echo term >> "$0.log"; exit 0' TERM; : > "$0.ready"; while :; do sleep 0.05; done"#,
    );
    r.start("a");
    let mut child = r.take("a").unwrap();
    wait_ready(&r.bin);
    let st = stop_child(&mut child, Duration::from_secs(5), Duration::from_secs(1)).unwrap();
    assert!(st.success(), "{st}");
    let log = std::fs::read_to_string(format!("{}.log", r.bin.display())).unwrap();
    assert_eq!(log.trim(), "term");
}

#[test]
fn stop_child_kills_after_kill_wait() {
    let r = rig(r#"trap '' TERM; : > "$0.ready"; while :; do sleep 0.05; done"#);
    r.start("a");
    let mut child = r.take("a").unwrap();
    wait_ready(&r.bin);
    let t0 = Instant::now();
    stop_child(
        &mut child,
        Duration::from_millis(300),
        Duration::from_secs(2),
    )
    .unwrap();
    let waited = t0.elapsed();
    assert!(
        waited >= Duration::from_millis(300),
        "child died on TERM after {waited:?}; the trap was not in place"
    );
    // Reaped: try_wait keeps answering with the cached status, no zombie.
    assert!(matches!(child.try_wait(), Ok(Some(_))));
}

#[test]
fn returned_child_is_reinserted_and_stop_reaches_it() {
    let r = rig("exec sleep 30");
    r.start("a");
    let child = r.take("a").unwrap();
    let pid = child.id();
    r.sup.try_return_child("a".into(), child).unwrap();
    r.rt.block_on(r.sup.stop("a"));
    assert!(
        gone_within(pid, Duration::from_secs(2)),
        "pid {pid} survived"
    );
}

#[test]
fn returned_child_for_an_occupied_slug_is_killed_and_reaped() {
    let r = rig("exec sleep 30");
    r.start("a");
    let old = r.take("a").unwrap();
    let old_pid = old.id();
    r.start("a"); // refills the slot with a new child
    r.sup.try_return_child("a".into(), old).unwrap();
    // Killed on return, reaped on the next tick: `pid_alive` is false only
    // once the zombie is gone.
    assert!(
        gone_within(old_pid, Duration::from_secs(2)),
        "pid {old_pid} not reaped"
    );
    let new = r.take("a").expect("the new child kept the slot");
    assert_ne!(new.id(), old_pid);
    r.sup.try_return_child("a".into(), new).unwrap();
    r.rt.block_on(r.sup.clone().shutdown());
}

#[test]
fn start_watched_reports_the_exit() {
    let r = rig("exit 3");
    let mut w = r.rt.block_on(r.sup.start_watched("a")).unwrap();
    assert!(w.pid.is_some());
    let st =
        r.rt.block_on(async {
            tokio::time::timeout(Duration::from_secs(2), w.exit.wait_for(Option::is_some))
                .await
                .map(|r| r.map(|s| *s))
        })
        .expect("exit reported within the poll")
        .unwrap()
        .unwrap();
    assert_eq!(st.code(), Some(3));
}

#[test]
fn start_watched_on_a_foreign_live_lock_has_no_pid() {
    let r = rig("exec sleep 30");
    super::super::tests::seed_runtime_state(r.home.path(), "a", std::process::id());
    let w = r.rt.block_on(r.sup.start_watched("a")).unwrap();
    assert!(w.pid.is_none());
    std::thread::sleep(POLL * 4);
    assert!(w.exit.borrow().is_none());
}

#[test]
fn shutdown_kills_the_reaping_list() {
    let r = rig("exec sleep 30");
    r.start("a");
    let old = r.take("a").unwrap();
    let old_pid = old.id();
    r.start("a");
    let new_pid = r.take("a").map(|c| {
        let id = c.id();
        r.sup.try_return_child("a".into(), c).unwrap();
        id
    });
    r.sup.try_return_child("a".into(), old).unwrap();
    r.rt.block_on(r.sup.clone().shutdown());
    assert!(gone_within(old_pid, Duration::from_secs(2)));
    if let Some(p) = new_pid {
        assert!(gone_within(p, Duration::from_secs(2)));
    }
}

#[test]
fn return_child_within_waits_for_room() {
    let r = rig("exec sleep 30");
    r.start("a");
    let child = r.take("a").unwrap();
    let pid = child.id();
    // From a plain (non-runtime) thread, as the Hub's blocking job does.
    let handle = r.rt.handle().clone();
    r.sup
        .return_child_within(&handle, "a".into(), child, Duration::from_secs(1))
        .unwrap();
    r.rt.block_on(r.sup.stop("a"));
    assert!(gone_within(pid, Duration::from_secs(2)));
}

#[test]
fn a_closed_actor_hands_the_child_back() {
    let r = rig("exec sleep 30");
    r.start("a");
    let mut child = r.take("a").unwrap();
    r.rt.block_on(r.sup.clone().shutdown());
    let deadline = Instant::now() + Duration::from_secs(2);
    while !r.sup.msg_tx.is_closed() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    child = r
        .sup
        .try_return_child("a".into(), child)
        .expect_err("a closed actor cannot take the child");
    let _ = child.kill();
    let _ = child.wait();
}

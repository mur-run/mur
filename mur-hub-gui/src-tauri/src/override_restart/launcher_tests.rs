//! `HubLauncher` against real processes: a runtime is a shell script set
//! through `MUR_AGENT_RUNTIME_BIN`. The helpers mirror `mur-gui-core`'s
//! hand-over tests; they are test code in another crate, so they are copied.

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus};
use std::time::{Duration, Instant};

use mur_common::lock_file::pid_alive;
use mur_common::test_env::EnvGuard;
use mur_core::cmd::agent::{Launcher, RestartNote};
use mur_gui_core::sidecar::Supervisor;

use super::HubLauncher;

const POLL: Duration = Duration::from_millis(50);
const SHORT: Duration = Duration::from_millis(300);

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
    _env: EnvGuard,
}

fn rig(body: &str) -> Rig {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join("agents/a")).unwrap();
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
        _env: env,
    }
}

impl Rig {
    fn launcher(&self) -> HubLauncher {
        HubLauncher::new(
            self.sup.clone(),
            self.rt.handle().clone(),
            "a",
            self.home.path(),
            false,
        )
    }
    /// Start `a` through the supervisor and return its pid.
    fn start_a(&self) -> u32 {
        let w = self.rt.block_on(self.sup.start_watched("a")).unwrap();
        w.pid.expect("supervisor spawned a child")
    }
    fn stop_a(&self) {
        self.rt.block_on(self.sup.stop("a"));
    }
}

fn gone_within(pid: u32, within: Duration) -> bool {
    let until = Instant::now() + within;
    while Instant::now() < until {
        if !pid_alive(pid) {
            return true;
        }
        std::thread::sleep(POLL);
    }
    !pid_alive(pid)
}

fn failing_stopper(_: &mut Child, _: Duration, _: Duration) -> Result<ExitStatus, ()> {
    Err(())
}

fn panicking_stopper(_: &mut Child, _: Duration, _: Duration) -> Result<ExitStatus, ()> {
    panic!("stopper blew up");
}

#[test]
fn stop_takes_our_child_and_reaps_it() {
    let r = rig("exec sleep 30");
    let pid = r.start_a();
    let mut l = r.launcher();
    assert!(l.stop(pid, SHORT).is_ok());
    // Reaped by stop_child, so not even a zombie remains.
    assert!(!pid_alive(pid));
}

#[test]
fn stop_falls_back_to_the_lock_pid_when_nothing_to_take() {
    let r = rig("exec sleep 30");
    let mut stray = Command::new("sleep").arg("30").spawn().unwrap();
    let pid = stray.id();
    let reaper = std::thread::spawn(move || stray.wait());
    let mut l = r.launcher();
    assert!(l.stop(pid, SHORT).is_ok());
    reaper.join().unwrap().unwrap();
    assert!(!pid_alive(pid));
}

#[test]
fn a_failed_stop_gives_the_child_back() {
    let r = rig("exec sleep 30");
    let pid = r.start_a();
    let mut l = r.launcher().with_stopper(failing_stopper);
    let err = l.stop(pid, SHORT).unwrap_err();
    assert_eq!(err.pid, pid);
    drop(l);
    // Back in the supervisor: its Stop reaches the child.
    r.stop_a();
    assert!(gone_within(pid, Duration::from_secs(2)));
}

#[test]
fn a_panic_returns_the_child_through_drop() {
    let r = rig("exec sleep 30");
    let pid = r.start_a();
    let l = r.launcher().with_stopper(panicking_stopper);
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let mut l = l;
        let _ = l.stop(pid, SHORT);
    }));
    assert!(res.is_err());
    r.stop_a();
    assert!(gone_within(pid, Duration::from_secs(2)));
}

#[test]
fn drop_with_a_closed_actor_kills_the_child() {
    let r = rig("exec sleep 30");
    let pid = r.start_a();
    let mut l = r.launcher().with_stopper(failing_stopper);
    assert!(l.stop(pid, SHORT).is_err());
    // The launcher holds the child; closing the actor leaves nowhere to return it.
    r.rt.block_on(r.sup.clone().shutdown());
    std::thread::sleep(POLL * 4);
    drop(l);
    assert!(gone_within(pid, Duration::from_secs(2)));
}

#[test]
fn start_uses_start_watched_and_reports_an_early_exit() {
    let r = rig("exit 3");
    let mut l = r.launcher();
    let pid = l.start().unwrap().expect("a pid from the supervisor");
    let until = Instant::now() + Duration::from_secs(2);
    let msg = loop {
        if let Some(m) = l.exited_early() {
            break m;
        }
        assert!(Instant::now() < until, "exit never reported");
        std::thread::sleep(POLL);
    };
    assert!(msg.contains("exit"), "{msg}");
    assert_eq!(l.fresh_pid(None), None);
    assert_ne!(pid, 0);
}

#[test]
fn fresh_pid_is_the_watched_child_while_it_lives() {
    let r = rig("exec sleep 30");
    let mut l = r.launcher();
    let pid = l.start().unwrap().unwrap();
    assert_eq!(l.fresh_pid(None), Some(pid));
    assert_eq!(l.fresh_pid(Some(pid)), None);
    assert_eq!(l.exited_early(), None);
    r.stop_a();
}

#[test]
fn kick_never_direct_respawns() {
    let r = rig("exec sleep 30");
    let bin_dir = tempfile::tempdir().unwrap();
    let _env = EnvGuard::set([("MUR_AGENT_BIN_DIR", bin_dir.path())]);
    let mut l = r.launcher();
    let mut notes = Vec::new();
    let pid = l
        .kick(&mut |n| notes.push(n))
        .unwrap()
        .expect("supervisor spawn");
    assert!(pid_alive(pid));
    assert_eq!(std::fs::read_dir(bin_dir.path()).unwrap().count(), 0);
    assert!(matches!(
        notes.as_slice(),
        [RestartNote::Info(s)] if s.contains("asked the Hub supervisor again")
    ));
    r.stop_a();
}

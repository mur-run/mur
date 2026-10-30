use super::*;
use mur_common::test_env::EnvGuard;
use std::io::Write;
use std::process::{Child, Command, Stdio};

/// An agent dir whose sentinel holds `contents`; with `held`, a second open
/// file description keeps the flock (flock conflicts across descriptions,
/// even inside one process), as a booting runtime would.
fn sentinel(home: &Path, slug: &str, contents: &str, held: bool) -> Option<std::fs::File> {
    let dir = home.join("agents").join(slug);
    std::fs::create_dir_all(&dir).unwrap();
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(true)
        .open(dir.join(SENTINEL_FILE))
        .unwrap();
    f.write_all(contents.as_bytes()).unwrap();
    if held {
        f.lock_exclusive().unwrap();
        Some(f)
    } else {
        None
    }
}

/// A live process running `bin`, blocked on its stdin until killed.
fn spawn_live(bin: &Path, args: &[&str]) -> Child {
    Command::new(bin)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

const SLEEP: &str = "/bin/sleep";
const CAT: &str = "/bin/cat";

/// Point the runtime lookup at `runtime` (`MUR_AGENT_RUNTIME_BIN`) and the
/// symlink dir at an empty `links/`. System binaries stand in for the runtime:
/// the check compares executables, and a copied binary in a temp dir is not
/// executable under every sandbox.
fn runtime_is(tmp: &Path, runtime: &str) -> EnvGuard {
    let links = tmp.join("links");
    std::fs::create_dir_all(&links).unwrap();
    EnvGuard::set([
        ("MUR_AGENT_RUNTIME_BIN", std::ffi::OsStr::new(runtime)),
        (mur_common::agent_bin::BIN_DIR_ENV, links.as_os_str()),
    ])
}

fn kill(mut c: Child) {
    let _ = c.kill();
    let _ = c.wait();
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn flocked_with_a_live_runtime_pid_targets_it() {
    let tmp = tempfile::tempdir().unwrap();
    let _env = runtime_is(tmp.path(), SLEEP);
    let child = spawn_live(Path::new(SLEEP), &["30"]);
    let pid = child.id();
    let _held = sentinel(tmp.path(), "a", &pid.to_string(), true);
    let got = sentinel_target(tmp.path(), "a");
    kill(child);
    assert_eq!(got, Some(pid));
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn flocked_with_a_runtime_launched_through_the_agent_symlink_targets_it() {
    let tmp = tempfile::tempdir().unwrap();
    // The Hub would launch `cat`; this runtime came in through the link.
    let _env = runtime_is(tmp.path(), CAT);
    let link = tmp
        .path()
        .join("links")
        .join(mur_common::agent_bin::link_name("a"));
    std::os::unix::fs::symlink(SLEEP, &link).unwrap();
    let child = spawn_live(&link, &["30"]);
    let pid = child.id();
    let _held = sentinel(tmp.path(), "a", &pid.to_string(), true);
    let got = sentinel_target(tmp.path(), "a");
    kill(child);
    assert_eq!(got, Some(pid));
}

#[test]
fn flocked_with_a_live_pid_that_is_not_the_runtime_targets_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let _env = runtime_is(tmp.path(), CAT);
    let child = spawn_live(Path::new(SLEEP), &["30"]);
    let pid = child.id();
    let _held = sentinel(tmp.path(), "a", &pid.to_string(), true);
    let got = sentinel_target(tmp.path(), "a");
    kill(child);
    assert_eq!(got, None);
}

#[test]
fn flocked_and_empty_targets_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let _held = sentinel(tmp.path(), "a", "", true);
    assert_eq!(sentinel_target(tmp.path(), "a"), None);
}

#[test]
fn free_with_a_pid_targets_nothing_and_is_truncated() {
    let tmp = tempfile::tempdir().unwrap();
    let child = spawn_live(Path::new(SLEEP), &["30"]);
    let pid = child.id();
    sentinel(tmp.path(), "a", &pid.to_string(), false);
    let got = sentinel_target(tmp.path(), "a");
    kill(child);
    assert_eq!(got, None);
    let path = tmp.path().join("agents/a").join(SENTINEL_FILE);
    assert_eq!(std::fs::read_to_string(path).unwrap(), "");
}

#[test]
fn no_sentinel_targets_nothing_and_creates_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("agents/a")).unwrap();
    assert_eq!(sentinel_target(tmp.path(), "a"), None);
    assert!(!tmp.path().join("agents/a").join(SENTINEL_FILE).exists());
}

/// The wiring: no `running.lock`, only a flocked sentinel naming the
/// runtime → `stop_unowned_runtime` signals it instead of returning.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn stop_unowned_runtime_signals_a_booting_runtime() {
    let tmp = tempfile::tempdir().unwrap();
    let _env = runtime_is(tmp.path(), SLEEP);
    let mut child = spawn_live(Path::new(SLEEP), &["30"]);
    let pid = child.id();
    let _held = sentinel(tmp.path(), "a", &pid.to_string(), true);
    // Reap as soon as it dies, or the zombie keeps `pid_alive` true.
    let reaper = std::thread::spawn(move || child.wait().unwrap());
    super::super::stop_unowned_runtime(tmp.path(), "a");
    let status = reaper.join().unwrap();
    assert!(!status.success(), "the booting runtime was signalled");
}

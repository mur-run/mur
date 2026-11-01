//! Review #2253: the agent-writable `ssh/` dir (P1) and concurrent commands
//! on one agent (P3).

use std::fs;
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use mur_common::agent::known_hosts_path;
use mur_common::entitlements_pin::{self, PinCheck};

use super::edit::RevokeScope;
use super::tests::{fp, named_grant, on_disk, write_agent};
use super::*;

/// How long a blocked command is given to (wrongly) get past the lock.
const RACE_WINDOW: Duration = Duration::from_millis(300);

fn setup() -> (mur_common::test_env::EnvGuard, tempfile::TempDir) {
    let mut env = mur_common::test_env::EnvGuard::hold();
    let tmp = tempfile::tempdir().unwrap();
    env.set_var("MUR_HOME", tmp.path());
    write_agent(tmp.path(), "bot");
    (env, tmp)
}

fn allow_named(host: char, name: &str) -> anyhow::Result<()> {
    commit::apply("bot", |e| {
        edit::allow(e, &named_grant('A', host, name));
        Ok(())
    })
}

fn staged_leftovers(ssh_dir: &Path) -> Vec<String> {
    fs::read_dir(ssh_dir)
        .map(|d| {
            d.filter_map(|e| e.ok()?.file_name().into_string().ok())
                .filter(|n| n.starts_with(stage::STAGED_PREFIX_FOR_TESTS))
                .collect()
        })
        .unwrap_or_default()
}

/// P1: the old fixed staging name, pre-planted as a symlink to a user file,
/// must not be written through. The write goes to a fresh name instead.
#[cfg(unix)]
#[test]
fn a_planted_symlink_at_the_old_staging_name_is_not_followed() {
    let (_env, tmp) = setup();
    let home = tmp.path();
    let ssh = home.join("agents/bot/ssh");
    fs::create_dir_all(&ssh).unwrap();
    let victim = home.join("victim.txt");
    fs::write(&victim, "precious").unwrap();
    std::os::unix::fs::symlink(&victim, ssh.join("known_hosts.mur-staged")).unwrap();

    allow_named('H', "gh.example").unwrap();

    assert_eq!(fs::read_to_string(&victim).unwrap(), "precious");
    assert!(
        staged_leftovers(&ssh).is_empty(),
        "staged file installed, not left"
    );
}

/// P1: a symlink at `known_hosts` itself is replaced, never written through.
#[cfg(unix)]
#[test]
fn a_symlinked_known_hosts_is_replaced_not_followed() {
    let (_env, tmp) = setup();
    let home = tmp.path();
    let kh = known_hosts_path(&home.join("agents/bot"));
    fs::create_dir_all(kh.parent().unwrap()).unwrap();
    let victim = home.join("victim.txt");
    fs::write(&victim, "precious").unwrap();
    std::os::unix::fs::symlink(&victim, &kh).unwrap();

    allow_named('H', "gh.example").unwrap();

    assert_eq!(fs::read_to_string(&victim).unwrap(), "precious");
    assert!(!fs::symlink_metadata(&kh).unwrap().file_type().is_symlink());
    assert!(fs::read_to_string(&kh).unwrap().contains("gh.example"));
}

/// P1: `ssh/` itself swapped for a symlink to a user directory is refused
/// before anything is written, and the profile and pin are untouched.
#[cfg(unix)]
#[test]
fn a_symlinked_ssh_dir_is_refused_and_nothing_changes() {
    let (_env, tmp) = setup();
    let home = tmp.path();
    let elsewhere = home.join("user-dir");
    fs::create_dir_all(&elsewhere).unwrap();
    fs::write(elsewhere.join("known_hosts"), "precious").unwrap();
    std::os::unix::fs::symlink(&elsewhere, home.join("agents/bot/ssh")).unwrap();
    let profile = home.join("agents/bot/profile.yaml");
    let before = fs::read(&profile).unwrap();

    let err = allow_named('H', "gh.example").unwrap_err();

    assert!(
        format!("{err:#}").contains("not a plain directory"),
        "{err:#}"
    );
    assert_eq!(fs::read(&profile).unwrap(), before);
    assert_eq!(
        fs::read_to_string(elsewhere.join("known_hosts")).unwrap(),
        "precious"
    );
    assert_eq!(
        fs::read_dir(&elsewhere).unwrap().count(),
        1,
        "nothing staged there"
    );
}

/// P3: a second command waits for the lock instead of loading the profile
/// while the first one is mid-write.
#[test]
fn a_second_command_waits_for_the_lock() {
    let (_env, tmp) = setup();
    let held = commit::hold_lock(tmp.path(), "bot").unwrap();
    let (tx, rx) = mpsc::channel();
    let t = std::thread::spawn(move || tx.send(allow_named('H', "gh.example")).unwrap());

    assert!(
        rx.recv_timeout(RACE_WINDOW).is_err(),
        "the command ran while another held the lock"
    );
    drop(held);
    rx.recv().unwrap().unwrap();
    t.join().unwrap();
    assert_eq!(on_disk(tmp.path(), "bot").entitlements.ssh.keys.len(), 1);
}

/// P3, the lost update itself: an allow started while a revoke is mid-edit
/// must not bring the revoked grant back, and the pin must match the result.
#[test]
fn an_allow_racing_a_revoke_does_not_restore_the_revoked_grant() {
    let (_env, tmp) = setup();
    let home = tmp.path();
    allow_named('H', "gh.example").unwrap();

    let mut other = None;
    commit::apply("bot", |e| {
        // Started after this command loaded the profile, before it saves.
        other = Some(std::thread::spawn(|| allow_named('I', "gl.example")));
        std::thread::sleep(RACE_WINDOW);
        edit::revoke(e, &RevokeScope::new(Some(fp('A')), Some(fp('H'))).unwrap()).map(|_| ())
    })
    .unwrap();
    other.unwrap().join().unwrap().unwrap();

    let p = on_disk(home, "bot");
    let hosts: Vec<&str> = p.entitlements.ssh.keys[0]
        .allow_hosts
        .iter()
        .map(|h| h.host_key.as_str())
        .collect();
    assert_eq!(hosts, [fp('I')], "revoked H stays revoked, I was added");
    assert_eq!(
        entitlements_pin::check(home, "bot", &p.entitlements).unwrap(),
        PinCheck::Match
    );
    let kh = fs::read_to_string(known_hosts_path(&home.join("agents/bot"))).unwrap();
    assert!(
        !kh.contains("gh.example") && kh.contains("gl.example"),
        "{kh}"
    );
}

//! Whether a dedicated key is offered (K3, K4), and `generate_with`: it
//! refuses on a stale plan and never writes the profile (K2).

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

use anyhow::Result;
use mur_agent_runtime::sandbox::policy::ExecBackend;
use mur_common::agent::{AgentProfile, Entitlements};
use mur_ssh_request::keystore::{PARTIAL_MIN_AGE, PARTIAL_PREFIX};
use mur_ssh_request::{KEYGEN_BIN, RequestHost};

use super::super::resolve::ScannedKey;
use super::super::tests::{fp, write_agent};
use super::dedicated::{self, dedicated_for};
use super::front::{generate_refusal, generate_with};
use super::plan::{Dedicated, DedicatedOff, plan};
use super::probe::{AgentKey, Probe};

const AGENT: &str = "bot";
const SEALED: Option<ExecBackend> = Some(ExecBackend::Seatbelt);

/// These tests run the real `ssh-keygen`. Without it they skip, loudly.
pub(super) fn keygen_missing() -> bool {
    let missing = Command::new(KEYGEN_BIN)
        .arg("-?")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_err();
    if missing {
        eprintln!("SKIPPED: {KEYGEN_BIN} is not on PATH");
    }
    missing
}

fn ent(fail_closed: bool) -> Entitlements {
    let mut e = AgentProfile::default_for_tests().entitlements;
    e.fail_closed_on_sandbox_error = fail_closed;
    e
}

/// A leftover generation directory old enough for the sweep to take.
fn old_partial(home: &Path) -> std::path::PathBuf {
    let dir = dedicated::key_dir(home, AGENT).unwrap();
    fs::create_dir_all(&dir).unwrap();
    let partial = dir.join(format!("{PARTIAL_PREFIX}old"));
    fs::create_dir(&partial).unwrap();
    fs::File::open(&partial)
        .unwrap()
        .set_modified(SystemTime::now() - PARTIAL_MIN_AGE - Duration::from_secs(1))
        .unwrap();
    partial
}

#[test]
fn the_switch_and_the_seal_decide_before_any_key_is_read() {
    let home = tempfile::tempdir().unwrap();
    assert_eq!(
        dedicated_for(home.path(), AGENT, &ent(true), false, SEALED),
        Dedicated::Off(DedicatedOff::SwitchOff)
    );
    assert_eq!(
        dedicated_for(home.path(), AGENT, &ent(false), true, SEALED),
        Dedicated::Off(DedicatedOff::NotSealedEveryRun)
    );
    assert_eq!(
        dedicated_for(home.path(), AGENT, &ent(true), true, None),
        Dedicated::Off(DedicatedOff::NotSealedEveryRun),
        "no kernel sandbox on this platform"
    );
    assert_eq!(
        dedicated_for(home.path(), AGENT, &ent(true), true, SEALED),
        Dedicated::Offer
    );
}

#[test]
fn a_stored_key_is_reported_with_its_fingerprint() {
    if keygen_missing() {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let key = dedicated::generate(home.path(), AGENT).unwrap();
    assert_eq!(
        dedicated_for(home.path(), AGENT, &ent(true), true, SEALED),
        Dedicated::Stored {
            fingerprint: key.fingerprint
        }
    );
}

#[test]
fn the_sweep_runs_only_when_a_key_is_offered() {
    let home = tempfile::tempdir().unwrap();
    let partial = old_partial(home.path());

    dedicated_for(home.path(), AGENT, &ent(true), false, SEALED);
    assert!(partial.exists(), "swept with the switch off");
    dedicated_for(home.path(), AGENT, &ent(false), true, SEALED);
    assert!(partial.exists(), "swept for an unsealed agent");

    assert_eq!(
        dedicated_for(home.path(), AGENT, &ent(true), true, SEALED),
        Dedicated::Offer
    );
    assert!(!partial.exists(), "the offered branch sweeps");
}

#[test]
fn a_file_that_is_not_this_stores_key_turns_the_offer_off() {
    let home = tempfile::tempdir().unwrap();
    let dir = dedicated::key_dir(home.path(), AGENT).unwrap();
    fs::create_dir_all(&dir).unwrap();
    std::os::unix::fs::symlink(home.path(), dir.join(mur_ssh_request::keystore::KEY_FILE)).unwrap();
    let got = dedicated_for(home.path(), AGENT, &ent(true), true, SEALED);
    assert!(
        matches!(got, Dedicated::Off(DedicatedOff::Unusable(_))),
        "{got:?}"
    );
}

struct TwoKeys;

impl Probe for TwoKeys {
    fn agent_keys(&self) -> Vec<AgentKey> {
        vec![AgentKey {
            fingerprint: fp('D'),
            comment: "mur-agent-bot".into(),
            path: None,
        }]
    }
    fn unloaded_keys(&self) -> Vec<AgentKey> {
        vec![]
    }
    fn scan(&self, _: &RequestHost) -> Result<Vec<ScannedKey>> {
        Ok(vec![])
    }
    fn stored_host_keys(&self, _: &RequestHost) -> Vec<String> {
        vec![]
    }
    fn user_known_hosts(&self) -> Option<std::path::PathBuf> {
        None
    }
}

/// K4: with the switch off, a key that is already loaded stays an ordinary
/// loaded key the user can grant.
#[test]
fn the_switch_does_not_hide_a_loaded_key() {
    let home = tempfile::tempdir().unwrap();
    let p = plan(
        &TwoKeys,
        AGENT,
        home.path(),
        &ent(true),
        RequestHost::parse("gh.example", None).unwrap(),
        None,
        Dedicated::Off(DedicatedOff::SwitchOff),
    )
    .unwrap();
    assert_eq!(p.dedicated, Dedicated::Off(DedicatedOff::SwitchOff));
    assert_eq!(p.keys_loaded.len(), 1);
    assert_eq!(p.keys_loaded[0].fingerprint, fp('D'));
}

fn profile_bytes(home: &Path) -> Vec<u8> {
    fs::read(home.join("agents").join(AGENT).join("profile.yaml")).unwrap()
}

#[test]
fn generate_refuses_on_a_stale_plan_and_makes_no_key() {
    let home = tempfile::tempdir().unwrap();
    write_agent(home.path(), AGENT);
    let before = profile_bytes(home.path());
    let never = |_: &dedicated::StoredKey| -> Result<(), String> { panic!("nothing to load") };

    assert_eq!(
        generate_refusal(&ent(true), false, SEALED),
        Some(DedicatedOff::SwitchOff)
    );
    let e = generate_with(home.path(), AGENT, &ent(true), false, SEALED, never).unwrap_err();
    assert!(e.contains("ssh_setup.generate_key: false"), "{e}");

    assert_eq!(
        generate_refusal(&ent(false), true, SEALED),
        Some(DedicatedOff::NotSealedEveryRun)
    );
    let e = generate_with(home.path(), AGENT, &ent(false), true, SEALED, never).unwrap_err();
    assert!(e.contains("not sandboxed on every run"), "{e}");

    assert_eq!(dedicated::stored(home.path(), AGENT).unwrap(), None);
    assert_eq!(profile_bytes(home.path()), before);
}

#[test]
fn generate_loads_the_key_and_never_writes_the_profile() {
    if keygen_missing() {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    write_agent(home.path(), AGENT);
    let before = profile_bytes(home.path());
    let loaded = std::cell::RefCell::new(Vec::new());

    let key = generate_with(home.path(), AGENT, &ent(true), true, SEALED, |k| {
        loaded.borrow_mut().push(k.fingerprint.clone());
        Ok(())
    })
    .unwrap();

    let stored = dedicated::stored(home.path(), AGENT).unwrap().unwrap();
    assert_eq!(key.fingerprint, stored.fingerprint);
    assert_eq!(key.comment, format!("mur-agent-{AGENT}"));
    assert_eq!(key.path, None, "the private key path is never handed out");
    assert_eq!(*loaded.borrow(), vec![stored.fingerprint]);
    assert_eq!(
        profile_bytes(home.path()),
        before,
        "generating grants nothing"
    );
}

#[test]
fn a_load_failure_says_the_key_exists() {
    if keygen_missing() {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    write_agent(home.path(), AGENT);
    let before = profile_bytes(home.path());

    let e = generate_with(home.path(), AGENT, &ent(true), true, SEALED, |_| {
        Err("no agent".into())
    })
    .unwrap_err();

    assert!(
        e.contains("exists but could not be added to ssh-agent"),
        "{e}"
    );
    assert!(e.contains("no agent"), "{e}");
    assert!(!e.contains("  "), "a line continuation left a gap: {e:?}");
    assert!(dedicated::stored(home.path(), AGENT).unwrap().is_some());
    assert_eq!(profile_bytes(home.path()), before);
}

#[test]
fn removing_an_agent_leaves_no_key_directory() {
    if keygen_missing() {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    assert_eq!(dedicated::remove(home.path(), AGENT), Ok(None), "no key");
    dedicated::generate(home.path(), AGENT).unwrap();
    let dir = dedicated::key_dir(home.path(), AGENT).unwrap();

    let note = dedicated::remove(home.path(), AGENT).unwrap();

    assert!(note.is_some_and(|n| n.contains("dedicated ssh key")));
    assert!(!dir.exists());
}

#[test]
fn renaming_an_agent_moves_its_key() {
    if keygen_missing() {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    assert_eq!(dedicated::rename(home.path(), AGENT, "next"), Ok(false));
    let key = dedicated::generate(home.path(), AGENT).unwrap();

    assert_eq!(dedicated::rename(home.path(), AGENT, "next"), Ok(true));

    assert_eq!(dedicated::stored(home.path(), AGENT).unwrap(), None);
    let moved = dedicated::stored(home.path(), "next").unwrap().unwrap();
    assert_eq!(moved.fingerprint, key.fingerprint);
}

#[test]
fn the_comment_is_the_third_field() {
    assert_eq!(dedicated::comment_of("ssh-ed25519 AAAA me@host"), "me@host");
    assert_eq!(
        dedicated::comment_of("ssh-ed25519 AAAA two words "),
        "two words"
    );
    assert_eq!(dedicated::comment_of("ssh-ed25519 AAAA"), "");
}

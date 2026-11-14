//! `apply_and_restart` step 4.3: the install runs after the profile write and
//! before the stop, and a failed install holds the restart until the user
//! retries or restarts anyway.

use std::cell::RefCell;
use std::fs;
use std::path::Path;
use std::rc::Rc;

use anyhow::{Result, anyhow};

use super::super::allow::install_preview;
use super::super::edit::HostGrant;
use super::super::tests::{fp, on_disk};
use super::apply::{
    Approval, InstallOutcome, InstallTarget, Lifecycle, Restart, apply_and_restart,
};
use super::dedicated;
use super::dedicated_tests::keygen_missing;
use super::front::{finish_with, retry_install};
use super::install::{InstallRequest, Installed, Installer};
use super::preapprove::Preapproval;
use super::tests::{AGENT, HOST, Home, expect_preapproval, scanned};

type Log = Rc<RefCell<Vec<&'static str>>>;

struct LogLife {
    pid: Option<u32>,
    log: Log,
}

impl Lifecycle for LogLife {
    fn running_pid(&self) -> Option<u32> {
        self.pid
    }
    fn stop(&mut self, _pid: u32) -> Result<()> {
        self.log.borrow_mut().push("stop");
        self.pid = None;
        Ok(())
    }
    fn start(&mut self) -> Result<()> {
        self.log.borrow_mut().push("start");
        Ok(())
    }
}

struct FakeInstaller {
    home: std::path::PathBuf,
    log: Log,
    fail: bool,
    seen: RefCell<Vec<Seen>>,
}

/// One install call as the installer saw it.
struct Seen {
    user: String,
    host: String,
    public_line: String,
    known_hosts: String,
    /// The profile already held the grant.
    granted: bool,
}

impl Installer for FakeInstaller {
    fn install(&self, r: &InstallRequest) -> Result<Installed> {
        self.log.borrow_mut().push("install");
        self.seen.borrow_mut().push(Seen {
            user: r.user.to_string(),
            host: r.host.canonical(),
            public_line: r.public_line.to_string(),
            known_hosts: r.known_hosts.to_string(),
            granted: granted(&self.home, &self.key_fp()),
        });
        if self.fail {
            return Err(anyhow!("Permission denied (publickey)"));
        }
        Ok(Installed::Added)
    }
}

impl FakeInstaller {
    fn key_fp(&self) -> String {
        dedicated::stored(&self.home, AGENT)
            .unwrap()
            .unwrap()
            .fingerprint
    }
}

fn granted(home: &Path, key_fp: &str) -> bool {
    on_disk(home, AGENT)
        .entitlements
        .ssh
        .keys
        .iter()
        .any(|k| k.fingerprint == key_fp && k.allow_hosts.iter().any(|h| h.host_key == fp('H')))
}

fn target() -> InstallTarget {
    InstallTarget {
        user: "git".into(),
        host: mur_ssh_request::RequestHost::parse(HOST, None).unwrap(),
    }
}

fn approval(key_fp: &str, restart: bool) -> Approval {
    Approval {
        key_fp: key_fp.to_string(),
        key_label: None,
        hosts: vec![HostGrant {
            host_key_fp: fp('H'),
            label: Some(HOST.into()),
            known_host: Some(scanned('H').entry),
        }],
        spawn: None,
        spawn_consented: false,
        restart,
        install: Some(target()),
    }
}

struct Rig {
    _h: Home,
    home: std::path::PathBuf,
    log: Log,
    key: dedicated::StoredKey,
}

impl Rig {
    fn new() -> Self {
        let (_h, home) = Home::new();
        let key = dedicated::generate(&home, AGENT).unwrap();
        Self {
            _h,
            home,
            log: Log::default(),
            key,
        }
    }
    fn life(&self) -> LogLife {
        LogLife {
            pid: Some(42),
            log: self.log.clone(),
        }
    }
    fn installer(&self, fail: bool) -> FakeInstaller {
        FakeInstaller {
            home: self.home.clone(),
            log: self.log.clone(),
            fail,
            seen: RefCell::default(),
        }
    }
    fn profile_bytes(&self) -> Vec<u8> {
        fs::read(self.home.join("agents").join(AGENT).join("profile.yaml")).unwrap()
    }
}

#[test]
fn the_install_runs_after_the_write_and_before_the_stop() {
    if keygen_missing() {
        return;
    }
    let rig = Rig::new();
    let inst = rig.installer(false);
    let a = approval(&rig.key.fingerprint, true);

    let got = apply_and_restart(&rig.home, AGENT, 0, &a, &mut rig.life(), &inst).unwrap();

    assert_eq!(*rig.log.borrow(), vec!["install", "stop", "start"]);
    let seen = inst.seen.borrow();
    let s = &seen[0];
    assert_eq!(s.user, "git");
    assert_eq!(s.host, target().host.canonical());
    assert_eq!(s.public_line, rig.key.public_line);
    assert_eq!(s.known_hosts, format!("{HOST} ssh-ed25519 AAAAH\n"));
    assert!(s.granted, "the profile held the grant when the install ran");
    assert_eq!(got.restart, Restart::Restarted);
    expect_preapproval(&rig.home, &got.preapproval);
    assert_eq!(
        got.install.line().unwrap(),
        format!(
            "✓ public key installed for git on {}",
            target().host.canonical()
        )
    );
}

#[test]
fn a_failed_install_keeps_the_grant_and_holds_the_restart() {
    if keygen_missing() {
        return;
    }
    let rig = Rig::new();
    let inst = rig.installer(true);
    let a = approval(&rig.key.fingerprint, true);

    let got = apply_and_restart(&rig.home, AGENT, 0, &a, &mut rig.life(), &inst).unwrap();

    assert_eq!(
        *rig.log.borrow(),
        vec!["install"],
        "never stopped or started"
    );
    assert!(granted(&rig.home, &rig.key.fingerprint));
    assert_eq!(got.preapproval, Preapproval::NotAttempted);
    assert_eq!(got.restart, Restart::HeldForInstall);
    assert!(
        got.restart.line(AGENT).is_none(),
        "the install line says it"
    );
    let line = got.install.line().unwrap();
    assert!(line.starts_with("Granted, key not installed: "), "{line}");
    assert!(line.contains("Permission denied"), "{line}");
    assert!(line.ends_with("Nothing was restarted."), "{line}");
}

#[test]
fn a_retry_installs_once_then_restarts_without_another_profile_write() {
    if keygen_missing() {
        return;
    }
    let rig = Rig::new();
    let a = approval(&rig.key.fingerprint, true);
    apply_and_restart(
        &rig.home,
        AGENT,
        0,
        &a,
        &mut rig.life(),
        &rig.installer(true),
    )
    .unwrap();
    rig.log.borrow_mut().clear();
    let before = rig.profile_bytes();
    let inst = rig.installer(false);

    let got = retry_install(&rig.home, AGENT, 0, &a, &mut rig.life(), &inst).unwrap();

    assert_eq!(*rig.log.borrow(), vec!["install", "stop", "start"]);
    assert_eq!(
        rig.profile_bytes(),
        before,
        "the profile is not written again"
    );
    assert!(matches!(got.install, InstallOutcome::Done { .. }));
    assert_eq!(got.restart, Restart::Restarted);
    expect_preapproval(&rig.home, &got.preapproval);
}

#[test]
fn a_retry_that_fails_again_still_holds() {
    if keygen_missing() {
        return;
    }
    let rig = Rig::new();
    let a = approval(&rig.key.fingerprint, true);
    apply_and_restart(
        &rig.home,
        AGENT,
        0,
        &a,
        &mut rig.life(),
        &rig.installer(true),
    )
    .unwrap();
    rig.log.borrow_mut().clear();

    let got = retry_install(
        &rig.home,
        AGENT,
        0,
        &a,
        &mut rig.life(),
        &rig.installer(true),
    )
    .unwrap();

    assert_eq!(*rig.log.borrow(), vec!["install"]);
    assert_eq!(got.restart, Restart::HeldForInstall);
    assert_eq!(got.preapproval, Preapproval::NotAttempted);
}

#[test]
fn restart_anyway_preapproves_and_never_installs() {
    if keygen_missing() {
        return;
    }
    let rig = Rig::new();
    let a = approval(&rig.key.fingerprint, true);
    apply_and_restart(
        &rig.home,
        AGENT,
        0,
        &a,
        &mut rig.life(),
        &rig.installer(true),
    )
    .unwrap();
    rig.log.borrow_mut().clear();

    let got = finish_with(
        &rig.home,
        AGENT,
        0,
        &a,
        &mut rig.life(),
        InstallOutcome::NotChosen,
    )
    .unwrap();

    assert_eq!(*rig.log.borrow(), vec!["stop", "start"], "no install call");
    assert_eq!(got.restart, Restart::Restarted);
    expect_preapproval(&rig.home, &got.preapproval);
}

#[test]
fn a_profile_write_that_fails_installs_nothing() {
    if keygen_missing() {
        return;
    }
    let rig = Rig::new();
    let inst = rig.installer(false);
    let a = approval(&rig.key.fingerprint, true);

    // No such agent: the locked profile write fails.
    let e = apply_and_restart(&rig.home, "ghost", 0, &a, &mut rig.life(), &inst);

    assert!(e.is_err());
    assert!(rig.log.borrow().is_empty(), "no key has been installed yet");
}

#[test]
fn only_the_dedicated_key_is_ever_installed() {
    if keygen_missing() {
        return;
    }
    let rig = Rig::new();
    let inst = rig.installer(false);
    // One of the user's own keys, with an install target forced onto it.
    let a = approval(&fp('A'), true);

    let got = apply_and_restart(&rig.home, AGENT, 0, &a, &mut rig.life(), &inst).unwrap();

    assert!(
        rig.log.borrow().is_empty(),
        "the installer was never reached"
    );
    assert!(
        matches!(&got.install, InstallOutcome::Failed(e) if e.contains("not the dedicated key")),
        "{:?}",
        got.install
    );
    assert_eq!(got.restart, Restart::HeldForInstall);
}

#[test]
fn no_restart_still_installs() {
    if keygen_missing() {
        return;
    }
    let rig = Rig::new();
    let inst = rig.installer(false);
    let a = approval(&rig.key.fingerprint, false);

    let got = apply_and_restart(&rig.home, AGENT, 0, &a, &mut rig.life(), &inst).unwrap();

    assert_eq!(*rig.log.borrow(), vec!["install"]);
    assert!(matches!(got.install, InstallOutcome::Done { .. }));
    assert_eq!(got.restart, Restart::Skipped);
    assert_eq!(got.preapproval, Preapproval::NotAttempted);
}

#[test]
fn the_preview_shows_the_line_and_where_it_goes() {
    let line = "ssh-ed25519 AAAAkey mur-agent-bot";
    let text = install_preview(&target(), line);
    assert!(
        text.contains(&format!("git@{}", target().host.canonical())),
        "{text}"
    );
    assert!(text.contains(&format!("\n    {line}\n")), "{text}");
    assert!(text.contains("authorized_keys"), "{text}");
    assert!(text.contains("your own keys"), "{text}");
}

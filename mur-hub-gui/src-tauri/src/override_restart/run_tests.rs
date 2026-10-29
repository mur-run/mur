//! The command bodies against a temp home and a fake runtime. The fake boot
//! mirrors `mur-core`'s private `test_support::boot`: it claims the live file
//! and records the nonce in a fresh `running.lock`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};
use std::time::Duration;

use mur_common::LockFile;
use mur_common::agent::{OverrideFlags, OverrideRecord};
use mur_common::startup_override::OverrideFile;
use mur_common::startup_override::claim::{ClaimAs, Claimed, claim, live_path, overrides_dir};
use mur_core::cmd::agent::{Launcher, OverrideOutcome, RestartNote, StopFailed, outcome_text};
use serde_json::json;

use super::dialog::HubConfirmation;
use super::jobs::{JobState, OverrideJobs};
use super::*;

const WINDOW: &str = "dashboard";

fn flags() -> OverrideFlags {
    OverrideFlags {
        no_hitl: true,
        ..Default::default()
    }
}

/// A unique agent name per test, so no real service unit matches it.
fn unique(tag: &str) -> String {
    format!("hub-ovr-{tag}-{}", std::process::id())
}

struct Home {
    /// Held for the test: the launcher tests point `MUR_AGENT_RUNTIME_BIN`
    /// at a `sleep 30` script, and the normal restart runs
    /// `<runtime> --build-id`, which would block on it. Unset here, so the
    /// build-id probe finds no binary and reads `unknown`.
    _env: mur_common::test_env::EnvGuard,
    tmp: tempfile::TempDir,
    name: String,
    /// The "old" runtime: alive (check R) and not this process, so the ack
    /// wait can tell the fresh lock (our pid) from the old one.
    old: std::process::Child,
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = self.old.kill();
        let _ = self.old.wait();
    }
}

impl Home {
    fn new(tag: &str) -> Self {
        let env = mur_common::test_env::EnvGuard::unset(["MUR_AGENT_RUNTIME_BIN"]);
        let tmp = tempfile::tempdir().unwrap();
        let name = unique(tag);
        let dir = tmp.path().join("agents").join(&name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("profile.yaml"), format!("name: {name}\n")).unwrap();
        let old = std::process::Command::new("sleep")
            .arg("300")
            .spawn()
            .unwrap();
        let h = Self {
            _env: env,
            tmp,
            name,
            old,
        };
        h.write_lock(base_lock(&h.name, h.old.id()));
        h
    }
    fn path(&self) -> &Path {
        self.tmp.path()
    }
    fn dir(&self) -> PathBuf {
        overrides_dir(self.path())
    }
    fn write_lock(&self, lock: LockFile) {
        let p = self
            .path()
            .join("agents")
            .join(&self.name)
            .join("running.lock");
        std::fs::write(p, serde_json::to_vec(&lock).unwrap()).unwrap();
    }
    fn audit(&self) -> String {
        std::fs::read_to_string(
            self.path()
                .join("agents")
                .join(&self.name)
                .join("permissions/audit.jsonl"),
        )
        .unwrap_or_default()
    }
    fn ctx(&self, jobs: &OverrideJobs) -> Ctx {
        Ctx {
            home: self.path().to_owned(),
            name: self.name.clone(),
            window: WINDOW.into(),
            jobs: jobs.clone(),
        }
    }
    /// Files in the overrides dir other than writer locks.
    fn override_files(&self) -> Vec<String> {
        std::fs::read_dir(self.dir())
            .map(|it| {
                it.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|n| !n.ends_with(".lock"))
                    .collect()
            })
            .unwrap_or_default()
    }
}

fn base_lock(name: &str, pid: u32) -> LockFile {
    serde_json::from_value(json!({
        "schema": 1, "uuid": "u", "name": name, "pid": pid, "ppid": 1,
        "started_at": "2026-10-29T00:00:00Z", "binary_version": "0.0.0",
        "transports": {"stdio": true, "unix_socket": null, "tcp": null},
        "card_digest": "d", "capabilities": [], "build_sha": "unknown", "proto_version": 1,
    }))
    .unwrap()
}

/// The runtime boots and applies whatever live file it finds.
fn boot_applies(home: &Path, name: &str) {
    let mut lock = base_lock(name, std::process::id());
    if let Claimed::Took(path) = claim(&overrides_dir(home), name, ClaimAs::Consume) {
        let file: OverrideFile = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        lock.startup_override = Some(OverrideRecord {
            flags: file.flags(),
            nonce: file.nonce,
            confirmed_by: file.confirmed_by,
        });
    }
    let p = home.join("agents").join(name).join("running.lock");
    std::fs::write(p, serde_json::to_vec(&lock).unwrap()).unwrap();
}

#[derive(Default)]
struct Fake {
    home: PathBuf,
    name: String,
    panic_in_stop: bool,
    /// `start` waits here, then fails (the quit path took the file meanwhile).
    start_gate: Option<(Arc<Barrier>, Arc<Barrier>)>,
}

impl Launcher for Fake {
    fn stop(&mut self, _old: u32, _wait: Duration) -> Result<(), StopFailed> {
        assert!(!self.panic_in_stop, "injected panic in stop");
        Ok(())
    }
    fn start(&mut self) -> anyhow::Result<Option<u32>> {
        if let Some((reached, release)) = &self.start_gate {
            reached.wait();
            release.wait();
            anyhow::bail!("the Hub is quitting");
        }
        boot_applies(&self.home, &self.name);
        Ok(Some(std::process::id()))
    }
    fn kick(&mut self, _note: &mut dyn FnMut(RestartNote)) -> anyhow::Result<Option<u32>> {
        Ok(None)
    }
    fn fresh_pid(&self, _old: Option<u32>) -> Option<u32> {
        None
    }
}

fn fake(home: &Home) -> Fake {
    Fake {
        home: home.path().to_owned(),
        name: home.name.clone(),
        ..Default::default()
    }
}

fn accept(_: &Prepared) -> Option<HubConfirmation> {
    Some(HubConfirmation::for_test(WINDOW, flags()))
}

#[test]
fn declined_writes_nothing_and_audits() {
    let home = Home::new("decline");
    let jobs = OverrideJobs::default();
    let ctx = home.ctx(&jobs);
    let entry = admit(&ctx, JobState::Dialog).unwrap();
    let err = run_override(ctx, entry, flags(), &mut |_| None, &mut |_| {
        Box::new(fake(&home))
    })
    .unwrap_err();
    assert_eq!(err, "Cancelled; nothing was changed");
    assert!(
        home.override_files().is_empty(),
        "{:?}",
        home.override_files()
    );
    let audit = home.audit();
    assert_eq!(
        audit.matches("startup_override_declined").count(),
        1,
        "{audit}"
    );
    assert!(audit.contains(r#""window":"dashboard""#), "{audit}");
    assert!(jobs.active_names().is_empty());
    // The writer lock was released with `Prepared`.
    let ctx = home.ctx(&jobs);
    let entry = admit(&ctx, JobState::Dialog).unwrap();
    assert!(
        run_override(ctx, entry, flags(), &mut |_| None, &mut |_| Box::new(fake(
            &home
        )))
        .is_err()
    );
    assert_eq!(home.audit().matches("startup_override_declined").count(), 2);
}

#[test]
fn accepted_while_quitting_is_declined() {
    let home = Home::new("quitaccept");
    let jobs = OverrideJobs::default();
    let ctx = home.ctx(&jobs);
    let entry = admit(&ctx, JobState::Dialog).unwrap();
    let j = jobs.clone();
    let err = run_override(
        ctx,
        entry,
        flags(),
        &mut |p| {
            j.begin_quit();
            accept(p)
        },
        &mut |_| Box::new(fake(&home)),
    )
    .unwrap_err();
    assert_eq!(err, "Cancelled; nothing was changed");
    assert!(home.override_files().is_empty());
    assert_eq!(home.audit().matches("startup_override_declined").count(), 1);
}

#[test]
fn a_confirmation_for_other_flags_is_declined() {
    let home = Home::new("otherflags");
    let jobs = OverrideJobs::default();
    let ctx = home.ctx(&jobs);
    let entry = admit(&ctx, JobState::Dialog).unwrap();
    let wider = OverrideFlags {
        full_perms: true,
        ..flags()
    };
    let err = run_override(
        ctx,
        entry,
        flags(),
        &mut |_| Some(HubConfirmation::for_test(WINDOW, wider)),
        &mut |_| Box::new(fake(&home)),
    )
    .unwrap_err();
    assert_eq!(err, "Cancelled; nothing was changed");
    assert!(home.override_files().is_empty());
}

#[test]
fn applied_on_time() {
    let home = Home::new("applied");
    let jobs = OverrideJobs::default();
    let ctx = home.ctx(&jobs);
    let entry = admit(&ctx, JobState::Dialog).unwrap();
    let reply = run_override(ctx, entry, flags(), &mut accept, &mut |_| {
        Box::new(fake(&home))
    })
    .unwrap();
    match &reply.outcome {
        OverrideOutcome::Applied {
            late: false,
            flags: f,
            ..
        } => assert_eq!(*f, flags()),
        other => panic!("{other:?}"),
    }
    assert_eq!(reply.message, outcome_text(&home.name, &reply.outcome));
    assert!(jobs.active_names().is_empty());
    assert!(!live_path(&home.dir(), &home.name).exists());
}

#[test]
fn cli_holds_the_writer_lock() {
    let home = Home::new("cliheld");
    let jobs = OverrideJobs::default();
    // Another issuer already holds the lock through its own prepare.
    let held = prepare(&OverrideRequest {
        home: home.path(),
        name: &home.name,
        mode: Mode::Restart,
        flags: flags(),
        by: IssuedBy::Cli,
    })
    .unwrap();
    let ctx = home.ctx(&jobs);
    let entry = admit(&ctx, JobState::Dialog).unwrap();
    let mut asked = false;
    let err = run_override(
        ctx,
        entry,
        flags(),
        &mut |p| {
            asked = true;
            accept(p)
        },
        &mut |_| Box::new(fake(&home)),
    )
    .unwrap_err();
    assert!(err.contains("another override is pending"), "{err}");
    assert!(
        !asked,
        "the user must not be asked while the CLI holds the lock"
    );
    assert!(jobs.active_names().is_empty());
    drop(held);
}

#[test]
fn a_panic_in_stop_releases_entry_and_lock() {
    let home = Home::new("panic");
    let jobs = OverrideJobs::default();
    let ctx = home.ctx(&jobs);
    let entry = admit(&ctx, JobState::Dialog).unwrap();
    let h = home.path().to_owned();
    let n = home.name.clone();
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_override(ctx, entry, flags(), &mut accept, &mut |_| {
            Box::new(Fake {
                home: h.clone(),
                name: n.clone(),
                panic_in_stop: true,
                ..Default::default()
            })
        })
    }));
    assert!(r.is_err());
    assert!(jobs.active_names().is_empty());
    assert!(
        prepare(&OverrideRequest {
            home: home.path(),
            name: &home.name,
            mode: Mode::Restart,
            flags: flags(),
            by: IssuedBy::Hub,
        })
        .is_ok(),
        "writer lock still held after the panic"
    );
}

#[test]
fn quit_during_start_abandons_and_audits_once() {
    let home = Home::new("quitstart");
    let jobs = OverrideJobs::default();
    let reached = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let ctx = home.ctx(&jobs);
    let entry = admit(&ctx, JobState::Dialog).unwrap();
    let (h, n) = (home.path().to_owned(), home.name.clone());
    let (r1, r2) = (reached.clone(), release.clone());
    let job = std::thread::spawn(move || {
        run_override(ctx, entry, flags(), &mut accept, &mut |_| {
            Box::new(Fake {
                home: h.clone(),
                name: n.clone(),
                start_gate: Some((r1.clone(), r2.clone())),
                ..Default::default()
            })
        })
    });
    reached.wait();
    let claims = jobs.begin_quit();
    assert_eq!(claims.len(), 1, "the published file is claimed");
    quit::run_claims(home.path(), &jobs, claims);
    release.wait();
    let reply = job.join().unwrap().unwrap();
    assert!(
        matches!(reply.outcome, OverrideOutcome::Abandoned { .. }),
        "{:?}",
        reply.outcome
    );
    let audit = home.audit();
    assert_eq!(
        audit.matches("startup_override_withdrawn").count(),
        1,
        "{audit}"
    );
    assert!(jobs.active_names().is_empty());
    assert!(!live_path(&home.dir(), &home.name).exists());
}

#[test]
fn normal_restart_sweeps_an_orphan() {
    let home = Home::new("normal");
    std::fs::create_dir_all(home.dir()).unwrap();
    std::fs::write(
        live_path(&home.dir(), &home.name),
        r#"{"nonce":"ovr-00000000000000000000000000000000"}"#,
    )
    .unwrap();
    let jobs = OverrideJobs::default();
    let ctx = home.ctx(&jobs);
    let entry = admit(&ctx, JobState::Preparing).unwrap();
    let reply = run_normal(ctx, entry, &mut |_| Box::new(fake(&home))).unwrap();
    assert!(reply.ok, "{reply:?}");
    assert_eq!(
        home.audit().matches("startup_override_discarded").count(),
        1
    );
    assert!(!live_path(&home.dir(), &home.name).exists());
    assert!(jobs.active_names().is_empty());
}

#[test]
fn busy_refusals_are_audited() {
    let a = Home::new("busya");
    let jobs = OverrideJobs::default();
    let _open = jobs
        .admit("someone-else", WINDOW, JobState::Dialog)
        .unwrap();
    let err = admit(&a.ctx(&jobs), JobState::Dialog).err().unwrap();
    assert_eq!(err, "an override confirmation is already open");
    let audit = a.audit();
    assert_eq!(
        audit.matches("startup_override_refused").count(),
        1,
        "{audit}"
    );
    assert!(audit.contains(r#""reason":"dialog open""#), "{audit}");

    jobs.set_state("someone-else", JobState::Preparing);
    let _mine = admit(&a.ctx(&jobs), JobState::Preparing).unwrap();
    let err = admit(&a.ctx(&jobs), JobState::Dialog).err().unwrap();
    assert_eq!(
        err,
        format!("an override restart of {} is already in progress", a.name)
    );
    assert!(a.audit().contains(r#""reason":"job in progress""#));
}

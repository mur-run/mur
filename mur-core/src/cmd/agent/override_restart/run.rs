//! The post-confirmation half: sweep, publish, (stop,) start, wait for the
//! acknowledgement, and Cleanup on every non-success path. The writer lock
//! inside `Prepared` is held throughout and released when this returns or
//! unwinds.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::Utc;
use mur_common::permissions::AuditEvent;
use mur_common::startup_override::claim::overrides_dir;
use mur_common::startup_override::{
    OVERRIDE_ACK_WAIT_SECS, OVERRIDE_SCHEMA_VERSION, OverrideFile, is_valid_pair,
};

use super::super::restart::{Launcher, RestartNote, kill_wait_secs};
use super::super::restart_confirm::wait_for_confirmed_lock;
use super::outcome::{Ack, Cleanup, OverrideOutcome, Step, classify};
use super::prepare::check_running;
use super::writer::{self, Swept, Withdrawn};
use super::{Issuer, JobGate, Mode, NOTHING_CHANGED, Prepared, audit};

/// Suffix F (Hub spec §Failure paths), shared by the sweep and Cleanup.
pub(in crate::cmd::agent) const FILE_REMAINS: &str = "a pending override file remains; the next \
     override from the CLI or Hub discards it, and a boot rejects it after `override_max_age`";

const HUB_QUITTING: &str = "hub quitting";

pub fn override_restart(
    prepared: Prepared,
    issuer: Issuer,
    mut gate: Box<dyn JobGate>,
    launcher: &mut dyn Launcher,
    note: &mut dyn FnMut(RestartNote),
) -> OverrideOutcome {
    let job = Job::new(&prepared.home, &prepared.name, issuer.window());
    let name = prepared.name.as_str();

    // ── Step 2: orphan sweep ──────────────────────────────────────────
    if let Err(failed) = job.sweep(note) {
        return failed;
    }

    // ── Check R again for Start, under the lock, just before publish ──
    if prepared.mode == Mode::Start
        && let Err(step) = check_running(&prepared.home, name, Mode::Start)
    {
        let detail = refusal_text(&step);
        return OverrideOutcome::Failed {
            step,
            cleanup: None,
            detail,
        };
    }

    // ── Step 3: publish ───────────────────────────────────────────────
    let Some(permit) = gate.begin_publish() else {
        return OverrideOutcome::Failed {
            step: Step::Cancelled,
            cleanup: None,
            detail: format!("cancelled; {NOTHING_CHANGED}"),
        };
    };
    let (issued_by, confirmed_by) = (issuer.issued_by(), issuer.confirmed_by());
    if !is_valid_pair(issued_by, confirmed_by) {
        return publish_failed("invalid issued_by/confirmed_by pair");
    }
    let nonce = match writer::new_nonce() {
        Ok(n) => n,
        Err(e) => return publish_failed(&e.to_string()),
    };
    let file = OverrideFile {
        version: OVERRIDE_SCHEMA_VERSION,
        nonce: nonce.clone(),
        created_at: Utc::now(),
        issued_by,
        confirmed_by,
        no_hitl: prepared.flags.no_hitl,
        net_unrestricted: prepared.flags.net_unrestricted,
        full_perms: prepared.flags.full_perms,
    };
    if let Err(e) = writer::publish(&job.dir, name, &file) {
        // The permit drops uncommitted: nothing is live.
        return publish_failed(&e.to_string());
    }
    permit.commit(&nonce);
    job.audit(AuditEvent::StartupOverrideIssued {
        ts: Utc::now(),
        nonce: nonce.clone(),
        flags: prepared.flags,
        issued_by,
        confirmed_by,
        window: job.window.clone(),
    });

    // ── Step 4: launch ────────────────────────────────────────────────
    let old_pid = prepared.old_pid.filter(|_| prepared.mode == Mode::Restart);
    if let Some(pid) = old_pid {
        let kill_wait = Duration::from_secs(kill_wait_secs(prepared.stop_timeout_secs));
        if let Err(e) = launcher.stop(pid, kill_wait) {
            let detail = format!("could not stop '{name}': {e}");
            return job.cleanup(&mut *gate, &nonce, Step::Stop, detail);
        }
    }
    if let Err(e) = launcher.start() {
        let detail = format!("'{name}' failed to start: {e:#}");
        return job.cleanup(&mut *gate, &nonce, Step::Start, detail);
    }

    // ── Step 5: acknowledgement, one deadline for both modes ──────────
    let deadline = Instant::now() + Duration::from_secs(OVERRIDE_ACK_WAIT_SECS);
    let waited = wait_for_confirmed_lock(&job.lock_path, old_pid, launcher, Some(deadline), note);
    let fresh = match waited {
        Ok(Some((_pid, lock))) => lock,
        Ok(None) => {
            let (step, detail) = match launcher.exited_early() {
                Some(why) => (Step::Start, format!("'{name}' failed to start: {why}")),
                None => (Step::NoAck, no_ack(name)),
            };
            return job.cleanup(&mut *gate, &nonce, step, detail);
        }
        Err(e) => {
            let detail = format!("'{name}' failed to start: {e:#}");
            return job.cleanup(&mut *gate, &nonce, Step::Start, detail);
        }
    };
    if !gate.begin_cleanup() {
        return abandoned();
    }
    match classify(&fresh, &nonce) {
        Ack::Applied(flags) => OverrideOutcome::Applied {
            flags,
            late: false,
            detail: None,
        },
        Ack::Rejected(reason) => OverrideOutcome::Rejected {
            reason,
            late: false,
            detail: None,
        },
        // This boot did not see our file: Cleanup, gate already passed.
        Ack::Other => job.withdraw_and_classify(&nonce, Step::NoAck, no_ack(name)),
    }
}

/// One agent's override paths and audit sink. Holds no lock: the writer
/// lock lives in `Prepared`, and the Hub quit path runs without one.
pub(super) struct Job {
    home: PathBuf,
    name: String,
    dir: PathBuf,
    lock_path: PathBuf,
    window: Option<String>,
}

impl Job {
    pub(super) fn new(home: &Path, name: &str, window: Option<String>) -> Self {
        Self {
            home: home.to_owned(),
            name: name.to_owned(),
            dir: overrides_dir(home),
            lock_path: home.join("agents").join(name).join("running.lock"),
            window,
        }
    }

    fn audit(&self, event: AuditEvent) {
        audit::append(&self.home, &self.name, &event);
    }

    /// Step 2: withdraw an orphan a crashed writer left live. `Err` is the
    /// outcome to return: the orphan stays and nothing may be published.
    pub(super) fn sweep(&self, note: &mut dyn FnMut(RestartNote)) -> Result<(), OverrideOutcome> {
        let name = self.name.as_str();
        match writer::sweep(&self.dir, name) {
            Ok(Swept::Clear) => Ok(()),
            Ok(Swept::Discarded { nonce }) => {
                note(RestartNote::Info(format!(
                    "discarded an orphaned override for '{name}' ({})",
                    nonce.as_deref().unwrap_or("no readable nonce")
                )));
                self.audit(AuditEvent::StartupOverrideDiscarded {
                    ts: Utc::now(),
                    nonce,
                    window: self.window.clone(),
                });
                Ok(())
            }
            Err(e) => {
                self.audit(AuditEvent::StartupOverrideWithdrawFailed {
                    ts: Utc::now(),
                    nonce: None,
                    error: e.to_string(),
                    window: self.window.clone(),
                });
                Err(OverrideOutcome::Failed {
                    step: Step::Sweep,
                    cleanup: None,
                    detail: format!("{FILE_REMAINS}: {e}"),
                })
            }
        }
    }

    /// Step 6, behind the gate.
    fn cleanup(
        &self,
        gate: &mut dyn JobGate,
        nonce: &str,
        step: Step,
        detail: String,
    ) -> OverrideOutcome {
        if !gate.begin_cleanup() {
            return abandoned();
        }
        self.withdraw_and_classify(nonce, step, detail)
    }

    pub(super) fn withdraw_and_classify(
        &self,
        nonce: &str,
        step: Step,
        detail: String,
    ) -> OverrideOutcome {
        let name = self.name.as_str();
        let failed = |cleanup: Cleanup, detail: String| OverrideOutcome::Failed {
            step: step.clone(),
            cleanup: Some(cleanup),
            detail,
        };
        match writer::withdraw(&self.dir, name, nonce) {
            Withdrawn::Ours => {
                self.audit(AuditEvent::StartupOverrideWithdrawn {
                    ts: Utc::now(),
                    nonce: nonce.to_owned(),
                    reason: detail.clone(),
                    window: self.window.clone(),
                });
                failed(Cleanup::Withdrawn, detail)
            }
            Withdrawn::Foreign { found } => {
                let error = format!(
                    "withdrew an override with nonce {}, not ours ({nonce}); it was left in place",
                    found.as_deref().unwrap_or("(unreadable)")
                );
                self.withdraw_failed(nonce, &error);
                failed(Cleanup::WithdrawFailed { error }, detail)
            }
            Withdrawn::Failed(e) => {
                let error = e.to_string();
                self.withdraw_failed(nonce, &error);
                failed(Cleanup::WithdrawFailed { error }, detail)
            }
            // A boot took it: its lock file is the answer, read once more.
            Withdrawn::Consumed => match self.read_lock().map(|l| classify(&l, nonce)) {
                Some(Ack::Applied(flags)) => OverrideOutcome::Applied {
                    flags,
                    late: true,
                    detail: Some(detail),
                },
                Some(Ack::Rejected(reason)) => OverrideOutcome::Rejected {
                    reason,
                    late: true,
                    detail: Some(detail),
                },
                Some(Ack::Other) | None => {
                    self.audit(AuditEvent::StartupOverrideUnacknowledged {
                        ts: Utc::now(),
                        nonce: nonce.to_owned(),
                        window: self.window.clone(),
                    });
                    failed(Cleanup::ConsumedUnacknowledged, detail)
                }
            },
        }
    }

    fn withdraw_failed(&self, nonce: &str, error: &str) {
        self.audit(AuditEvent::StartupOverrideWithdrawFailed {
            ts: Utc::now(),
            nonce: Some(nonce.to_owned()),
            error: error.to_owned(),
            window: self.window.clone(),
        });
    }

    fn read_lock(&self) -> Option<mur_common::LockFile> {
        read_lock_at(&self.lock_path)
    }
}

fn read_lock_at(path: &Path) -> Option<mur_common::LockFile> {
    mur_common::lock_file::read(path).ok().flatten()
}

fn publish_failed(why: &str) -> OverrideOutcome {
    OverrideOutcome::Failed {
        step: Step::Publish,
        cleanup: None,
        detail: format!("could not write the override ({why}); {NOTHING_CHANGED}"),
    }
}

fn abandoned() -> OverrideOutcome {
    OverrideOutcome::Abandoned {
        reason: HUB_QUITTING.to_owned(),
    }
}

fn no_ack(name: &str) -> String {
    format!("'{name}' did not acknowledge the override")
}

fn refusal_text(step: &Step) -> String {
    match step {
        Step::Refusal { reason } => reason.clone(),
        other => format!("{other:?}"),
    }
}

#[cfg(test)]
#[path = "run_tests.rs"]
mod tests;

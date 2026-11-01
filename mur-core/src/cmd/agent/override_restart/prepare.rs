//! The pre-confirmation half: check H (home), check R (running, per mode),
//! then the writer lock. Nothing is written to `startup-overrides/` except
//! the lock file itself.

use std::path::Path;

use mur_common::LockFile;
use mur_common::lock_file::{SentinelProbe, pid_alive, probe_sentinel};
use mur_common::startup_override::IssuedBy;
use mur_common::startup_override::claim::overrides_dir;

use super::super::restart::stop_timeout_for;
use super::outcome::Step;
use super::writer::{self, LockError};
use super::{Mode, NOTHING_CHANGED, OverrideRequest, Prepared};
use crate::cmd::init_daemon::service_home::descriptor_home;

/// A refusal before anything is written.
pub(in crate::cmd::agent) fn refusal(reason: String) -> Step {
    Step::Refusal { reason }
}

pub fn prepare(req: &OverrideRequest<'_>) -> Result<Prepared, Step> {
    let name = req.name;
    if !req.home.join("agents").join(name).is_dir() {
        return Err(refusal(format!(
            "agent '{name}' not found under {}; {NOTHING_CHANGED}",
            req.home.display()
        )));
    }
    let service = super::super::service::installed_service(name);
    if let Some(unit) = &service {
        check_home(name, req.home, unit, req.by)?;
    }
    let old_pid = check_running(req.home, name, req.mode)?;
    let lock = match writer::lock(&overrides_dir(req.home), name) {
        Ok(l) => l,
        Err(LockError::Busy) => {
            return Err(refusal(format!(
                "another override is pending for '{name}'; {NOTHING_CHANGED}"
            )));
        }
        Err(LockError::Io(e)) => {
            return Err(refusal(format!(
                "could not take the override lock for '{name}': {e}; {NOTHING_CHANGED}"
            )));
        }
    };
    let agent_home = req.home.join("agents").join(name);
    let stop_timeout_secs = stop_timeout_for(name, &agent_home, &mut |_| {});
    Ok(Prepared {
        home: req.home.to_owned(),
        name: name.to_owned(),
        mode: req.mode,
        flags: req.flags,
        has_service: service.is_some(),
        old_pid,
        stop_timeout_secs,
        _lock: lock,
    })
}

/// Check R, also re-run for `Mode::Start` under the writer lock just
/// before publish. `Restart` → the live pid; `Start` → `None`.
pub(in crate::cmd::agent) fn check_running(
    home: &Path,
    name: &str,
    mode: Mode,
) -> Result<Option<u32>, Step> {
    let agent_dir = home.join("agents").join(name);
    let live = live_lock(&agent_dir);
    match mode {
        Mode::Restart => match live {
            Some(l) => Ok(Some(l.pid)),
            None => Err(refusal(format!(
                "agent '{name}' is not running; use `mur agent start {name}` with the same \
                 flags; {NOTHING_CHANGED}"
            ))),
        },
        Mode::Start => {
            let running = || {
                refusal(format!(
                    "agent '{name}' is already running; use `mur agent restart {name}` with the \
                     same flags; {NOTHING_CHANGED}"
                ))
            };
            if live.is_some() {
                return Err(running());
            }
            // A booting runtime holds the sentinel before it writes the lock:
            // no JSON is not "not running". The probe's flock is dropped
            // here, before launch, or the launched runtime exits as a dup.
            match probe_sentinel(&agent_dir) {
                SentinelProbe::Free(_) | SentinelProbe::NoAgentDir => Ok(None),
                SentinelProbe::Owned => Err(running()),
            }
        }
    }
}

fn live_lock(agent_dir: &Path) -> Option<LockFile> {
    mur_common::lock_file::read(&agent_dir.join("running.lock"))
        .ok()
        .flatten()
        .filter(|l| pid_alive(l.pid))
}

/// Check H: the service must read the same home this writer writes to.
///
/// The service's home is its *effective* environment (drop-ins, the
/// manager environment, a running process), not the unit file alone; when
/// that cannot be proven the override is refused rather than written to a
/// home the service may never read.
fn check_home(name: &str, home: &Path, unit: &Path, by: IssuedBy) -> Result<(), Step> {
    let this = match by {
        IssuedBy::Cli => "this CLI",
        IssuedBy::Hub => "this Hub",
    };
    let service_home = match descriptor_home(unit) {
        Ok(h) => h,
        Err(reason) => {
            return Err(refusal(format!(
                "'{name}' is installed as a service, but which MUR home it uses cannot be \
                 told ({reason}), so {this} cannot know an override written to {} would \
                 reach it; {NOTHING_CHANGED}",
                home.display()
            )));
        }
    };
    let same = match (
        std::fs::canonicalize(&service_home),
        std::fs::canonicalize(home),
    ) {
        (Ok(s), Ok(h)) => s == h,
        _ => false,
    };
    if same {
        return Ok(());
    }
    Err(refusal(format!(
        "'{name}' is installed as a service that uses {}, but {this} uses {}. \
         An override written here would never reach it; {NOTHING_CHANGED}",
        service_home.display(),
        home.display()
    )))
}

#[cfg(test)]
#[path = "prepare_tests.rs"]
mod tests;

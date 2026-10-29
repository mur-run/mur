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
fn check_home(name: &str, home: &Path, unit: &Path, by: IssuedBy) -> Result<(), Step> {
    let service_home = service_home(unit);
    let same = match (&service_home, std::fs::canonicalize(home)) {
        (Some(s), Ok(h)) => std::fs::canonicalize(s).is_ok_and(|s| s == h),
        _ => false,
    };
    if same {
        return Ok(());
    }
    let shown =
        service_home.map_or_else(|| "an unknown home".to_owned(), |p| p.display().to_string());
    let this = match by {
        IssuedBy::Cli => "this CLI",
        IssuedBy::Hub => "this Hub",
    };
    Err(refusal(format!(
        "'{name}' is installed as a service that uses {shown}, but {this} uses {}. \
         An override written here would never reach it; {NOTHING_CHANGED}",
        home.display()
    )))
}

/// `MUR_HOME` from the unit's environment if it sets one, else the default
/// home a service started by the OS resolves.
fn service_home(unit: &Path) -> Option<std::path::PathBuf> {
    let text = std::fs::read_to_string(unit).ok();
    let baked = text.as_deref().and_then(|t| {
        if unit.extension().is_some_and(|e| e == "plist") {
            crate::cmd::init_daemon::launchd_plist_mur_home(t)
        } else {
            crate::cmd::init_daemon::systemd_unit_mur_home(t)
        }
    });
    baked.or_else(|| mur_common::home::resolve_from(None, dirs::home_dir().as_deref()))
}

#[cfg(test)]
#[path = "prepare_tests.rs"]
mod tests;

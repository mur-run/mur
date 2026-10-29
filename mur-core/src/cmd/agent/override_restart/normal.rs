//! "Restart normally" from the Hub's override badge (Hub spec §Restart
//! normally): under the writer lock, sweep any orphan, then restart through
//! the same launcher with nothing pending. It never publishes, so a pending
//! orphan cannot ride the restart the user chose to drop the override.

use super::super::restart::{Launcher, RestartNote, restart_one_with};
use super::super::stale;
use super::Prepared;
use super::outcome::OverrideOutcome;
use super::run::Job;

/// The verdict of a normal restart: `ok` only when the fresh runtime was
/// confirmed. `detail` is the line the CLI would print.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct NormalRestart {
    pub ok: bool,
    pub detail: String,
}

/// `prepared` comes from `prepare` with `Mode::Restart` (checks H and R, the
/// writer lock); dropping it on return releases the lock.
pub fn restart_normally(
    prepared: Prepared,
    window: Option<String>,
    launcher: &mut dyn Launcher,
    note: &mut dyn FnMut(RestartNote),
) -> Result<NormalRestart, OverrideOutcome> {
    let job = Job::new(&prepared.home, &prepared.name, window);
    job.sweep(note)?;
    let name = prepared.name.as_str();
    let on_disk = stale::on_disk_sha_for(name);
    match restart_one_with(name, &prepared.home, &on_disk, launcher, note) {
        Ok(r) => Ok(NormalRestart {
            ok: r.ok,
            detail: r.detail,
        }),
        Err(e) => Ok(NormalRestart {
            ok: false,
            detail: format!("{e:#}"),
        }),
    }
}

#[cfg(test)]
#[path = "normal_tests.rs"]
mod tests;

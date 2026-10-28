//! The startup-override lines of `mur agent status` (spec §Visibility).
//!
//! Read from `running.lock`, gated on the lock's pid being alive: the lock is
//! removed on a clean exit, but a killed runtime leaves it behind, and a dead
//! process's override is not active.

use std::path::Path;
use std::time::{Duration, Instant};

use mur_common::LockFile;
use mur_common::hitl::HITL_PENDING_METHOD;

/// How long `mur agent status` waits for the waiting-call count. Short: the
/// count is a hint on a status line, and a busy runtime must not stall it.
const WAITING_DIAL_SECS: u64 = 2;

/// The override lines for the agent whose lock is at `lock_path`.
pub(super) fn lines_for(mur_home: &Path, name: &str, lock_path: &Path) -> Vec<String> {
    let Ok(Some(lock)) = mur_common::lock_file::read(lock_path) else {
        return Vec::new();
    };
    if lock.startup_override.is_none() && lock.override_rejected.is_none() {
        return Vec::new();
    }
    let alive = mur_common::lock_file::pid_alive(lock.pid);
    let waiting = (alive && lock.startup_override.is_some())
        .then(|| waiting_count(mur_home, name))
        .flatten();
    lines(&lock, alive, waiting)
}

/// Calls the runtime is holding for a human, or `None` if it did not answer.
fn waiting_count(mur_home: &Path, name: &str) -> Option<usize> {
    let deadline = Instant::now() + Duration::from_secs(WAITING_DIAL_SECS);
    let v = crate::a2a_dial::dial_method_until(
        mur_home,
        name,
        HITL_PENDING_METHOD,
        serde_json::json!({}),
        deadline,
    )
    .ok()?;
    v.get("pending")?.as_array().map(Vec::len)
}

/// The lines for `lock`, or none when no override is recorded or the process
/// that recorded it is gone. `waiting` is how many calls the runtime holds for
/// a human, when it could be asked — rule 4 still parks under `--no-hitl`, so a
/// wedged override boot shows here rather than looking idle.
pub(super) fn lines(lock: &LockFile, pid_alive: bool, waiting: Option<usize>) -> Vec<String> {
    if !pid_alive {
        return Vec::new();
    }
    let mut out = Vec::new();
    if let Some(r) = &lock.startup_override {
        let flags = r.flags.cli_names();
        let flags = if flags.is_empty() {
            "no flags".to_string()
        } else {
            flags.join(", ")
        };
        out.push(format!("     ⚠ override active ({}): {flags}", r.nonce));
        match waiting {
            Some(0) => {}
            Some(n) => out.push(format!(
                "       {n} call(s) waiting for a human — `mur agent hitl {} pending`",
                lock.name
            )),
            None => out.push(
                "       waiting calls: unknown (the agent did not answer `hitl/pending`)".into(),
            ),
        }
    }
    if let Some(r) = &lock.override_rejected {
        let nonce = r.nonce.as_deref().unwrap_or("no nonce");
        out.push(format!(
            "     override not applied ({nonce}): {} — this boot runs with the profile as written",
            r.reason
        ));
    }
    out
}

#[cfg(test)]
#[path = "status_override_tests.rs"]
mod tests;

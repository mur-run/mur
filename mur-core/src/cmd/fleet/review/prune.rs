//! §7.1 / AC15b / AC15d: `mur fleet prune-reviews` — the manual, never-timed
//! erase of retained review-session channels.
//!
//! Same posture as `mur monitor prune`: `--older-than` is required and there
//! is no automatic GC behind it. Candidates come from the §7.0 derived state
//! (`state.rs`), all aged by last activity (the later of the last readable
//! event and a readable marker's `detected_at`):
//!
//! | State | Default | `--include-paused` |
//! |---|---|---|
//! | running (lock held) | never | never |
//! | stopped, corrupted (readable marker), orphaned | candidate | candidate |
//! | corrupted, unreadable marker | listed by path, skipped | same |
//! | paused, crashed | kept | candidate |
//!
//! A paused or crashed candidate is ended crash-safely under its run lock:
//! `session_stopped` (reason `pruned`), then the definition, then the
//! channel. Dying midway leaves a `stopped` session the next prune takes.

use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use mur_channel::ChannelService;

use super::constants::{FLEET_CHANNEL_PREFIX, REVIEW_FLEET_PREFIX, REVIEW_STOP_REASON_PRUNED};
use super::state::{SessionState, observe, stop_from_outside};

/// The review fleet name a review channel belongs to, if it is one.
fn review_session_of(channel_id: &str) -> Option<&str> {
    channel_id
        .strip_prefix(FLEET_CHANNEL_PREFIX)
        .filter(|name| name.starts_with(REVIEW_FLEET_PREFIX))
}

/// The label prune prints for a candidate state, or `None` when the state
/// is never a candidate under these flags.
fn candidate_label(state: &SessionState, include_paused: bool) -> Option<&'static str> {
    match state {
        SessionState::Stopped => Some("stopped"),
        SessionState::Corrupted => Some("corrupted"),
        SessionState::Orphaned => Some("orphaned"),
        SessionState::Paused if include_paused => Some("paused"),
        SessionState::Crashed if include_paused => Some("crashed"),
        _ => None,
    }
}

/// Erase review channels idle longer than `older_than`. `include_paused`
/// also takes paused and crashed sessions. With `dry_run`, only lists them.
pub fn prune_reviews(
    mur_home: &Path,
    older_than: &str,
    include_paused: bool,
    dry_run: bool,
    out: &mut dyn Write,
    now: DateTime<Utc>,
) -> Result<()> {
    let age = mur_common::limits::parse_duration(older_than).ok_or_else(|| {
        anyhow::anyhow!(
            "unrecognised duration `{older_than}` — use a number of seconds or a \
             unit suffix, e.g. `30d`, `36h`, `90m`"
        )
    })?;
    let age = chrono::Duration::from_std(age)
        .with_context(|| format!("duration `{older_than}` is too large"))?;
    let cutoff = now - age;

    let svc = ChannelService::open(mur_home)?;
    let mut ids = svc.store().list_ids()?;
    ids.sort();
    let mut pruned = 0usize;
    for id in &ids {
        let Some(session) = review_session_of(id) else {
            continue;
        };
        let observed = observe(&svc, mur_home, id, session)?;
        if let SessionState::CorruptedUnreadable(path) = &observed.state {
            writeln!(
                out,
                "skipped {session}: corrupted marker cannot be read ({})",
                path.display()
            )?;
            continue;
        }
        let Some(why) = candidate_label(&observed.state, include_paused) else {
            continue;
        };
        let Some(last) = observed.last.filter(|l| *l <= cutoff) else {
            continue;
        };
        if dry_run {
            writeln!(out, "would prune {session} ({why}, last activity {last})")?;
        } else {
            let lock = observed
                .lock
                .as_ref()
                .expect("observe holds the lock for every non-running state");
            stop_from_outside(
                &svc,
                mur_home,
                session,
                id,
                &observed.state,
                REVIEW_STOP_REASON_PRUNED,
                lock,
            )?;
            drop(observed.lock);
            svc.delete_channel(id)?;
            writeln!(out, "pruned {session} ({why}, last activity {last})")?;
        }
        pruned += 1;
    }
    let scope = if include_paused {
        "review session (stopped, corrupted, orphaned, paused or crashed)"
    } else {
        "stopped, corrupted or orphaned review session"
    };
    match (pruned, dry_run) {
        (0, _) => writeln!(
            out,
            "nothing to prune — no {scope} has been idle longer than {older_than}"
        )?,
        (n, true) => writeln!(
            out,
            "{n} review session(s) would be pruned (dry run, nothing erased)"
        )?,
        (n, false) => writeln!(out, "{n} review session(s) pruned")?,
    }
    Ok(())
}

#[cfg(test)]
#[path = "prune_tests.rs"]
mod prune_tests;

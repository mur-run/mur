//! `mur monitor prune` — bulk erase of finished monitors past an age cutoff.
//! Split out of `monitor.rs` to keep that file under the 800-line limit; the
//! verb's rationale lives on `prune` itself.

use std::io::Write;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use mur_monitor::state::MonitorState;
use mur_monitor::store::{ListFilter, MonitorRow, MonitorStore};

use super::{ID_SHORT, ago, truncate};
/// The last moment this monitor is known to have done anything. Deliberately
/// not `created_at` alone: a monitor registered long ago but checked five
/// minutes ago is *young*, and pruning it would erase evidence a user is
/// still reading. `last_checked_at` is `None` until the first cycle lands,
/// so fall back through progress to creation.
pub(super) fn last_activity(r: &MonitorRow) -> DateTime<Utc> {
    r.last_checked_at
        .unwrap_or(r.last_progress_at)
        .max(r.created_at)
}

/// `prune` is `delete` in bulk with an age gate, and it is deliberately the
/// timid one: it only ever touches monitors that have STOPPED — `completed`
/// (finished or cancelled) and, opt-in, `exhausted`. Anything still being
/// watched is never eligible at any age, because "old" is not "over"; a
/// monitor past its deadline is exactly the one a user still wants to see.
/// There is no automatic GC behind this on purpose: erasing evidence is a
/// user's call, and a background reaper would make old evidence vanish
/// between one `list` and the next with nothing to point at.
///
/// A leased row is skipped rather than fatal — one worker mid-cycle must not
/// abort a sweep over fifty finished monitors — and the count reported is of
/// rows actually erased, never of rows merely selected.
pub(super) fn prune(
    store: &MonitorStore,
    older_than: &str,
    include_exhausted: bool,
    dry_run: bool,
    out: &mut dyn Write,
    now: DateTime<Utc>,
) -> Result<()> {
    let age = mur_common::limits::parse_duration(older_than).ok_or_else(|| {
        anyhow::anyhow!(
            "unrecognised duration `{older_than}` — use a number of seconds or a \
             unit suffix, e.g. `7d`, `36h`, `90m`"
        )
    })?;
    let age = chrono::Duration::from_std(age)
        .with_context(|| format!("duration `{older_than}` is too large"))?;
    let cutoff = now - age;

    let rows = store.list(&ListFilter {
        state: None,
        include_completed: true,
    })?;
    let eligible: Vec<&MonitorRow> = rows
        .iter()
        .filter(|r| {
            (r.state == MonitorState::Completed
                || (include_exhausted && r.state == MonitorState::Exhausted))
                && last_activity(r) <= cutoff
        })
        .collect();

    if eligible.is_empty() {
        let extra = if include_exhausted {
            "completed or exhausted"
        } else {
            "completed"
        };
        writeln!(
            out,
            "nothing to prune — no {extra} monitor has been idle longer than {older_than}"
        )?;
        return Ok(());
    }

    for r in &eligible {
        writeln!(
            out,
            "{}  {}  {}  last activity {}",
            &r.id[..ID_SHORT.min(r.id.len())],
            r.state.as_str(),
            truncate(&r.name, 28),
            ago(now, last_activity(r))
        )?;
    }

    if dry_run {
        writeln!(
            out,
            "dry run — {} monitor(s) match; re-run without --dry-run to erase them",
            eligible.len()
        )?;
        return Ok(());
    }

    let mut deleted = 0usize;
    let mut skipped: Vec<String> = Vec::new();
    for r in &eligible {
        match store.delete(&r.id, false) {
            Ok(true) => deleted += 1,
            // Vanished under us (another prune, another shell): not this
            // call's deletion, so it is not counted as one.
            Ok(false) => {}
            Err(e) => skipped.push(format!("{}: {e}", &r.id[..ID_SHORT.min(r.id.len())])),
        }
    }
    for s in &skipped {
        writeln!(out, "skipped {s}")?;
    }
    writeln!(
        out,
        "pruned {deleted} monitor(s) — evidence erased; the monitored work itself was NOT cancelled"
    )?;
    Ok(())
}

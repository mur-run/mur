//! Drains `monitor_registration_outbox` (spec §自動註冊邊界 clause 2): a
//! `MonitorSpec` whose work is already running but whose immediate
//! `MonitorStore::create` failed lands there via `register_or_outbox`
//! (`mur_monitor::register`) instead of disappearing. This module is what
//! turns a parked spec into a real, pollable monitor on a later tick — or,
//! if it never can be, says so instead of quietly dropping it.
//!
//! Split out of `service.rs` as its own module rather than a function added
//! there, following `drain_actions`'s precedent for the identical two
//! reasons recorded in `service.rs`'s own module doc: staying under
//! CLAUDE.md's 800-line-per-file rule, and avoiding a `pub use` re-export
//! that `mur-core`'s bin target would flag as an unused import under
//! `-D warnings` (the daemon reaches this module directly, the same way it
//! reaches `drain_actions`).

use std::path::Path;

use anyhow::Result;
use chrono::{DateTime, Utc};
use mur_monitor::store::{MonitorStore, OutboxRow};

/// Bounds one `drain_outbox` call, mirroring `DRAIN_MAX_PER_TICK`
/// (notifications) and `DRAIN_MAX_ACTIONS_PER_TICK` (actions): a backlog
/// after downtime spreads across ticks instead of registering an unbounded
/// number of specs in one pass.
pub const OUTBOX_MAX_PER_TICK: usize = 10;

/// Consecutive failed registration attempts before a row is given up on.
/// Spec §自動註冊邊界 clause 2 exists to keep already-running work
/// trackable; giving up is the one path that breaks that promise, so it
/// must be rare (finite, not small) and — per rule 3 below — never silent.
pub const OUTBOX_MAX_ATTEMPTS: u32 = 8;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct OutboxReport {
    pub registered: usize,
    pub retried: usize,
    pub gave_up: usize,
}

/// Turn outboxed specs into real monitors. Never creates a store (a user
/// who has never run `mur monitor` acquires nothing just because the daemon
/// ticked) and never fails the caller for an individual row's registration
/// failure — spec §錯誤處理's discipline throughout this subsystem: a
/// delivery/registration problem is recorded and retried, not propagated as
/// a tick failure. An `Err` here is reserved for the store itself
/// misbehaving (e.g. `outbox_drop` failing), which the caller logs and
/// carries on past, same as every other drain in this package.
pub fn drain_outbox(mur_home: &Path, now: DateTime<Utc>) -> Result<OutboxReport> {
    let Some(store) = MonitorStore::open_existing(mur_home)? else {
        return Ok(OutboxReport::default());
    };
    let mut rep = OutboxReport::default();
    for row in store.outbox_due(now, OUTBOX_MAX_PER_TICK)? {
        if !is_due(&row, now) {
            continue;
        }
        attempt(&store, &row, now, &mut rep)?;
    }
    Ok(rep)
}

/// One row's attempt: re-validate (defense in depth — see the module test
/// `home_with_permanently_unregisterable_spec` for why this matters even
/// though `register_or_outbox` already validates before a spec ever reaches
/// the outbox) then try `create`. Success drops the row and counts as
/// `registered` whether or not `Created::existing` is true — either way a
/// real, pollable monitor now exists at this idempotency key, which is all
/// this function promises.
fn attempt(
    store: &MonitorStore,
    row: &OutboxRow,
    now: DateTime<Utc>,
    rep: &mut OutboxReport,
) -> Result<()> {
    let outcome = row
        .spec
        .validate()
        .map_err(anyhow::Error::from)
        .and_then(|()| store.create(&row.spec, now, None));
    match outcome {
        Ok(_created) => {
            store.outbox_drop(&row.id)?;
            rep.registered += 1;
        }
        Err(e) => {
            let attempts_after = row.attempts + 1;
            if attempts_after >= OUTBOX_MAX_ATTEMPTS {
                give_up(store, row, &e, rep)?;
            } else {
                store.outbox_record_failure(&row.id, &e.to_string())?;
                rep.retried += 1;
            }
        }
    }
    Ok(())
}

/// Rule 3: giving up must be visible, never silent — the same discipline as
/// a parked notification (`enough_consecutive_failures_park_the_notification`
/// in `service.rs`). There is no monitor row to hang a `monitor_events` entry
/// off here — the whole point of this path is that one was never created —
/// so fabricating one purely to log an event would itself violate spec
/// §自動註冊邊界 clause 3 ("不得聲稱已監看"): this work must never be
/// reported as monitored. A structured `tracing::error!` is therefore the
/// visibility mechanism, landing in the same daemon log CLAUDE.md already
/// documents for other notable monitor events — a human grepping it finds
/// exactly which spec's work is now unmonitored, by name and idempotency
/// key. The error is redacted first, same rule as `outbox_record_failure`'s
/// stored errors: a secret must never reach a log.
fn give_up(
    store: &MonitorStore,
    row: &OutboxRow,
    error: &anyhow::Error,
    rep: &mut OutboxReport,
) -> Result<()> {
    tracing::error!(
        outbox_id = %row.id,
        spec_name = %row.spec.name,
        idempotency_key = %row.spec.idempotency_key,
        attempts = row.attempts + 1,
        error = %mur_common::redact::redact_secrets(&error.to_string()),
        "monitor registration outbox: giving up — this work is running and will NOT be monitored"
    );
    store.outbox_drop(&row.id)?;
    rep.gave_up += 1;
    Ok(())
}

/// Whether `row` has waited long enough to be worth retrying.
/// `monitor_registration_outbox` has no `next_attempt_at` column (task 7's
/// documented gap — a schema change was explicitly out of scope for that
/// task, and still is for this one), so this is derived on every call from
/// the two columns the table does have, `created_at` and `attempts`,
/// instead of being read back from storage.
///
/// The schedule is `crate::backoff::unknown_delay` — the "our problem to
/// notice quickly, not the work's" table, which is the right one here: a
/// registration retry is retrying our own store write, not asking the
/// monitored work anything — summed across every attempt so far and
/// anchored at `created_at`, because no timestamp of the *last* attempt is
/// persisted either. This is restart-safe: both inputs live in the row, so
/// a daemon restart costs the wait nothing. What it does cost is precision
/// on the fast path — the schedule is measured from creation, not from the
/// most recent failure, so a slow tick (or a daemon that was down for a
/// while) can make the next retry land sooner than a last-attempt-anchored
/// schedule would have allowed. That direction is harmless: it only ever
/// makes a row eligible to retry *earlier*, never delays giving up on it.
fn is_due(row: &OutboxRow, now: DateTime<Utc>) -> bool {
    let elapsed = now.signed_duration_since(row.created_at);
    let required: i64 = (0..row.attempts)
        .map(|attempt| mur_monitor::backoff::unknown_delay(attempt).as_secs() as i64)
        .sum();
    elapsed >= chrono::Duration::seconds(required)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use mur_monitor::spec::MonitorSpec;
    use mur_monitor::store::ListFilter;

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 16, 12, 0, 0).unwrap()
    }

    fn spec_with_reference(reference: &str) -> MonitorSpec {
        MonitorSpec::from_yaml(&format!(
            "schema_version: 1\nname: t\nsource: {{ type: mur_run, reference: \"{reference}\" }}\nidempotency_key: \"mur_run:{reference}\"\ncreated_by: {{ actor: user:test }}\n"
        ))
        .unwrap()
    }

    /// `create()` performs no validation itself (it just inserts), so the
    /// only portable, public-API way to make `store.create` permanently
    /// fail from `mur-core` — a different crate, which cannot reach
    /// `MonitorStore::conn()` (`pub(crate)` to `mur-monitor`, the trick
    /// task 7's own tests use for this exact purpose) — is a spec that
    /// fails `MonitorSpec::validate()` itself. `outbox_enqueue` performs no
    /// validation gate, so this can be parked directly, simulating an
    /// outbox row `register_or_outbox` would never itself create (it
    /// validates first) but that `drain_outbox`'s own re-validation (see
    /// `attempt`'s doc) must still handle defensively — e.g. a future
    /// schema bump orphaning an old row.
    fn permanently_invalid_spec() -> MonitorSpec {
        MonitorSpec::from_yaml(
            "schema_version: 999\nname: bad\nsource: { type: mur_run, reference: run-x }\nidempotency_key: bad-key\ncreated_by: { actor: user:test }\n",
        )
        .unwrap()
    }

    fn home_with_outboxed_spec(reference: &str) -> (tempfile::TempDir, std::path::PathBuf, String) {
        let d = tempfile::tempdir().unwrap();
        let home = d.path().to_path_buf();
        let s = MonitorStore::open(&home).unwrap();
        let id = s
            .outbox_enqueue(&spec_with_reference(reference), t0())
            .unwrap();
        (d, home, id)
    }

    fn home_with_permanently_unregisterable_spec() -> (tempfile::TempDir, std::path::PathBuf, String)
    {
        let d = tempfile::tempdir().unwrap();
        let home = d.path().to_path_buf();
        let s = MonitorStore::open(&home).unwrap();
        let id = s.outbox_enqueue(&permanently_invalid_spec(), t0()).unwrap();
        (d, home, id)
    }

    /// Same rule as every other drain in this package
    /// (`tick_once_against_a_home_with_no_store_creates_nothing`,
    /// `a_home_with_no_store_drains_nothing_and_creates_nothing`): a user
    /// who has never run `mur monitor` acquires no database, even by
    /// draining an outbox that (by definition) cannot exist yet either.
    /// Asserted on the filesystem, not only the returned report — an empty
    /// `OutboxReport` comes back either way (nothing to drain either way),
    /// so a return-value-only assertion would pass against a `drain_outbox`
    /// that used `MonitorStore::open` instead of `open_existing`.
    #[test]
    fn a_home_with_no_store_drains_no_outbox_and_creates_nothing() {
        let d = tempfile::tempdir().unwrap();
        let dir = mur_monitor::store::db_dir(d.path());
        assert!(!dir.exists());
        assert_eq!(
            drain_outbox(d.path(), t0()).unwrap(),
            OutboxReport::default()
        );
        assert!(!dir.exists(), "draining must not create the store");
    }

    #[test]
    fn an_outboxed_spec_becomes_a_real_monitor_on_the_next_tick() {
        let (_d, home, _) = home_with_outboxed_spec("run-1");
        let r = drain_outbox(&home, t0()).unwrap();
        assert_eq!(r.registered, 1);
        let s = MonitorStore::open_existing(&home).unwrap().unwrap();
        assert_eq!(s.list(&ListFilter::default()).unwrap().len(), 1);
        assert!(
            s.outbox_due(t0(), 10).unwrap().is_empty(),
            "a registered row must leave the outbox"
        );
    }

    /// The brief's given body for this test only asserts `gave_up == 1`
    /// summed over the whole loop and that the outbox ends up empty — both
    /// of which a drain that gives up on the very first failure (or one
    /// that never enqueued the row at all) would also satisfy, since the
    /// row is gone after iteration 0 and every later `drain_outbox` call
    /// against an empty outbox reports all-zero. That is exactly the
    /// "absence is not evidence" trap this task's self-review names.
    /// Pinning the exact iteration `gave_up` first becomes nonzero, and the
    /// number of `retried` results accumulated before it, proves
    /// `OUTBOX_MAX_ATTEMPTS - 1` real retries happened first — a drain that
    /// gives up early fails `gave_up_at`; a drain that never enqueues fails
    /// the very first `assert_eq!(gave_up, 1)`.
    #[test]
    fn a_spec_that_keeps_failing_is_given_up_on_visibly_not_silently() {
        let (_d, home, _) = home_with_permanently_unregisterable_spec();
        let mut gave_up = 0;
        let mut retried_total = 0;
        let mut gave_up_at = None;
        for i in 0..(OUTBOX_MAX_ATTEMPTS + 2) {
            let r = drain_outbox(&home, t0() + chrono::Duration::hours(i64::from(i))).unwrap();
            gave_up += r.gave_up;
            retried_total += r.retried;
            if r.gave_up > 0 && gave_up_at.is_none() {
                gave_up_at = Some(i);
            }
        }
        assert_eq!(gave_up, 1);
        assert_eq!(
            gave_up_at,
            Some(OUTBOX_MAX_ATTEMPTS - 1),
            "must retry OUTBOX_MAX_ATTEMPTS - 1 times before giving up, not on the first failure"
        );
        assert_eq!(
            retried_total,
            (OUTBOX_MAX_ATTEMPTS - 1) as usize,
            "every attempt before the give-up must be counted as a retry"
        );
        let s = MonitorStore::open_existing(&home).unwrap().unwrap();
        assert!(
            s.outbox_due(t0() + chrono::Duration::days(9), 10)
                .unwrap()
                .is_empty()
        );
    }

    /// Proves the backoff schedule in `is_due` is actually load-bearing —
    /// without it, `drain_outbox` would hammer a failing row on every tick
    /// regardless of `crate::backoff::unknown_delay`'s schedule.
    #[test]
    fn a_failed_row_is_not_retried_before_its_backoff_window_elapses() {
        let (_d, home, _) = home_with_permanently_unregisterable_spec();
        let r1 = drain_outbox(&home, t0()).unwrap();
        assert_eq!(r1.retried, 1, "attempts == 0 must always be due");

        // unknown_delay(0) == 10s: a tick 1s later must not retry yet.
        let r2 = drain_outbox(&home, t0() + chrono::Duration::seconds(1)).unwrap();
        assert_eq!(
            r2,
            OutboxReport::default(),
            "must wait out the backoff window before retrying"
        );

        // Past the window: retried again.
        let r3 = drain_outbox(&home, t0() + chrono::Duration::seconds(11)).unwrap();
        assert_eq!(r3.retried, 1, "past the backoff window, must retry");
    }
}

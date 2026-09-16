//! Registering a monitor for asynchronous work an agent just started, or
//! putting the spec somewhere recoverable when registration itself fails
//! (spec §自動註冊邊界).
//!
//! `Registered` is deliberately three-valued and the values are NOT
//! interchangeable — collapsing this into a bool loses the reason this type
//! exists. See the doc on each variant.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};

use crate::spec::MonitorSpec;
use crate::state::MonitorState;
use crate::store::{MonitorStore, ts};

/// The three outcomes of trying to make a monitor exist for work an agent
/// just started (spec §自動註冊邊界 clauses 1-3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Registered {
    /// A real monitor exists at this id and the daemon will poll it.
    Monitored(String),
    /// The work is running and is NOT monitored yet. The caller MUST tell
    /// the user so in the same turn — this is exactly the failure clause 2
    /// exists to prevent. The id names an outbox row, not a monitor: do not
    /// pass it to `MonitorStore::get`.
    Outboxed(String),
    /// No queryable reference (or another spec defect), so nothing was
    /// created and nothing is pending. The caller must not claim
    /// monitoring (clause 3).
    Refused(String),
}

/// Validate `spec`, then either register it immediately or, if the store
/// write itself fails, park it in the outbox so the work stays trackable
/// instead of disappearing. Never silently drops a spec whose work is
/// already running.
pub fn register_or_outbox(
    store: &MonitorStore,
    spec: &MonitorSpec,
    now: DateTime<Utc>,
) -> Registered {
    if let Err(e) = spec.validate() {
        return Registered::Refused(e.to_string());
    }
    match store.create(spec, now, None) {
        Ok(created) => Registered::Monitored(created.id),
        Err(create_err) => match store.outbox_enqueue(spec, now) {
            Ok(outbox_id) => Registered::Outboxed(outbox_id),
            Err(outbox_err) => Registered::Refused(format!(
                "registration failed ({create_err}) and could not be queued ({outbox_err})"
            )),
        },
    }
}

/// Clause 1: where the source lets a caller record before starting, persist
/// the monitor in `Registering` — not claimable
/// (`MonitorState::is_claimable`), so the daemon never polls a monitor with
/// no reference yet — then the caller starts the work, then calls
/// `attach_reference`.
///
/// Reuses `create`'s idempotency-key dedup rather than inserting directly:
/// if a non-completed monitor already exists under this key, that existing
/// monitor's id comes back untouched (it may already be well past
/// `Registering`) instead of a second row being made, or the existing one
/// being demoted back to `Registering`.
pub fn begin_registering(
    store: &MonitorStore,
    spec: &MonitorSpec,
    now: DateTime<Utc>,
) -> Result<String> {
    // One INSERT, already `Registering`. `create` + `set_state` would leave a
    // window in which the row is `Active` with no reference — and
    // `is_claimable` includes `Active`, so a tick landing there would claim
    // and poll a monitor that cannot be queried. Clause 1 asks for atomic
    // semantics 「盡可能」; this is the version that has them.
    let created = store.create_in_state(spec, now, None, MonitorState::Registering)?;
    Ok(created.id)
}

/// Fill in the reference once the work is actually running, and move the
/// monitor from `Registering` to `Active` so the scheduler can claim it.
///
/// `MonitorRow` carries the reference twice: inside `spec.source.reference`
/// (the JSON blob) and as the denormalized top-level `reference` column
/// `create` also populates. Both are written here, in one transaction, or
/// `list`/`show` (which read the column) would disagree with the adapter
/// (which reads `spec.source.reference`) about what is being watched.
///
/// Read-then-write (the current spec_json must be read to patch its
/// `reference` field), so this runs under an explicit `BEGIN IMMEDIATE` /
/// `COMMIT` with `ROLLBACK` on every error path — same shape as
/// `MonitorStore::create` and `store/lease.rs`'s claim/heartbeat/expiry, for
/// the identical reason: a deferred transaction's read snapshot could go
/// stale under WAL if another writer touched this row in between.
pub fn attach_reference(
    store: &MonitorStore,
    id: &str,
    reference: &str,
    now: DateTime<Utc>,
) -> Result<()> {
    store
        .conn()
        .execute_batch("BEGIN IMMEDIATE")
        .context("begin attach_reference transaction")?;

    let result = (|| -> Result<()> {
        let spec_json: String = store
            .conn()
            .query_row("SELECT spec_json FROM monitors WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .with_context(|| format!("monitor {id} not found"))?;
        let mut spec: MonitorSpec =
            serde_json::from_str(&spec_json).context("parse stored spec_json")?;
        spec.source.reference = reference.to_string();
        // `AND state = ?6`. Without it this is a way to force ANY monitor
        // back to `Active` and overwrite its spec: `begin_registering`
        // returns an existing row when the idempotency key matches, and
        // that row may be live — `AwaitingApproval` with a parked action,
        // say. Resurrecting it would drop the reference it is actually
        // watching and reset its version. Only a monitor this function
        // itself parked in `Registering` is a valid target.
        let n = store.conn().execute(
            "UPDATE monitors SET spec_json = ?1, reference = ?2, state = ?3, \
             next_check_at = ?4, version = version + 1 WHERE id = ?5 AND state = ?6",
            rusqlite::params![
                serde_json::to_string(&spec)?,
                reference,
                MonitorState::Active.as_str(),
                ts(now),
                id,
                MonitorState::Registering.as_str(),
            ],
        )?;
        if n != 1 {
            anyhow::bail!("monitor {id} is not registering; refusing to overwrite it");
        }
        Ok(())
    })();

    // The shared tail, not a fourth hand-rolled copy: this one omitted the
    // rollback-on-COMMIT-failure arm, which is the case the helper exists
    // for — a COMMIT that fails leaves the transaction open, and every later
    // statement on that connection silently joins it instead of
    // autocommitting.
    crate::store::commit_or_rollback(store.conn(), result, "attach_reference")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::ListFilter;
    use chrono::TimeZone;

    fn store() -> (tempfile::TempDir, MonitorStore) {
        let d = tempfile::tempdir().unwrap();
        let s = MonitorStore::open(d.path()).unwrap();
        (d, s)
    }

    /// Forces `MonitorStore::create` to fail deterministically: `create`'s
    /// own existence `SELECT` and `INSERT` both target the `monitors`
    /// table, so dropping it (while leaving `monitor_registration_outbox`
    /// intact) makes every `create` call error without touching anything
    /// this test needs to still work — `outbox_enqueue` targets a different
    /// table entirely.
    fn store_that_fails_create() -> (tempfile::TempDir, MonitorStore) {
        let (d, s) = store();
        s.conn().execute_batch("DROP TABLE monitors").unwrap();
        (d, s)
    }

    fn spec_with_reference(reference: &str) -> MonitorSpec {
        let idem = if reference.is_empty() {
            "auto:no-ref".to_string()
        } else {
            format!("mur_run:{reference}")
        };
        MonitorSpec::from_yaml(&format!(
            r#"
schema_version: 1
name: t
source: {{ type: mur_run, reference: "{reference}" }}
idempotency_key: "{idem}"
created_by: {{ actor: user:test }}
"#
        ))
        .unwrap()
    }

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 16, 12, 0, 0).unwrap()
    }

    #[test]
    fn a_spec_with_a_queryable_reference_is_monitored_immediately() {
        let (_d, s) = store();
        match register_or_outbox(&s, &spec_with_reference("run-1"), t0()) {
            Registered::Monitored(id) => assert!(s.get(&id).unwrap().is_some()),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_spec_with_no_reference_is_refused_and_creates_nothing() {
        // spec §自動註冊邊界 clause 3: 「不得聲稱已監看」.
        let (_d, s) = store();
        let before = s.list(&ListFilter::default()).unwrap().len();
        match register_or_outbox(&s, &spec_with_reference(""), t0()) {
            Registered::Refused(r) => assert!(!r.is_empty()),
            other => panic!("must refuse, got {other:?}"),
        }
        assert_eq!(
            s.list(&ListFilter::default()).unwrap().len(),
            before,
            "nothing may be created"
        );
        assert!(
            s.outbox_due(t0(), 10).unwrap().is_empty(),
            "and nothing may be queued"
        );
    }

    #[test]
    fn a_registration_that_fails_lands_in_the_outbox_not_on_the_floor() {
        // spec clause 2. The store is made to fail the insert; the spec must
        // survive somewhere recoverable.
        let (_d, s) = store_that_fails_create();
        let spec = spec_with_reference("run-1");
        match register_or_outbox(&s, &spec, t0()) {
            Registered::Outboxed(_) => {}
            other => panic!("{other:?}"),
        }
        let due = s.outbox_due(t0(), 10).unwrap();
        assert_eq!(due.len(), 1);
        // Not just "a row landed": the RIGHT spec landed. A version of
        // register_or_outbox that enqueued an empty/default spec, or a spec
        // built from the wrong input, would still pass a bare length check.
        assert_eq!(due[0].spec.idempotency_key, spec.idempotency_key);
        assert_eq!(due[0].spec.source.reference, spec.source.reference);
    }

    #[test]
    fn attach_reference_refuses_a_monitor_it_did_not_park() {
        // `begin_registering` returns an EXISTING row when the idempotency
        // key matches, and that row may be live. Without the state guard,
        // attaching would force it back to `Active`, overwrite the spec and
        // reference it is actually watching, and bump its version — a live
        // monitor silently repointed at different work.
        let (_d, s) = store();
        let id = s
            .create(&spec_with_reference("run-1"), t0(), None)
            .unwrap()
            .id;
        s.set_state(&id, MonitorState::AwaitingApproval, t0())
            .unwrap();
        let before = s.get(&id).unwrap().unwrap();

        let err = attach_reference(&s, &id, "run-hijacked", t0()).unwrap_err();
        assert!(
            err.to_string().contains("not registering"),
            "must say why it refused: {err}"
        );

        let after = s.get(&id).unwrap().unwrap();
        assert_eq!(
            after.state,
            MonitorState::AwaitingApproval,
            "state must not move"
        );
        assert_eq!(after.reference, before.reference, "reference must not move");
        assert_eq!(
            after.spec.source.reference, before.spec.source.reference,
            "the spec's copy must not move either"
        );
        assert_eq!(
            after.version, before.version,
            "and no write may have happened"
        );
    }

    #[test]
    fn begin_registering_then_attach_makes_a_pollable_monitor() {
        // clause 1: record first, start, then fill in the reference. Until
        // the reference lands the monitor is `Registering`, which
        // `is_claimable` excludes — the daemon must not poll a monitor with
        // no reference yet.
        let (_d, s) = store();
        let id = begin_registering(&s, &spec_with_reference(""), t0()).unwrap();
        let m = s.get(&id).unwrap().unwrap();
        assert_eq!(m.state, MonitorState::Registering);
        assert!(
            !m.state.is_claimable(),
            "a registering monitor must not be polled"
        );
        attach_reference(&s, &id, "run-7", t0()).unwrap();
        let m = s.get(&id).unwrap().unwrap();
        assert_eq!(m.spec.source.reference, "run-7");
        assert_eq!(
            m.reference, "run-7",
            "the denormalized column must agree with spec.source.reference"
        );
        assert!(
            m.state.is_claimable(),
            "attaching the reference must make it pollable"
        );
    }

    #[test]
    fn the_idempotency_key_returns_the_existing_monitor_rather_than_a_second_one() {
        // spec §建立時驗證 clause 6, already enforced by `create`; asserted
        // here because auto-registration is the path that will actually
        // retry.
        let (_d, s) = store();
        let a = register_or_outbox(&s, &spec_with_reference("run-1"), t0());
        let b = register_or_outbox(&s, &spec_with_reference("run-1"), t0());
        assert_eq!(format!("{a:?}"), format!("{b:?}"));
        assert_eq!(s.list(&ListFilter::default()).unwrap().len(), 1);
    }
}

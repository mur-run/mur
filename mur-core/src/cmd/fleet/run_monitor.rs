//! Register a durable monitor for a `mur fleet run` invocation so the daemon
//! keeps tracking it after this process exits (spec §自動註冊邊界). This is
//! the first real caller of `mur_monitor::register::register_or_outbox` —
//! everything downstream of it only matters if the three-valued
//! [`Registered`] outcome is actually surfaced to the user, not collapsed.
//!
//! Registration is bookkeeping on top of work that is already running: it
//! must never fail the run itself — [`register_fleet_run_monitor`] returns
//! no `Result`, only an optional message to relay — and it must never claim
//! monitoring happened when it did not (clauses 2 and 3 of
//! spec §自動註冊邊界; see the doc on each [`Registered`] variant).

use std::path::Path;

use chrono::Utc;
use mur_monitor::register::{Registered, register_or_outbox};
use mur_monitor::spec::{
    Actions, CreatedBy, MonitorSpec, Notifications, Outcomes, Policy, SCHEMA_VERSION, Source,
    SourceType,
};
use mur_monitor::store::MonitorStore;

/// Required by `MonitorSpec::validate` whenever `created_by.actor` starts
/// with `agent:`.
const REASON: &str = "fleet run started and returned a trackable run id";

/// Build the `mur_run` spec for one fleet run and try to register it.
///
/// Returns `None` when a real monitor now exists (the common case — nothing
/// to tell the operator). Returns `Some(message)` on `Outboxed` or
/// `Refused`: the work is running regardless, so the caller's only job is to
/// relay the message and keep going. Every failure path (the store won't
/// open, the spec won't validate, the write itself errors) ends here, in a
/// message — never in a `Result` the caller could `?`-abort the run with.
pub fn register_fleet_run_monitor(
    mur_home: &Path,
    fleet_name: &str,
    run_id: &str,
) -> Option<String> {
    let spec = MonitorSpec {
        schema_version: SCHEMA_VERSION,
        name: format!("fleet run: {fleet_name}"),
        source: Source {
            r#type: SourceType::MurRun,
            reference: run_id.to_string(),
            credential_ref: None,
        },
        outcomes: Outcomes::default(),
        actions: Actions::default(),
        policy: Policy::default(),
        notifications: Notifications::default(),
        idempotency_key: format!("fleet:{fleet_name}:{run_id}"),
        created_by: CreatedBy {
            actor: "agent:fleet".to_string(),
            reason: REASON.to_string(),
            originating_run_id: Some(run_id.to_string()),
        },
    };

    let store = match MonitorStore::open(mur_home) {
        Ok(s) => s,
        Err(e) => {
            return Some(format!(
                "fleet run {run_id} is running but not monitored: could not open the monitor store ({e:#})"
            ));
        }
    };

    match register_or_outbox(&store, &spec, Utc::now()) {
        // A real monitor exists; the daemon will poll it. Nothing more to
        // tell the operator than `mur monitor list` already shows.
        Registered::Monitored(_id) => None,
        // The work is running but is NOT monitored yet — spec clause 2
        // requires saying so, by name, in this same turn. A later daemon
        // tick retries this from the outbox.
        Registered::Outboxed(outbox_id) => Some(format!(
            "fleet run {run_id} is running but not monitored yet (queued as {outbox_id}; it will retry automatically)"
        )),
        // Nothing was created and nothing is pending — clause 3: never claim
        // monitoring happened.
        Registered::Refused(reason) => Some(format!(
            "fleet run {run_id} is running but not monitored: {reason}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_monitor::store::ListFilter;

    fn home() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn a_fleet_run_registers_a_monitor_for_its_run_id() {
        let d = home();
        let msg = register_fleet_run_monitor(d.path(), "demo", "run-1");
        // Closes the "prints on every run" loophole: a version that always
        // said "not monitored" regardless of outcome would fail this.
        assert!(msg.is_none(), "happy path must say nothing: {msg:?}");

        let store = MonitorStore::open_existing(d.path()).unwrap().unwrap();
        let rows = store.list(&ListFilter::default()).unwrap();
        assert_eq!(rows.len(), 1);
        let m = &rows[0];
        // Exact fields, not just "a monitor exists": a monitor with the
        // wrong idempotency_key would silently duplicate on the next run,
        // and a monitor of a source type nothing can query would never
        // resolve — both would still pass a bare `rows.len() == 1` check.
        assert_eq!(m.reference, "run-1");
        assert_eq!(m.spec.source.reference, "run-1");
        assert_eq!(
            m.source_type,
            SourceType::MurRun,
            "must be queryable by MurRunAdapter"
        );
        assert_eq!(m.idempotency_key, "fleet:demo:run-1");
    }

    #[test]
    fn a_failed_registration_says_so_and_does_not_fail_the_run() {
        // spec §自動註冊邊界 clause 2: the turn must report the work started
        // but is not monitored, with the trackable id.
        //
        // Forces `MonitorStore::open` to fail without reaching into
        // mur-monitor's private connection: `open` creates `monitor/` under
        // `mur_home` via `create_dir_all`, which errors when that path is
        // already a plain file.
        let d = home();
        std::fs::write(d.path().join("monitor"), b"not a directory").unwrap();

        let msg = register_fleet_run_monitor(d.path(), "demo", "run-2")
            .expect("a failed registration must say so, not stay silent");
        assert!(msg.contains("not monitored"), "{msg}");
        assert!(msg.contains("run-2"), "must name the run id: {msg}");
    }

    #[test]
    fn registering_twice_for_the_same_run_id_does_not_make_two_monitors() {
        let d = home();
        assert!(register_fleet_run_monitor(d.path(), "demo", "run-3").is_none());
        assert!(register_fleet_run_monitor(d.path(), "demo", "run-3").is_none());
        let store = MonitorStore::open_existing(d.path()).unwrap().unwrap();
        assert_eq!(store.list(&ListFilter::default()).unwrap().len(), 1);
    }

    #[test]
    fn a_name_over_the_limit_is_refused_and_creates_or_queues_nothing() {
        // spec §自動註冊邊界 clause 3: the caller must never claim monitoring
        // happened when nothing was created and nothing is pending. This
        // drives the real `Registered::Refused` arm (not the store-open
        // shortcut the test above uses, which returns before
        // `register_or_outbox`'s match is ever reached).
        // `MonitorSpec::validate` caps `name` at 64 chars (spec.rs); the
        // built name is `format!("fleet run: {fleet_name}")`, and
        // "fleet run: " alone is 11 of those, so a 60-char fleet_name pushes
        // the built name to 71 and fails `NameTooLong`.
        //
        // Mutation this closes: replacing the `Refused` arm's body with
        // `Registered::Refused(_) => None` (silently claiming success on a
        // rejected spec) makes the `.expect` below panic instead of the
        // assertions running.
        let d = home();
        let over_limit_fleet_name = "x".repeat(60);
        let msg = register_fleet_run_monitor(d.path(), &over_limit_fleet_name, "run-4")
            .expect("a refused registration must say so, not stay silent");
        assert!(msg.contains("not monitored"), "{msg}");
        assert!(msg.contains("run-4"), "must name the run id: {msg}");
        // Distinguishes Refused from Outboxed even if the text above were
        // muddled: Refused means nothing was created AND nothing is
        // pending, unlike Outboxed which leaves a row behind.
        assert!(
            !msg.contains("retry automatically"),
            "Refused must not promise a retry: {msg}"
        );

        let store = MonitorStore::open_existing(d.path()).unwrap().unwrap();
        assert!(
            store.list(&ListFilter::default()).unwrap().is_empty(),
            "a refused spec must create no monitor"
        );
        assert!(
            store.outbox_due(Utc::now(), 10).unwrap().is_empty(),
            "and nothing may be queued either"
        );
    }

    #[test]
    fn a_registration_that_fails_to_write_lands_in_the_outbox_and_says_so() {
        // spec §自動註冊邊界 clause 2, driving the real `Registered::Outboxed`
        // arm. `register.rs`'s own test for this same arm drops the
        // `monitors` table via `MonitorStore::conn()`, which is `pub(crate)`
        // to mur-monitor and unreachable from mur-core. Reached here a
        // different way that only needs public API: hand-create a
        // `monitors` table missing the `hard_reached` column BEFORE
        // `register_fleet_run_monitor`'s own `MonitorStore::open` ever
        // runs. `migrate()`'s `CREATE TABLE IF NOT EXISTS monitors` is then
        // a no-op (the table already exists) and its indexes still build
        // fine — they only touch `idempotency_key`/`state`/`next_check_at`
        // — but `store.create()`'s `INSERT INTO monitors (..., hard_reached,
        // ...)` fails on the missing column, so `register_or_outbox` falls
        // through to `outbox_enqueue`, which targets an unrelated, intact
        // table.
        //
        // Mutation this closes: collapsing `Outboxed` into `Monitored`'s
        // `None` return, or into `Refused`'s wording, fails one of the
        // assertions below (the `.expect`, or the exact-text checks that
        // only `Outboxed`'s message satisfies).
        let d = home();
        let dir = mur_monitor::store::db_dir(d.path());
        std::fs::create_dir_all(&dir).unwrap();
        let raw = rusqlite::Connection::open(dir.join(mur_monitor::store::DB_FILE)).unwrap();
        raw.execute_batch(
            "CREATE TABLE monitors (
                id                   TEXT PRIMARY KEY,
                name                 TEXT NOT NULL,
                spec_json            TEXT NOT NULL,
                state                TEXT NOT NULL,
                outcome              TEXT NOT NULL,
                source_type          TEXT NOT NULL,
                reference            TEXT NOT NULL,
                idempotency_key      TEXT NOT NULL,
                created_at           TEXT NOT NULL,
                work_started_at      TEXT NOT NULL,
                next_check_at        TEXT NOT NULL,
                last_checked_at      TEXT,
                last_progress_at     TEXT NOT NULL,
                progress_token       TEXT,
                pending_attempts     INTEGER NOT NULL DEFAULT 0,
                unknown_streak       INTEGER NOT NULL DEFAULT 0,
                remediation_attempts INTEGER NOT NULL DEFAULT 0,
                cycle_id             TEXT NOT NULL,
                stalled_since        TEXT,
                soft_notified        INTEGER NOT NULL DEFAULT 0,
                fence                INTEGER NOT NULL DEFAULT 0,
                version              INTEGER NOT NULL DEFAULT 1
            );",
        )
        .unwrap();
        drop(raw);

        let msg = register_fleet_run_monitor(d.path(), "demo", "run-5")
            .expect("an outboxed registration must say so, not stay silent");
        assert!(msg.contains("not monitored yet"), "{msg}");
        assert!(msg.contains("run-5"), "must name the run id: {msg}");
        assert!(
            msg.contains("retry automatically"),
            "Outboxed must promise a retry: {msg}"
        );

        // Count through a raw connection, not `list`: the `monitors` table
        // here is deliberately missing a column (that is what makes `create`
        // fail), so any reader that needs the real schema fails too — which
        // would report a broken fixture as a broken assertion.
        let raw =
            rusqlite::Connection::open(mur_monitor::store::db_dir(d.path()).join("monitors.db"))
                .unwrap();
        let monitors: i64 = raw
            .query_row("SELECT COUNT(*) FROM monitors", [], |r| r.get(0))
            .unwrap();
        assert_eq!(monitors, 0, "Outboxed must create no monitor row");
        drop(raw);

        let store = MonitorStore::open_existing(d.path()).unwrap().unwrap();
        let due = store.outbox_due(Utc::now(), 10).unwrap();
        assert_eq!(due.len(), 1, "the spec must land in the outbox");
        assert_eq!(due[0].spec.idempotency_key, "fleet:demo:run-5");
    }
}

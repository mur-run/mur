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
}

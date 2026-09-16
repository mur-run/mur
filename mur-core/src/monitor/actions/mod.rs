//! Executors for the monitor action verbs this build can actually run
//! (spec §行動執行器, plan-2 Task 3). `executor_for` is the single dispatch
//! point Task 4's gate and Task 5's drain consult; `gate.rs` (Task 4) lives
//! beside this file — this module is where its `pub mod gate;` line goes.

pub mod gate;
pub mod local;

use chrono::{DateTime, Utc};
use mur_monitor::adapter::AdapterRegistry;
use mur_monitor::store::{MonitorRow, MonitorStore};

/// Everything one action run needs, and nothing more: no network client of
/// its own, no secret beyond what an adapter already resolves through
/// `credential_ref`.
///
/// `registry` is not in the task-3 brief's original three-field sketch. It
/// was added because `collect_logs` cannot "ask the monitor's adapter for
/// its evidence" without a way to reach that adapter, and `mur-core`
/// already has exactly one place that builds one
/// (`mur_core::monitor::registry`, used by `service::tick_once`). Building
/// it once per tick and passing it down here — rather than reconstructing
/// it per action — mirrors that existing call shape. See task-3-report.md.
pub struct ActionCtx<'a> {
    pub store: &'a MonitorStore,
    pub row: &'a MonitorRow,
    pub now: DateTime<Utc>,
    pub registry: &'a AdapterRegistry,
}

pub trait ActionExecutor: Sync {
    fn verb(&self) -> &'static str;
    /// `Ok(summary)` → `ActionState::Done` with the summary as the stored
    /// result; `Err(reason)` → `ActionState::Failed`. Must never panic and
    /// must never touch the network beyond what `ActionCtx::registry`'s
    /// adapters already do for an ordinary check cycle — this slice adds
    /// no new credential scope.
    fn run(
        &self,
        ctx: &ActionCtx<'_>,
        params: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<String, String>;
}

static NOTIFY: local::Notify = local::Notify;
static COLLECT: local::CollectLogs = local::CollectLogs;
static RESCHEDULE: local::Reschedule = local::Reschedule;

/// `None` means "this build cannot run that verb" — including `rerun`,
/// `start_downstream` and `apply_known_remedy`, which are real entries in
/// `mur_monitor::spec::KNOWN_ACTIONS` but have no local executor yet.
/// Paired with Task 1's `risk::classify` fallback (`Privileged` for
/// anything it does not recognise), an unclassified verb is both gated
/// (never auto-approved) and unrunnable (no executor exists): two
/// independent defences, deliberately. Do not add a catch-all arm here —
/// a verb only becomes runnable by naming it explicitly.
pub fn executor_for(verb: &str) -> Option<&'static dyn ActionExecutor> {
    match verb {
        "notify" => Some(&NOTIFY),
        "collect_logs" => Some(&COLLECT),
        "reschedule_monitor" => Some(&RESCHEDULE),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use mur_monitor::adapter::{Observation, SourceAdapter};
    use mur_monitor::spec::{MonitorSpec, SourceType};
    use mur_monitor::state::MonitorState;

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 16, 9, 0, 0).unwrap()
    }

    fn spec_with_source(source_type: &str, idem: &str) -> MonitorSpec {
        MonitorSpec::from_yaml(&format!(
            "schema_version: 1\nname: t\nsource: {{ type: {source_type}, reference: r1 }}\nidempotency_key: {idem}\ncreated_by: {{ actor: user:test }}\n"
        ))
        .unwrap()
    }

    /// A generic monitor row — source type does not matter for `notify` /
    /// `reschedule_monitor` / the dispatch tests, which never touch
    /// `ctx.registry`.
    fn fixture() -> (tempfile::TempDir, MonitorStore, MonitorRow) {
        let d = tempfile::tempdir().unwrap();
        let s = MonitorStore::open(d.path()).unwrap();
        let created = s
            .create(&spec_with_source("mur_run", "k1"), t0(), None)
            .unwrap();
        // Actions only ever run for a monitor the scheduler parked in
        // `ActionPending` — `is_claimable` is `Active | Sleeping`, so a
        // settled monitor is never polled again and the drain is the only
        // thing that touches it. A fixture in any other state exercises a
        // situation production cannot produce.
        s.set_state(&created.id, MonitorState::ActionPending, t0())
            .unwrap();
        let row = s.get(&created.id).unwrap().unwrap();
        (d, s, row)
    }

    struct FakeAdapter {
        evidence: String,
    }

    impl SourceAdapter for FakeAdapter {
        fn source_type(&self) -> SourceType {
            SourceType::Custom
        }
        fn validate_reference(&self, _reference: &str) -> Result<(), String> {
            Ok(())
        }
        fn observe(&self, _reference: &str, _credential_ref: Option<&str>) -> Observation {
            Observation::pending("t1", self.evidence.clone())
        }
    }

    /// A `custom`-source monitor with a fake adapter registered for it,
    /// returning exactly `evidence` from `observe`.
    fn fixture_with_evidence(
        evidence: &str,
    ) -> (tempfile::TempDir, MonitorStore, MonitorRow, AdapterRegistry) {
        let d = tempfile::tempdir().unwrap();
        let s = MonitorStore::open(d.path()).unwrap();
        let created = s
            .create(&spec_with_source("custom", "k2"), t0(), None)
            .unwrap();
        let row = s.get(&created.id).unwrap().unwrap();
        let mut reg = AdapterRegistry::new();
        reg.register(Box::new(FakeAdapter {
            evidence: evidence.to_string(),
        }));
        (d, s, row, reg)
    }

    #[test]
    fn notify_appends_an_event_rather_than_delivering_directly() {
        let (_d, s, row) = fixture();
        let reg = AdapterRegistry::default();
        let ctx = ActionCtx {
            store: &s,
            row: &row,
            now: t0(),
            registry: &reg,
        };
        let out = executor_for("notify")
            .unwrap()
            .run(&ctx, &Default::default())
            .unwrap();
        let kinds: Vec<_> = s
            .events(&row.id)
            .unwrap()
            .into_iter()
            .map(|e| e.kind)
            .collect();
        assert!(kinds.contains(&"action_notify".to_string()), "{kinds:?}");
        assert!(!out.is_empty());
    }

    #[test]
    fn reschedule_pushes_the_next_check_out_and_returns_it_to_sleeping() {
        let (_d, s, row) = fixture();
        let before = s.get(&row.id).unwrap().unwrap().next_check_at;
        let reg = AdapterRegistry::default();
        let ctx = ActionCtx {
            store: &s,
            row: &row,
            now: t0(),
            registry: &reg,
        };
        executor_for("reschedule_monitor")
            .unwrap()
            .run(&ctx, &Default::default())
            .unwrap();
        let after = s.get(&row.id).unwrap().unwrap();
        // `after > before` alone would pass a literal duration, which rule 3
        // forbids. Pin it to the schedule the code must actually use: the
        // delay has to sit inside the jitter band `unknown_delay` produces for
        // this streak, not merely be positive.
        let base = mur_monitor::backoff::unknown_delay(row.unknown_streak);
        let moved = (after.next_check_at - before).to_std().unwrap();
        assert!(
            moved >= base / 2 && moved <= base * 2,
            "next check moved by {moved:?}, outside the backoff band around {base:?}"
        );
        assert_eq!(after.state, MonitorState::Sleeping);
    }

    #[test]
    fn collect_logs_records_evidence_and_no_secret() {
        // The adapter fixture returns evidence containing a token-shaped
        // string; the executor's own output must already be clean, not
        // rely on the store's redaction as the only line of defence.
        let (_d, s, row, reg) =
            fixture_with_evidence("token=ghp_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA ok");
        let ctx = ActionCtx {
            store: &s,
            row: &row,
            now: t0(),
            registry: &reg,
        };
        let out = executor_for("collect_logs")
            .unwrap()
            .run(&ctx, &Default::default())
            .unwrap();
        assert!(
            !out.contains("ghp_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
            "{out}"
        );
        assert!(out.contains("ok"), "{out}");
        // Tightened beyond the brief: a hard-coded `"ok"` (or `""`, which
        // would already fail the assert above) would still pass the two
        // asserts above without ever calling the adapter. These two require
        // the fixture's own evidence to have actually flowed through
        // `observe` and then `redact_secrets`.
        assert!(
            out.starts_with("token="),
            "must carry the adapter's own evidence, not a stub: {out:?}"
        );
        assert!(
            out.contains("[REDACTED:"),
            "the secret must be visibly redacted, not silently dropped: {out:?}"
        );
    }

    #[test]
    fn an_unknown_verb_has_no_executor() {
        // Pairs with Task 1's Privileged fallback: an unclassified verb is
        // both gated AND unrunnable. Two independent defences, on purpose.
        assert!(executor_for("apply_known_remedy").is_none());
        assert!(executor_for("rerun").is_none());
        assert!(executor_for("nonsense").is_none());
    }

    #[test]
    fn every_executor_is_registered_under_the_verb_it_reports() {
        // Paired with `an_unknown_verb_has_no_executor`: that test alone
        // would pass an `executor_for` stubbed to always return `None`.
        // This one requires the three known verbs to come back `Some` and
        // self-report the same verb they were looked up by.
        for v in ["notify", "collect_logs", "reschedule_monitor"] {
            assert_eq!(executor_for(v).unwrap().verb(), v);
        }
    }
}

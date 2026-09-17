//! Executors for the monitor action verbs this build can actually run
//! (spec §行動執行器, plan-2 Task 3). `executor_for` is the single dispatch
//! point Task 4's gate and Task 5's drain consult; `gate.rs` (Task 4) lives
//! beside this file — this module is where its `pub mod gate;` line goes.

pub mod gate;
pub mod local;
pub mod rerun;

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
/// The only thing an executor may write.
///
/// Executors used to receive `&MonitorStore`, which is every write in the
/// crate — including the ones that return a settled monitor to a claimable
/// state. That is exactly how the `reschedule_monitor` executor un-froze a
/// monitor's fence and made the whole action list re-run every ~10 s,
/// unbounded and silent, until a whole-branch review found it. The fix at
/// the time was to delete that executor; the hazard it proved is that
/// nothing *stopped* an executor from doing it, and a comment saying "do
/// not" is not a mechanism.
///
/// Appending an event is the entire store surface the three shipped
/// executors need. Handing over only that makes the rule structural: an
/// executor cannot write a monitor's state because it cannot reach the
/// method. It also removes four arguments `Notify` used to pass by hand and
/// could have passed wrongly — the monitor, the cycle and the clock now come
/// from the drain, which is the only place that knows them.
pub struct EventWriter<'a> {
    store: &'a MonitorStore,
    row: &'a MonitorRow,
    now: DateTime<Utc>,
}

impl<'a> EventWriter<'a> {
    pub fn new(store: &'a MonitorStore, row: &'a MonitorRow, now: DateTime<Utc>) -> Self {
        Self { store, row, now }
    }

    /// Append one event to this monitor's history, in its current cycle.
    ///
    /// `dedup: false` deliberately: the transition guards that write
    /// deduplicated events live in the scheduler, not in an action. An
    /// action firing twice is a claim bug, and silently swallowing the
    /// second write would hide it.
    pub fn append(&self, kind: &'static str, payload: serde_json::Value) -> Result<(), String> {
        self.store
            .append_event(
                &self.row.id,
                &self.row.cycle_id,
                kind,
                payload,
                false,
                self.now,
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

pub struct ActionCtx<'a> {
    /// Narrow by construction — see `EventWriter`. Not `&MonitorStore`.
    pub events: EventWriter<'a>,
    pub row: &'a MonitorRow,
    pub now: DateTime<Utc>,
    pub registry: &'a AdapterRegistry,
}

pub trait ActionExecutor: Sync {
    fn verb(&self) -> &'static str;
    /// `Ok(summary)` → `ActionState::Done` with the summary as the stored
    /// result; `Err(reason)` → `ActionState::Failed`. Must never panic.
    /// Every network call an executor makes must go through
    /// `ActionCtx::registry`'s adapters — `notify`/`collect_logs` reuse the
    /// same read-only credential an ordinary check cycle already has, but
    /// `rerun` (Task 3) is a genuine new WRITE under its own separate grant
    /// (`Source::write_credential_ref`), never a standing capability.
    fn run(
        &self,
        ctx: &ActionCtx<'_>,
        params: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<String, String>;
}

static NOTIFY: local::Notify = local::Notify;
static COLLECT: local::CollectLogs = local::CollectLogs;
static RERUN: rerun::Rerun = rerun::Rerun;

/// `None` means "this build cannot run that verb" — including
/// `start_downstream`, `apply_known_remedy` and `reschedule_monitor`, which
/// are real entries in `mur_monitor::spec::KNOWN_ACTIONS` but have no local
/// executor. Paired with Task 1's `risk::classify` fallback (`Privileged`
/// for anything it does not recognise), an unclassified verb is both gated
/// (never auto-approved) and unrunnable (no executor exists): two
/// independent defences, deliberately. Do not add a catch-all arm here —
/// a verb only becomes runnable by naming it explicitly.
///
/// `reschedule_monitor` is absent on purpose (whole-branch review H1). It
/// is the only verb that ever wrote a CLAIMABLE monitor state, and the
/// claim mechanism's exactly-once property rests on the opposite: a settled
/// monitor is never claimed again, so its fence is frozen, so the action
/// keys derived from that fence are fixed for the terminal's lifetime.
/// Returning a settled monitor to `Sleeping` unfroze the fence, the same
/// terminal was re-observed, and the entire action list re-ran under fresh
/// keys every poll interval. The invariant this restores is the MVP
/// acceptance criterion 「同一成功／失敗終態即使被觀測多次,也只執行一次副作用」,
/// and it holds structurally: NO executor in this module may write
/// `Active` or `Sleeping`, so nothing here can unfreeze a fence.
///
/// `reschedule_monitor`'s intended home was `on_unknown`, which ruling R4
/// established cannot reach the drain at all (`Outcome::Unknown` is not
/// terminal, and the terminal branch is the only thing that ever assigns
/// `ActionPending`). The unknown path already backs off through
/// `mur_monitor::backoff::unknown_delay` inside the scheduler; the deleted
/// executor duplicated that schedule, it did not enable it. Re-adding it
/// needs a way for the drain to tell "the same terminal, re-observed" from
/// "a human asked for another attempt" — today only the fence carries that,
/// and it is exactly what the verb destroyed.
pub fn executor_for(verb: &str) -> Option<&'static dyn ActionExecutor> {
    match verb {
        "notify" => Some(&NOTIFY),
        "collect_logs" => Some(&COLLECT),
        "rerun" => Some(&RERUN),
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
    use serde_json::{Map, Value};

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 16, 9, 0, 0).unwrap()
    }

    fn spec_with_source(source_type: &str, idem: &str) -> MonitorSpec {
        MonitorSpec::from_yaml(&format!(
            "schema_version: 1\nname: t\nsource: {{ type: {source_type}, reference: r1 }}\nidempotency_key: {idem}\ncreated_by: {{ actor: user:test }}\n"
        ))
        .unwrap()
    }

    /// A generic monitor row — source type does not matter for `notify` or
    /// the dispatch tests, which never touch `ctx.registry`.
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

    /// An adapter that could not read its source at all — a credential
    /// rejection, a network fault — returning `Observation::unknown`,
    /// which sets both `outcome: Unknown` and `adapter_error` (L6,
    /// whole-branch review).
    struct FailingAdapter {
        reason: String,
    }

    impl SourceAdapter for FailingAdapter {
        fn source_type(&self) -> SourceType {
            SourceType::Custom
        }
        fn validate_reference(&self, _reference: &str) -> Result<(), String> {
            Ok(())
        }
        fn observe(&self, _reference: &str, _credential_ref: Option<&str>) -> Observation {
            Observation::unknown(self.reason.clone())
        }
    }

    fn fixture_with_failure(
        reason: &str,
    ) -> (tempfile::TempDir, MonitorStore, MonitorRow, AdapterRegistry) {
        let d = tempfile::tempdir().unwrap();
        let s = MonitorStore::open(d.path()).unwrap();
        let created = s
            .create(&spec_with_source("custom", "k3"), t0(), None)
            .unwrap();
        let row = s.get(&created.id).unwrap().unwrap();
        let mut reg = AdapterRegistry::new();
        reg.register(Box::new(FailingAdapter {
            reason: reason.to_string(),
        }));
        (d, s, row, reg)
    }

    #[test]
    fn notify_appends_an_event_rather_than_delivering_directly() {
        let (_d, s, row) = fixture();
        let reg = AdapterRegistry::default();
        let ctx = ActionCtx {
            events: EventWriter::new(&s, &row, t0()),
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
    fn notify_redacts_secrets_in_params_before_writing_history() {
        // L4, whole-branch review: `params` comes straight from the
        // monitor's spec YAML, which can carry a secret pasted in by
        // mistake. `show --history` prints this event's payload raw, so
        // this is the one chokepoint before it reaches `monitor_events`.
        let (_d, s, row) = fixture();
        let reg = AdapterRegistry::default();
        let ctx = ActionCtx {
            events: EventWriter::new(&s, &row, t0()),
            row: &row,
            now: t0(),
            registry: &reg,
        };
        let mut params = Map::new();
        params.insert("note".to_string(), Value::String("hello".to_string()));
        params.insert(
            "message".to_string(),
            Value::String("token=ghp_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_string()),
        );
        executor_for("notify").unwrap().run(&ctx, &params).unwrap();
        let event = s
            .events(&row.id)
            .unwrap()
            .into_iter()
            .find(|e| e.kind == "action_notify")
            .unwrap();
        let payload = event.payload.to_string();
        assert!(
            !payload.contains("ghp_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
            "the secret must not reach monitor_events: {payload}"
        );
        assert!(
            payload.contains("[REDACTED:"),
            "must be visibly redacted, not silently dropped: {payload}"
        );
        // What would still make this green if the code were wrong? A `run`
        // that dropped `params` entirely (writing `{}`) would also hide
        // the secret, so a non-secret field must also survive intact.
        assert!(
            payload.contains("hello"),
            "must not wholesale drop params, only redact secrets within them: {payload}"
        );
    }

    #[test]
    fn collect_logs_records_evidence_and_no_secret() {
        // The adapter fixture returns evidence containing a token-shaped
        // string; the executor's own output must already be clean, not
        // rely on the store's redaction as the only line of defence.
        let (_d, s, row, reg) =
            fixture_with_evidence("token=ghp_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA ok");
        let ctx = ActionCtx {
            events: EventWriter::new(&s, &row, t0()),
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
    fn collect_logs_fails_when_the_source_could_not_be_read() {
        // L6, whole-branch review: a credential rejection or network fault
        // comes back as `Observation::unknown`, which sets `adapter_error`
        // and `outcome: Unknown`. The old code ignored both and stored the
        // error text as if it were collected evidence, with `ActionState::
        // Done`. That distinction — a MONITOR problem, not a work problem —
        // must survive down here: the executor has to return `Err`, not a
        // successful summary.
        let (_d, s, row, reg) = fixture_with_failure(
            "credential rejected: token=ghp_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        );
        let ctx = ActionCtx {
            events: EventWriter::new(&s, &row, t0()),
            row: &row,
            now: t0(),
            registry: &reg,
        };
        let err = executor_for("collect_logs")
            .unwrap()
            .run(&ctx, &Default::default())
            .unwrap_err();
        assert!(
            err.contains("could not be read"),
            "must say the SOURCE could not be read, not report a made-up success: {err}"
        );
        assert!(
            !err.contains("ghp_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
            "the adapter's own error text must be redacted too: {err}"
        );
        // What would still make this green if the code were wrong? An
        // executor that always returns `Err` regardless of the adapter's
        // outcome would also pass this test — `collect_logs_records_
        // evidence_and_no_secret` above is the companion positive control,
        // proving a healthy adapter still returns `Ok` with its evidence.
    }

    #[test]
    fn an_unknown_verb_has_no_executor() {
        // Pairs with Task 1's Privileged fallback: an unclassified verb is
        // both gated AND unrunnable. Two independent defences, on purpose.
        // `rerun` moved out of this test in Task 3 — it now has an executor
        // (see `every_executor_is_registered_under_the_verb_it_reports` and
        // `rerun::tests`).
        assert!(executor_for("apply_known_remedy").is_none());
        assert!(executor_for("nonsense").is_none());
    }

    /// Whole-branch review H1. `reschedule_monitor` is still a valid entry
    /// in `KNOWN_ACTIONS` and still classifies `Read`, so `MonitorSpec`
    /// accepts it and the gate waves it through — the ONLY thing standing
    /// between a user writing it and the unbounded re-observation loop is
    /// this `None`. Asserted separately from the runnable verbs above because
    /// those are unrunnable for a different reason (out of scope, no
    /// credential scope); this one is unrunnable because running it was
    /// unsafe.
    #[test]
    fn reschedule_monitor_is_named_and_classified_but_deliberately_unrunnable() {
        assert!(
            mur_monitor::spec::KNOWN_ACTIONS.contains(&"reschedule_monitor"),
            "the verb must still be a known action, or this test proves nothing"
        );
        assert_eq!(
            mur_monitor::action::risk::classify("reschedule_monitor"),
            mur_common::hitl::RiskTier::Read,
            "still Read tier, so nothing else would stop it running"
        );
        assert!(
            executor_for("reschedule_monitor").is_none(),
            "a runnable reschedule_monitor unfreezes a settled monitor's fence \
             and re-runs its whole action list on every poll"
        );
    }

    #[test]
    fn every_executor_is_registered_under_the_verb_it_reports() {
        // Paired with `an_unknown_verb_has_no_executor`: that test alone
        // would pass an `executor_for` stubbed to always return `None`.
        // This one requires every runnable verb to come back `Some` and
        // self-report the same verb they were looked up by.
        for v in ["notify", "collect_logs", "rerun"] {
            assert_eq!(executor_for(v).unwrap().verb(), v);
        }
    }
}

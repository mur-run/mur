//! The `rerun` executor (spec §行動執行器, durable-monitor-rerun plan, Task
//! 3): the first executor that performs an external WRITE, gated on the
//! spec's separate `source.write_credential_ref` grant (plan Task 1) and
//! reached through Task 2's `GithubActionsAdapter::rerun`.
//!
//! Two decisions this module encodes rather than leaves to the caller:
//!
//! **The monitor does not follow the new run.** Re-running failed jobs is a
//! new GitHub run attempt with nothing watching it — the spec's child-cycle
//! machinery (§混合處置策略 step 5, `monitor_cycles.parent_cycle_id`) is out
//! of scope and unwritten. Saying "watching it" here would be the exact
//! register-or-outbox lie Part C exists to prevent, so the result string
//! says plainly that the new run is **not monitored**.
//!
//! **A failed dispatch is a failed remediation, not a failed monitor.** An
//! `Err` here becomes `ActionState::Failed` and the drain's existing
//! `remediation_failed` event; nothing in this module writes to
//! `ctx.store`, so the monitor's own settled `outcome` is untouched either
//! way — a rerun we could not even dispatch says nothing about whether the
//! build passed.

use serde_json::{Map, Value};

use mur_monitor::spec::SourceType;

use super::{ActionCtx, ActionExecutor};

pub struct Rerun;

impl ActionExecutor for Rerun {
    fn verb(&self) -> &'static str {
        "rerun"
    }

    /// `rerun` is the one verb in this module whose adapter call is a real
    /// external write, so — unlike `notify`/`collect_logs` — it forwards
    /// `source.write_credential_ref`, never `source.credential_ref`: the
    /// HITL gate approved this one action, not a standing capability, and a
    /// credential supplied only so MUR could watch a run never authorised
    /// MUR to restart it.
    fn run(&self, ctx: &ActionCtx<'_>, _params: &Map<String, Value>) -> Result<String, String> {
        // A non-github_actions source has nothing to call. Checked against
        // `row.source_type` directly (not adapter-registry absence) so the
        // refusal names the real requirement even when some other adapter
        // happens to be registered under a different type.
        if ctx.row.source_type != SourceType::GithubActions {
            return Err(format!(
                "rerun is only supported for github_actions sources; this monitor's \
                 source is `{}`",
                ctx.row.source_type.as_str()
            ));
        }
        let adapter = ctx
            .registry
            .get(SourceType::GithubActions)
            .ok_or_else(|| "no github_actions adapter registered".to_string())?;
        let write_credential_ref = ctx.row.spec.source.write_credential_ref.as_deref();
        let summary = adapter
            .rerun(&ctx.row.reference, write_credential_ref)
            .map_err(|e| mur_common::redact::redact_secrets(&e).into_owned())?;
        // Redacted defense-in-depth, same reasoning as `collect_logs`: this
        // string becomes the action's stored result immediately, before any
        // store-side pass runs on it.
        Ok(mur_common::redact::redact_secrets(&format!(
            "{summary} for {} — the new run is not monitored; register a \
             separate monitor if you want it watched",
            ctx.row.reference
        ))
        .into_owned())
    }
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, TimeZone, Utc};

    use mur_common::hitl::RiskTier;
    use mur_monitor::action::risk;
    use mur_monitor::adapter::{AdapterRegistry, Observation, SourceAdapter};
    use mur_monitor::spec::{MonitorSpec, SourceType};
    use mur_monitor::state::MonitorState;
    use mur_monitor::store::{MonitorRow, MonitorStore};

    use super::super::{ActionCtx, executor_for};

    /// Stands in for the resolved secret value a real `write_credential_ref`
    /// would point at. Used as the fixture's `write_credential_ref` itself
    /// (not a `env:`/`keychain:` scheme) purely so
    /// `the_result_never_carries_the_token` is non-vacuous: it proves the
    /// executor's own formatting never echoes this field, not merely that
    /// an unrelated string is absent.
    const TEST_TOKEN_VALUE: &str = "ghp_TASK3TESTTOKENVALUE00000000000000000";
    const OK_REFERENCE: &str = "o/r/42";
    const FORBIDDEN_REFERENCE: &str = "o/r/FORBIDDEN";

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 17, 9, 0, 0).unwrap()
    }

    /// Stands in for `GithubActionsAdapter`. Task 2 already covers the real
    /// HTTP/credential-resolution behaviour end to end without a mock
    /// server (`github_actions.rs` deliberately has no stub-server harness,
    /// and must not grow one); this double's only job is the EXECUTOR's own
    /// plumbing — which grant it forwards, how it maps `Ok`/`Err`, and that
    /// a failure never reaches the store — parameterised by `reference` so
    /// one `reg()` serves every scenario below.
    struct FakeGithubRerun;

    impl SourceAdapter for FakeGithubRerun {
        fn source_type(&self) -> SourceType {
            SourceType::GithubActions
        }
        fn validate_reference(&self, _reference: &str) -> Result<(), String> {
            Ok(())
        }
        fn observe(&self, _reference: &str, _credential_ref: Option<&str>) -> Observation {
            panic!("the rerun executor must never call observe")
        }
        fn rerun(
            &self,
            reference: &str,
            write_credential_ref: Option<&str>,
        ) -> Result<String, String> {
            assert_eq!(
                write_credential_ref,
                Some(TEST_TOKEN_VALUE),
                "the executor must forward the row's write_credential_ref verbatim"
            );
            if reference == FORBIDDEN_REFERENCE {
                Err(
                    "forbidden (403) — the credential needs actions:write to rerun jobs"
                        .to_string(),
                )
            } else {
                Ok("rerun requested (http 201)".to_string())
            }
        }
    }

    fn reg() -> AdapterRegistry {
        let mut r = AdapterRegistry::new();
        r.register(Box::new(FakeGithubRerun));
        r
    }

    fn row_with(
        source_type: &str,
        reference: &str,
        idem: &str,
    ) -> (tempfile::TempDir, MonitorStore, MonitorRow) {
        let d = tempfile::tempdir().unwrap();
        let s = MonitorStore::open(d.path()).unwrap();
        let spec = MonitorSpec::from_yaml(&format!(
            "schema_version: 1\n\
             name: t\n\
             source: {{ type: {source_type}, reference: {reference}, \
             write_credential_ref: {TEST_TOKEN_VALUE} }}\n\
             actions:\n  on_failure:\n    - type: rerun\n\
             idempotency_key: {idem}\n\
             created_by: {{ actor: user:test }}\n"
        ))
        .unwrap();
        let created = s.create(&spec, t0(), None).unwrap();
        // `rerun`, like every action, only ever runs for a monitor the
        // scheduler parked in `ActionPending`.
        s.set_state(&created.id, MonitorState::ActionPending, t0())
            .unwrap();
        let row = s.get(&created.id).unwrap().unwrap();
        (d, s, row)
    }

    fn fixture_with_write_grant() -> (tempfile::TempDir, MonitorStore, MonitorRow) {
        row_with("github_actions", OK_REFERENCE, "rerun-ok")
    }

    fn fixture_where_rerun_403s() -> (tempfile::TempDir, MonitorStore, MonitorRow) {
        row_with("github_actions", FORBIDDEN_REFERENCE, "rerun-403")
    }

    fn fixture_with_source(
        source_type: SourceType,
    ) -> (tempfile::TempDir, MonitorStore, MonitorRow) {
        row_with(source_type.as_str(), OK_REFERENCE, "rerun-wrong-source")
    }

    #[test]
    fn a_successful_rerun_records_the_new_run_and_says_it_is_unwatched() {
        let (_d, s, row) = fixture_with_write_grant();
        let ctx = ActionCtx {
            store: &s,
            row: &row,
            now: t0(),
            registry: &reg(),
        };
        let out = executor_for("rerun")
            .unwrap()
            .run(&ctx, &Default::default())
            .unwrap();
        assert!(
            out.contains("not monitored"),
            "must not imply we are watching it: {out}"
        );
        // Self-review: a hard-coded `"not monitored"` (or that phrase alone)
        // would also pass the assert above without the adapter ever having
        // run. Require the reference AND the adapter's own summary too, so
        // a stub that never called `FakeGithubRerun::rerun` cannot pass.
        assert!(out.contains(OK_REFERENCE), "must name which run: {out}");
        assert!(
            out.contains("rerun requested"),
            "must carry the adapter's own confirmation, not just a static \
             disclaimer: {out}"
        );
    }

    #[test]
    fn a_rerun_failure_is_an_action_failure_not_a_work_failure() {
        let (_d, s, row) = fixture_where_rerun_403s();
        let ctx = ActionCtx {
            store: &s,
            row: &row,
            now: t0(),
            registry: &reg(),
        };
        let e = executor_for("rerun")
            .unwrap()
            .run(&ctx, &Default::default())
            .unwrap_err();
        assert!(e.contains("actions:write"), "{e}");
        // The monitor's own verdict is untouched: a rerun we could not
        // dispatch says nothing about whether the build passed.
        assert_eq!(s.get(&row.id).unwrap().unwrap().outcome, row.outcome);
    }

    #[test]
    fn the_result_never_carries_the_token() {
        let (_d, s, row) = fixture_where_rerun_403s();
        let ctx = ActionCtx {
            store: &s,
            row: &row,
            now: t0(),
            registry: &reg(),
        };
        let e = executor_for("rerun")
            .unwrap()
            .run(&ctx, &Default::default())
            .unwrap_err();
        assert!(!e.contains(TEST_TOKEN_VALUE), "{e}");
    }

    #[test]
    fn rerun_is_registered_and_still_classified_write() {
        assert_eq!(executor_for("rerun").unwrap().verb(), "rerun");
        assert_eq!(
            risk::classify("rerun"),
            RiskTier::Write,
            "having an executor must not lower its tier"
        );
        // Pairs `classify` with the OTHER question about this same verb —
        // Task 1's `needs_write_grant` — so the two cannot silently drift
        // apart now that `rerun` is both gated AND runnable.
        assert!(
            risk::needs_write_grant("rerun"),
            "an executor that performs a real write must still require the grant"
        );
    }

    #[test]
    fn a_source_that_is_not_github_refuses_rather_than_pretending() {
        // `rerun` on a `mur_run` monitor has nothing to call.
        let (_d, s, row) = fixture_with_source(SourceType::MurRun);
        let ctx = ActionCtx {
            store: &s,
            row: &row,
            now: t0(),
            registry: &reg(),
        };
        let e = executor_for("rerun")
            .unwrap()
            .run(&ctx, &Default::default())
            .unwrap_err();
        assert!(e.contains("github_actions"), "{e}");
    }
}

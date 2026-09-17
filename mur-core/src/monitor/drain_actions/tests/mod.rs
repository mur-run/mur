//! Tests for the action drain. Split out of `mod.rs`, and then again into
//! `approval.rs`, as pure code movement to stay under CLAUDE.md's
//! 800-line-per-file rule; nothing changed in either move.
//!
//! This file holds the shared fixtures plus the claim/routing/containment
//! tests; everything about the HITL gate — parking, late approval, the
//! remediation cap, drift retirement — is in `approval.rs`, which reaches
//! the fixtures here through its own `use super::*`.

use super::*;
use chrono::TimeZone;
use mur_channel::ChannelService;
use mur_common::channel::{ChannelActor, EventKind};
use mur_common::hitl::HitlResponse;
use mur_monitor::adapter::{Observation, SourceAdapter};
use mur_monitor::scheduler;
use mur_monitor::spec::{MonitorSpec, SourceType};
use mur_monitor::store::CycleUpdate;

use crate::monitor::actions::gate::channel_id_for;

fn t0() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 16, 9, 0, 0).unwrap()
}

/// `verbs` land in `on_success` (outcome `Succeeded`) or `on_failure`
/// (any other terminal outcome), matching `actions_for_outcome`'s own
/// routing. `policy` is spliced in verbatim so a test can override
/// `max_remediation_attempts`; `idem` must differ per monitor, because
/// `create` returns the EXISTING row for a repeated `idempotency_key`.
fn spec_for(
    source_type: &str,
    verbs: &[&str],
    outcome: Outcome,
    idem: &str,
    policy: &str,
) -> MonitorSpec {
    let list: String = verbs.iter().map(|v| format!("    - type: {v}\n")).collect();
    let key = if outcome == Outcome::Succeeded {
        "on_success"
    } else {
        "on_failure"
    };
    MonitorSpec::from_yaml(&format!(
        "schema_version: 1\nname: t\nsource: {{ type: {source_type}, reference: r1 }}\nactions:\n  {key}:\n{list}{policy}idempotency_key: {idem}\ncreated_by: {{ actor: user:test }}\n"
    ))
    .unwrap()
}

/// Settle one spec into an EXISTING store through the production path
/// (`scheduler::plan_cycle` + `apply_cycle`), so a fixture can hold two
/// monitors that one `drain_actions` call sees together. `apply_cycle`'s
/// `bool` is asserted, never discarded: it returns `Ok(false)` having
/// written NOTHING for a stale fence or a missing monitor, which would
/// leave the row `Active`/`pending` — invisible to Phase 1, and every
/// "no actions ran" assertion downstream green for the wrong reason.
fn settle_into(s: &MonitorStore, spec: &MonitorSpec, outcome: Outcome) -> String {
    let created = s.create(spec, t0(), None).unwrap();
    let row = s.get(&created.id).unwrap().unwrap();
    let update = scheduler::plan_cycle(&row, Observation::terminal(outcome, "done"), t0());
    assert!(
        s.apply_cycle(&created.id, row.fence, &update).unwrap(),
        "fixture write was refused: the monitor never reached ActionPending"
    );
    created.id
}

fn settled_with_policy(
    source_type: &str,
    verbs: &[&str],
    outcome: Outcome,
    policy: &str,
) -> (tempfile::TempDir, std::path::PathBuf, String) {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().to_path_buf();
    let s = MonitorStore::open(&home).unwrap();
    let id = settle_into(
        &s,
        &spec_for(source_type, verbs, outcome, "k", policy),
        outcome,
    );
    (d, home, id)
}

fn settled_with_source(
    source_type: &str,
    verbs: &[&str],
    outcome: Outcome,
) -> (tempfile::TempDir, std::path::PathBuf, String) {
    settled_with_policy(source_type, verbs, outcome, "")
}

fn settled_with(
    verbs: &[&str],
    outcome: Outcome,
) -> (tempfile::TempDir, std::path::PathBuf, String) {
    settled_with_source("mur_run", verbs, outcome)
}

fn store(home: &Path) -> MonitorStore {
    MonitorStore::open_existing(home).unwrap().unwrap()
}

fn event_kinds(s: &MonitorStore, id: &str) -> Vec<String> {
    s.events(id).unwrap().into_iter().map(|e| e.kind).collect()
}

/// Approve one action on the monitor's derived channel, keyed on
/// `action_hash` exactly as `mur channel approve` is, through the same
/// unsigned `ChannelService::append` path `gate.rs`'s
/// `an_approval_already_on_the_channel_releases_the_gate` uses. The hash
/// comes from `expected_hash`, so no `HitlRequest` need be parked first:
/// `scan_prior` matches on the hash, never on a `hitl_id` — which is
/// what makes a late (or standing) approval releasable at all.
fn approve(home: &Path, row: &MonitorRow, verb: &str, index: usize) {
    let resp = HitlResponse {
        hitl_id: format!("hitl-test-{verb}-{index}"),
        action_hash: expected_hash(row, verb, index, &serde_json::Map::new()),
        allow: true,
        reason: "test".into(),
        surface: "cli".into(),
    };
    ChannelService::open(home)
        .unwrap()
        .append(
            &channel_id_for(&row.id),
            ChannelActor::System,
            EventKind::HitlResponse,
            serde_json::to_value(&resp).unwrap(),
            None,
        )
        .unwrap();
}

mod approval;

/// Mirrors `a_home_with_no_store_drains_no_actions_and_creates_nothing`
/// in `service.rs` for notifications: an empty `ActionReport` comes back
/// whether or not the store was created, so the assertion that actually
/// proves the fix has to look at the filesystem.
#[test]
fn a_home_with_no_store_drains_no_actions_and_creates_nothing() {
    let d = tempfile::tempdir().unwrap();
    let dir = mur_monitor::store::db_dir(d.path());
    assert!(!dir.exists());
    let rt = tokio::runtime::Runtime::new().unwrap();
    assert_eq!(
        drain_actions(d.path(), rt.handle(), t0()).unwrap(),
        ActionReport::default()
    );
    assert!(!dir.exists(), "draining must not create the store");
}

#[test]
fn a_settled_monitor_with_one_read_action_runs_it_once_and_completes() {
    let (_d, home, id) = settled_with(&["notify"], Outcome::Succeeded);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let first = drain_actions(&home, rt.handle(), t0()).unwrap();
    assert_eq!(first.executed, 1);
    // What would still make this green if the claim were broken? A
    // second `executed == 1` would pass if `claim_action` always
    // returned `true` — so the second call is not padding, it is the
    // only thing separating "claimed once" from "runs every tick".
    let second = drain_actions(&home, rt.handle(), t0()).unwrap();
    assert_eq!(second.executed, 0, "the claim must stop a second run");
    assert_eq!(
        store(&home).get(&id).unwrap().unwrap().state,
        MonitorState::Completed
    );
}

/// R4's central defence, reshaped from the brief's `on_unknown` version
/// (unimplementable, see `actions_for_outcome`'s doc): zero actions at
/// all, not merely "not the failure list" — this fixture populates
/// `on_success` too, so a mistaken `Unknown → on_success` routing would
/// survive the weaker check.
///
/// "No rows" is satisfied by emptiness, so the test carries its own
/// positive control: a monitor identical but for a terminal outcome, in
/// the SAME store, drained by the SAME call, must produce a row that
/// really ran. Without it, a `drain_actions` that returns
/// `Ok(ActionReport::default())` on line one passes.
#[test]
fn an_unknown_outcome_produces_no_actions_at_all() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().to_path_buf();
    let both_lists = |idem: &str| {
        MonitorSpec::from_yaml(&format!(
            "schema_version: 1\nname: t\nsource: {{ type: mur_run, reference: r1 }}\nactions:\n  on_success:\n    - type: notify\n  on_failure:\n    - type: notify\n    - type: rerun\nidempotency_key: {idem}\ncreated_by: {{ actor: user:test }}\n"
        ))
        .unwrap()
    };
    let (unknown, control) = {
        let s = MonitorStore::open(&home).unwrap();
        // Hand-parked in `ActionPending` with `outcome: Unknown` and
        // BOTH lists non-empty — a combination the real scheduler can
        // never produce. `Observation::terminal` `debug_assert!`s
        // against `Unknown`, so the outcome is overridden on the
        // `CycleUpdate`, which carries no such invariant.
        let created = s.create(&both_lists("k-unknown"), t0(), None).unwrap();
        let row = s.get(&created.id).unwrap().unwrap();
        let update = CycleUpdate {
            outcome: Outcome::Unknown,
            observation: Observation::unknown("source query failed"),
            observed_at: t0(),
            new_state: MonitorState::ActionPending,
            next_check_at: t0(),
            pending_attempts: row.pending_attempts,
            unknown_streak: row.unknown_streak + 1,
            last_progress_at: row.last_progress_at,
            progress_token: row.progress_token.clone(),
            stalled_since: row.stalled_since,
            soft_notified: row.soft_notified,
            hard_reached: row.hard_reached,
            finish_cycle: true,
            events: Vec::new(),
        };
        assert!(
            s.apply_cycle(&created.id, row.fence, &update).unwrap(),
            "fixture write was refused: nothing was parked in ActionPending"
        );
        let parked = s.get(&created.id).unwrap().unwrap();
        assert_eq!(parked.state, MonitorState::ActionPending);
        assert_eq!(parked.outcome, Outcome::Unknown);
        (
            created.id,
            settle_into(&s, &both_lists("k-control"), Outcome::Failed),
        )
    };

    let rt = tokio::runtime::Runtime::new().unwrap();
    drain_actions(&home, rt.handle(), t0()).unwrap();

    let s = store(&home);
    let rows = s.actions_for(&unknown).unwrap();
    assert!(rows.is_empty(), "unknown must run no actions: {rows:?}");
    let control_rows = s.actions_for(&control).unwrap();
    assert!(
        control_rows.iter().any(|a| a.state == ActionState::Done),
        "the control must really have run, or 'no rows' proves nothing: {control_rows:?}"
    );
}

#[test]
fn a_failing_executor_does_not_fail_the_tick() {
    // spec §錯誤處理, same rule the notification drain follows. There is
    // no local `Read` verb whose executor can fail structurally except
    // `collect_logs` against a source type the production registry never
    // registers an adapter for. `custom` is exactly that:
    // `mur_core::monitor::registry` wires up `mur_run`/`github_actions`/
    // `codex`/`claude_code`, never `Custom`, so `AdapterRegistry::get`
    // returns `None` and `CollectLogs::run` fails for a real structural
    // reason — no fake adapter or injected failure needed.
    let (_d, home, _id) = settled_with_source("custom", &["collect_logs"], Outcome::Failed);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let r = drain_actions(&home, rt.handle(), t0()).unwrap();
    assert_eq!(r.failed, 1);
    // `unwrap()` above IS the assertion: an Err would have panicked.
}

/// Observes the same terminal every time it is asked, so a monitor that is
/// wrongly returned to a claimable state really does re-settle on the SAME
/// outcome — the shape H1 produced in production. An empty registry would
/// yield `Observation::unknown` instead, which never re-parks
/// `ActionPending` and would hide the bug.
struct AlwaysTheSameTerminal;

impl SourceAdapter for AlwaysTheSameTerminal {
    fn source_type(&self) -> SourceType {
        SourceType::MurRun
    }
    fn validate_reference(&self, _reference: &str) -> Result<(), String> {
        Ok(())
    }
    fn observe(&self, _reference: &str, _credential_ref: Option<&str>) -> Observation {
        Observation::terminal(Outcome::Failed, "still the same failed run")
    }
}

/// Whole-branch review H1, and the MVP acceptance criterion
/// 「同一成功／失敗終態即使被觀測多次,也只執行一次副作用」.
///
/// `reschedule_monitor` used to be a live executor that wrote `Sleeping`
/// onto a monitor the scheduler had just settled. `is_claimable` is
/// `Active | Sleeping`, so the next tick re-claimed it — bumping the fence
/// — re-observed the same terminal, and `plan_cycle`'s terminal branch
/// re-parked it in `ActionPending`. The bumped fence produced fresh
/// `action_key`s, so `claim_action` did not collide and the WHOLE list ran
/// again. Every poll interval. Forever, and silently: the `terminal` event
/// is deduped per (monitor, cycle) and `cycle_id` never rotates, so nothing
/// was ever notified about the loop.
///
/// Four independent assertions, because no single one of them is safe on
/// its own:
/// - the settled monitor's fence must not move across a real
///   `scheduler::tick` — that IS the mechanism, and it fails under the old
///   code even if the adapter below were misconfigured;
/// - the control monitor's fence MUST move in the same tick, so "the fence
///   did not move" can never be green merely because this fixture cannot
///   claim anything at all;
/// - exactly ONE `action_notify` event after the second drain — `== 1`, so
///   a `drain_actions` that ran nothing (the emptiness trap this project
///   keeps hitting) goes red here rather than passing;
/// - exactly TWO action rows, so a second terminal episode minting fresh
///   keys is caught even if its side effect were deduped somewhere else.
#[test]
fn a_terminal_list_with_reschedule_monitor_runs_once_and_leaves_the_fence_frozen() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().to_path_buf();
    let (id, control) = {
        let s = MonitorStore::open(&home).unwrap();
        let id = settle_into(
            &s,
            &spec_for(
                "mur_run",
                &["reschedule_monitor", "notify"],
                Outcome::Failed,
                "k-loop",
                "",
            ),
            Outcome::Failed,
        );
        // An ordinary, never-settled monitor with no action list: the
        // positive control for the tick below. Same store, same tick call.
        let control = s
            .create(
                &MonitorSpec::from_yaml(
                    "schema_version: 1\nname: control\nsource: { type: mur_run, reference: r2 }\n\
                     idempotency_key: k-control\ncreated_by: { actor: user:test }\n",
                )
                .unwrap(),
                t0(),
                None,
            )
            .unwrap()
            .id;
        (id, control)
    };

    let rt = tokio::runtime::Runtime::new().unwrap();
    let first = drain_actions(&home, rt.handle(), t0()).unwrap();
    assert_eq!(
        (first.executed, first.blocked, first.failed),
        (1, 0, 1),
        "notify runs; reschedule_monitor is classified Read but has no executor"
    );

    let (fence_before, control_fence_before) = {
        let s = store(&home);
        let m = s.get(&id).unwrap().unwrap();
        assert_eq!(m.state, MonitorState::Completed);
        assert!(
            !m.state.is_claimable(),
            "an actioned terminal must never be claimable again"
        );
        let rows = s.actions_for(&id).unwrap();
        let resched = rows
            .iter()
            .find(|a| verb_and_index_from_key(&a.action_key) == Some(("reschedule_monitor", 0)))
            .expect("the reschedule_monitor row must exist — it was claimed, just not runnable");
        assert_eq!(resched.state, ActionState::Failed);
        let why = resched.result.clone().unwrap_or_default();
        assert!(
            why.contains("no executor") && why.contains("reschedule_monitor"),
            "the row must say why it did not run, not fail silently: {why}"
        );
        (m.fence, s.get(&control).unwrap().unwrap().fence)
    };

    // A real scheduler pass, with an adapter that keeps returning the same
    // terminal — exactly what the daemon does ~10 s after a reschedule.
    let later = t0() + chrono::Duration::hours(1);
    let mut registry = AdapterRegistry::new();
    registry.register(Box::new(AlwaysTheSameTerminal));
    let report = scheduler::tick(&store(&home), &registry, later, "test-owner", 10).unwrap();
    assert_eq!(
        report.claimed, 1,
        "only the control is claimable; if this is 0 the tick proved nothing"
    );

    {
        let s = store(&home);
        assert_eq!(
            s.get(&id).unwrap().unwrap().fence,
            fence_before,
            "a settled monitor must never be re-claimed — its frozen fence is what \
             keeps this terminal's action keys stable"
        );
        assert_ne!(
            s.get(&control).unwrap().unwrap().fence,
            control_fence_before,
            "the control must really have been claimed, or the assertion above is vacuous"
        );
    }

    let second = drain_actions(&home, rt.handle(), later).unwrap();
    assert_eq!(
        second,
        ActionReport::default(),
        "the same terminal must not be actioned a second time"
    );

    let s = store(&home);
    let notifies = s
        .events(&id)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "action_notify")
        .count();
    assert_eq!(notifies, 1, "one terminal, one side effect");
    assert_eq!(
        s.actions_for(&id).unwrap().len(),
        2,
        "no fresh action key may be minted for a terminal that was already actioned"
    );
}

/// M3 (whole-branch review; the ledger rated this LOW and was wrong).
/// `decide(...)?` was the one `?` in this subsystem that propagated, and
/// both drain phases `?` on `attempt_action`, so a single `Err` aborted the
/// WHOLE drain mid-loop. The daemon catches it at the tick boundary and
/// carries on, so a one-off is invisible — but a persistent fault (an
/// unreadable channel event file for one monitor, a signing-key failure in
/// `append_as_writer`) aborted at the same monitor on every tick and NO
/// monitor's actions ever ran again. spec §錯誤處理 wants a failure in one
/// unit recorded and retried, never propagated — which every other path in
/// this subsystem already does.
///
/// The poison is structural, not injected: `events.jsonl` is replaced by a
/// DIRECTORY, so `fs::read_to_string` fails with something that is not
/// `NotFound` and `ChannelStore::load_events` returns `Err` rather than the
/// empty vec a missing channel gives. A merely corrupt line would not do —
/// `load_events` skips those by design. The poisoned monitor's verb must be
/// GATED (`rerun`): a `Read`-tier action is answered by `gate`'s `Auto` arm
/// before it ever opens the channel, so a `notify` here would sail past the
/// poison and the test would prove nothing.
///
/// `drain_actions(...).unwrap()` IS the red assertion and it does not
/// depend on which monitor Phase 1 reaches first: under the old code the
/// `Err` propagates whether the poisoned monitor is walked before or after
/// the healthy one. The other two assertions separate "did not propagate"
/// from "did nothing": the healthy monitor's action must really have run,
/// and the poisoned row must carry the reason AND still be retryable.
#[test]
fn a_poisoned_gate_stops_its_own_monitor_and_no_other() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().to_path_buf();
    let (poisoned, healthy) = {
        let s = MonitorStore::open(&home).unwrap();
        (
            settle_into(
                &s,
                &spec_for("mur_run", &["rerun"], Outcome::Failed, "k-poison", ""),
                Outcome::Failed,
            ),
            settle_into(
                &s,
                &spec_for("mur_run", &["notify"], Outcome::Failed, "k-healthy", ""),
                Outcome::Failed,
            ),
        )
    };

    let events = home
        .join("channels")
        .join(channel_id_for(&poisoned))
        .join("events.jsonl");
    std::fs::create_dir_all(&events).unwrap();
    assert!(events.is_dir(), "the poison must actually be in place");

    let rt = tokio::runtime::Runtime::new().unwrap();
    let rep = drain_actions(&home, rt.handle(), t0()).unwrap();

    let s = store(&home);
    assert_eq!(
        rep.executed, 1,
        "the healthy monitor's action must still run: {rep:?}"
    );
    let healthy_rows = s.actions_for(&healthy).unwrap();
    assert_eq!(healthy_rows[0].state, ActionState::Done);
    assert!(
        event_kinds(&s, &healthy).contains(&"action_notify".to_string()),
        "and must really have had its side effect"
    );

    let stuck = s.actions_for(&poisoned).unwrap().remove(0);
    assert_eq!(
        stuck.state,
        ActionState::Claimed,
        "a gate fault is retried, not settled"
    );
    assert!(
        stuck
            .result
            .unwrap_or_default()
            .contains("approval gate failed"),
        "the reason must be recorded on the row, not only propagated away"
    );
    assert!(
        s.pending_actions(t0(), 10, 10)
            .unwrap()
            .iter()
            .any(|a| a.monitor_id == poisoned),
        "and the next tick must pick it back up"
    );
}

#[test]
fn a_key_yields_both_its_verb_and_its_index() {
    // Phase 2's only handle on what a parked row was FOR: the index
    // alone resolves to whatever verb now sits at that slot.
    let key = action_key("mon-1", "cyc-2", 7, "rerun", 3);
    assert_eq!(verb_and_index_from_key(&key), Some(("rerun", 3)));
    assert_eq!(verb_and_index_from_key("nonsense"), None);
}

/// The settle guard only does anything when the resolver is ON, and every
/// other test in this file runs with it OFF — so without this one, a fully
/// green suite would prove only that a disabled feature is inert. It was
/// written after exactly that happened: a green run that stayed green when
/// the Phase-1 stamp was silently missing from the file.
///
/// Asserts both directions in one monitor: held on the tick its action
/// failed, released on the next. The release matters as much as the hold —
/// a consultation that could not happen (no model is configured here) must
/// still let the monitor settle, or an unreachable resolver would strand
/// every failed monitor on the machine.
#[test]
fn an_enabled_resolver_holds_a_failed_cycle_open_for_exactly_one_tick() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().to_path_buf();
    std::fs::write(
        home.join("config.yaml"),
        "monitor_resolver:\n  enabled: true\n",
    )
    .unwrap();

    let id = {
        let s = MonitorStore::open(&home).unwrap();
        settle_into(
            &s,
            // `reschedule_monitor` is classified but has no executor, so its
            // action reliably ends `Failed` — the condition the guard reads.
            &spec_for(
                "mur_run",
                &["reschedule_monitor"],
                Outcome::Failed,
                "k-resolver-hold",
                "",
            ),
            Outcome::Failed,
        )
    };

    let rt = tokio::runtime::Runtime::new().unwrap();
    drain_actions(&home, rt.handle(), t0()).unwrap();
    assert_eq!(
        store(&home).get(&id).unwrap().unwrap().state,
        MonitorState::ActionPending,
        "a failed cycle must wait for its consultation rather than completing \
         in the same tick its last action failed"
    );

    drain_actions(&home, rt.handle(), t0()).unwrap();
    assert_eq!(
        store(&home).get(&id).unwrap().unwrap().state,
        MonitorState::Completed,
        "once the cycle is stamped consulted the monitor must settle, whatever \
         the consultation produced"
    );
}

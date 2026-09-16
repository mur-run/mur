//! Tests for the action drain. Split out of `mod.rs` as pure code
//! movement to stay under CLAUDE.md's 800-line-per-file rule; nothing
//! here changed in the move.

use super::*;
use chrono::TimeZone;
use mur_channel::ChannelService;
use mur_common::channel::{ChannelActor, EventKind};
use mur_common::hitl::HitlResponse;
use mur_monitor::adapter::Observation;
use mur_monitor::scheduler;
use mur_monitor::spec::MonitorSpec;
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

/// Two properties in one fixture, because the second is the first held
/// over time. Tick 1: `rerun` classifies above `Read`
/// (mur-monitor/src/action/risk.rs) and has no `auto_approve_tiers`
/// entry in `decide()`'s hardcoded policy, so with no TTY and no prior
/// approval it parks — blocked, not failed.
///
/// Ticks 2-6: it is still parked. Spec 「高風險 action 必須停在
/// `awaiting_approval`」 and CLAUDE.md's "unattended approvals defer,
/// they do not time out". Counting a deferral as a remediation attempt
/// exhausted the monitor after three ticks — 45 s at the daemon's 15 s
/// cadence — and Phase 2 skips `Exhausted` monitors, so the approval
/// could then never land. Six drains, because the default cap is 3.
#[test]
fn a_parked_action_stores_the_id_channel_approve_actually_matches_on() {
    // The bug this pins: `GateDecision` carried only `action_hash`, so
    // the parked row stored the hash and `mur monitor show` printed
    // `mur channel approve <channel> <hash>` — a command `approve`
    // rejects, because it matches strictly on the `hitl_id` the gate
    // minted. The monitor was parked with no reachable way to release
    // it, and every existing test stayed green because none of them
    // went near the real approve path.
    let (_d, home, id) = settled_with(&["rerun"], Outcome::Failed);
    let rt = tokio::runtime::Runtime::new().unwrap();
    drain_actions(&home, rt.handle(), t0()).unwrap();

    let s = store(&home);
    let action = s.actions_for(&id).unwrap().into_iter().next().unwrap();
    let approval_id = action
        .approval_id
        .expect("a parked action must name its request");
    assert!(
        approval_id.starts_with("hitl-"),
        "must store the gate's hitl_id, not the action_hash: {approval_id}"
    );

    // And that id must be the one actually on the channel, not merely
    // hitl-shaped: read the request back the way `approve` does.
    let row = s.get(&id).unwrap().unwrap();
    let channel = crate::monitor::actions::gate::channel_id_for(&row.id);
    let svc = mur_channel::ChannelService::open(&home).unwrap();
    let found = svc
        .load_events(&channel)
        .unwrap()
        .into_iter()
        .filter_map(|e| serde_json::from_value::<mur_common::hitl::HitlRequest>(e.payload).ok())
        .any(|r| r.hitl_id == approval_id);
    assert!(found, "no HitlRequest on {channel} carries {approval_id}");
}

#[test]
fn a_gated_action_parks_and_keeps_waiting_without_spending_the_budget() {
    let (_d, home, id) = settled_with(&["rerun"], Outcome::Failed);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let first = drain_actions(&home, rt.handle(), t0()).unwrap();
    assert_eq!(
        (first.executed, first.blocked, first.failed),
        (0, 1, 0),
        "blocked is not failed"
    );
    for _ in 0..5 {
        drain_actions(&home, rt.handle(), t0()).unwrap();
    }
    let s = store(&home);
    let m = s.get(&id).unwrap().unwrap();
    assert_eq!(
        m.remediation_attempts, 0,
        "waiting for a human is not remediating"
    );
    assert_eq!(
        m.state,
        MonitorState::AwaitingApproval,
        "a parked action must never time out into Exhausted"
    );
    // The only thing that tells a human a monitor is waiting on them —
    // deleting `record_approval_required` left every other test green.
    let kinds = event_kinds(&s, &id);
    assert!(
        kinds.contains(&"approval_required".to_string()),
        "a deferral must say so: {kinds:?}"
    );
}

/// The payoff of the whole slice: the answer arrives later and the next
/// tick proceeds without asking again (`gate.rs` proves the gate does
/// this; nothing proved the drain acts on it).
///
/// What a released action can DO is bounded by this build: every gated
/// verb has no executor here, so the honest terminal state is `Failed`
/// naming the verb plus a `remediation_failed` event — never silence.
/// That makes `Blocked` → `Failed`-with-that-reason the proof the gate
/// released: a still-deferring gate leaves the row `Blocked`, spends no
/// budget and appends no `remediation_failed`. All three are asserted,
/// so no single mutation satisfies them by accident.
#[test]
fn an_approval_that_lands_later_releases_the_parked_action() {
    let (_d, home, id) = settled_with(&["rerun"], Outcome::Failed);
    let rt = tokio::runtime::Runtime::new().unwrap();
    drain_actions(&home, rt.handle(), t0()).unwrap();
    let row = {
        let s = store(&home);
        let m = s.get(&id).unwrap().unwrap();
        // Negative control: before the approval it really is parked.
        assert_eq!(m.state, MonitorState::AwaitingApproval);
        assert_eq!(
            s.actions_for(&id).unwrap()[0].state,
            ActionState::Blocked,
            "the action must be waiting before the approval is written"
        );
        m
    };
    approve(&home, &row, "rerun", 0);

    drain_actions(&home, rt.handle(), t0()).unwrap();

    let s = store(&home);
    let action = s.actions_for(&id).unwrap().remove(0);
    assert_eq!(
        action.state,
        ActionState::Failed,
        "an approved action must leave Blocked, not sit there forever"
    );
    let result = action.result.unwrap_or_default();
    assert!(
        result.contains("no executor") && result.contains("rerun"),
        "the reason must name the verb this build cannot run: {result}"
    );
    let kinds = event_kinds(&s, &id);
    assert!(
        kinds.contains(&"remediation_failed".to_string()),
        "approval then silence is the failure this feature removes: {kinds:?}"
    );
    let m = s.get(&id).unwrap().unwrap();
    assert_ne!(m.state, MonitorState::AwaitingApproval);
    assert_eq!(
        m.remediation_attempts, 1,
        "a remedy actually attempted counts exactly once"
    );
}

/// The cap is 2 here, never the default 3
/// (`mur_monitor::spec::DEFAULT_MAX_REMEDIATION`), precisely so a
/// hardcoded `>= 3` in the drain cannot keep this green: with a literal,
/// two attempts never exhaust and the state assertion goes red.
///
/// Three gated actions, all approved before the first tick, against a
/// cap of 2 — exactly the shape that overshot. Phase 1 walks them in one
/// pass from a single `store.list()` snapshot, so reading
/// `remediation_attempts` from that snapshot gives every sibling the
/// count as it was before any of them incremented it and all three run.
/// Asserting the total EQUALS the cap (not `<=`) is what catches that.
#[test]
fn reaching_the_remediation_cap_exhausts_the_monitor_and_stops_acting() {
    let (_d, home, id) = settled_with_policy(
        "mur_run",
        &["rerun", "rerun", "rerun"],
        Outcome::Failed,
        "policy:\n  max_remediation_attempts: 2\n",
    );
    {
        let s = store(&home);
        let row = s.get(&id).unwrap().unwrap();
        assert_eq!(
            row.spec.policy.max_remediation_attempts, 2,
            "the fixture must not use the default cap"
        );
        for index in 0..3 {
            approve(&home, &row, "rerun", index);
        }
    }
    let rt = tokio::runtime::Runtime::new().unwrap();
    drain_actions(&home, rt.handle(), t0()).unwrap();

    let s = store(&home);
    let m = s.get(&id).unwrap().unwrap();
    let cap = m.spec.policy.max_remediation_attempts;
    assert_eq!(
        m.remediation_attempts, cap,
        "the cap must be reached exactly, never overshot"
    );
    assert_eq!(m.state, MonitorState::Exhausted);
    let attempted = s
        .actions_for(&id)
        .unwrap()
        .into_iter()
        .filter(|a| a.state == ActionState::Failed)
        .count();
    assert_eq!(attempted, cap as usize, "no remedy past the cap may run");
    let kinds = event_kinds(&s, &id);
    assert!(kinds.contains(&"exhausted".to_string()), "{kinds:?}");
    assert!(
        kinds.contains(&"remediation_failed".to_string()),
        "{kinds:?}"
    );
}

/// A monitor is `Completed` only when every action in its list settled.
/// One `Read` action runs, one gated action parks: the monitor waits.
/// Letting the `Read` action's success decide the monitor's fate would
/// report success while a remedy nobody approved is still owed.
#[test]
fn a_monitor_whose_second_action_is_gated_waits_instead_of_completing() {
    let (_d, home, id) = settled_with(&["notify", "rerun"], Outcome::Failed);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let r = drain_actions(&home, rt.handle(), t0()).unwrap();
    assert_eq!((r.executed, r.blocked, r.failed), (1, 1, 0));
    let s = store(&home);
    assert_eq!(
        s.get(&id).unwrap().unwrap().state,
        MonitorState::AwaitingApproval,
        "one action still owed means the monitor is not Completed"
    );
    let states: Vec<_> = s
        .actions_for(&id)
        .unwrap()
        .into_iter()
        .map(|a| a.state)
        .collect();
    assert!(
        states.contains(&ActionState::Done) && states.contains(&ActionState::Blocked),
        "both halves must be real: {states:?}"
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

/// Fix round 2: a Phase-2 row whose key names a verb the monitor's current
/// action list no longer has at that index used to `continue` forever —
/// `Claimed`/`Blocked`, permanently occupying one of the ten oldest-first
/// `pending_actions` slots. Reproduces the real mechanism the
/// `action.r#type != verb` comment already describes: the monitor's
/// OUTCOME flips under a parked row (never its `spec`, which `MonitorRow`
/// holds fixed for the monitor's whole life — `spec_json` is written once
/// at `create` and there is no update verb), so index 0 resolves against a
/// different list without `cycle_id` ever rotating (it is set once at
/// `create` and never touched again — `store/mod.rs`), which is exactly
/// what lets Phase 2's `cycle_id` check pass through to the verb check.
#[test]
fn a_row_whose_verb_drifted_out_from_under_it_is_retired_not_skipped_forever() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().to_path_buf();
    let spec = MonitorSpec::from_yaml(
        "schema_version: 1\nname: t\nsource: { type: mur_run, reference: r1 }\n\
         actions:\n  on_failure:\n    - type: rerun\n  on_success:\n    - type: notify\n\
         idempotency_key: k\ncreated_by: { actor: user:test }\n",
    )
    .unwrap();
    let id = {
        let s = MonitorStore::open(&home).unwrap();
        settle_into(&s, &spec, Outcome::Failed)
    };

    let rt = tokio::runtime::Runtime::new().unwrap();
    // Tick 1: claims `rerun` at index 0 of `on_failure` and parks it — gated,
    // no TTY, nothing approved yet (same shape as
    // `a_gated_action_parks_and_keeps_waiting_without_spending_the_budget`).
    let first = drain_actions(&home, rt.handle(), t0()).unwrap();
    assert_eq!((first.executed, first.blocked, first.failed), (0, 1, 0));
    let action_key = {
        let s = store(&home);
        let a = s.actions_for(&id).unwrap().into_iter().next().unwrap();
        assert_eq!(
            a.state,
            ActionState::Blocked,
            "must really be parked before the flip"
        );
        a.action_key
    };

    // The outcome flips under the parked row without rotating `cycle_id` —
    // hand-built the same way `an_unknown_outcome_produces_no_actions_at_all`
    // manufactures a combination the real scheduler races into only rarely.
    // After this, Phase 2 resolves index 0 against `on_success` (`notify`)
    // instead of the `on_failure` (`rerun`) list the parked key named.
    {
        let s = store(&home);
        let row = s.get(&id).unwrap().unwrap();
        let update = CycleUpdate {
            outcome: Outcome::Succeeded,
            observation: Observation::terminal(Outcome::Succeeded, "flipped for the test"),
            observed_at: t0(),
            new_state: MonitorState::AwaitingApproval,
            next_check_at: t0(),
            pending_attempts: row.pending_attempts,
            unknown_streak: row.unknown_streak,
            last_progress_at: row.last_progress_at,
            progress_token: row.progress_token.clone(),
            stalled_since: row.stalled_since,
            soft_notified: row.soft_notified,
            hard_reached: row.hard_reached,
            finish_cycle: true,
            events: Vec::new(),
        };
        assert!(
            s.apply_cycle(&id, row.fence, &update).unwrap(),
            "fixture write was refused: the flip never landed"
        );
    }

    // Tick 2: Phase 2 must retire the row now, not skip it again.
    let second = drain_actions(&home, rt.handle(), t0()).unwrap();
    assert_eq!(
        second.failed, 1,
        "the drifted row must be retired this tick, not skipped"
    );
    let (state, result) = {
        let s = store(&home);
        let a = s
            .actions_for(&id)
            .unwrap()
            .into_iter()
            .find(|a| a.action_key == action_key)
            .unwrap();
        (a.state, a.result.unwrap_or_default())
    };
    assert_eq!(
        state,
        ActionState::Failed,
        "a row that can never resolve must be retired terminally, not left parked"
    );
    assert!(
        result.contains("rerun") && result.contains("notify"),
        "the reason must name the drift — what the key said and what is there now: {result}"
    );

    // "Must not come back" is the negative-assertion trap the brief warns
    // about: `ActionReport::default()` on a further call proves nothing by
    // itself — the OLD `continue` behaviour also produced an empty report
    // on every tick after the first, forever, because a silently skipped
    // row makes no noise either. `pending_actions()` only ever returns
    // `Claimed`/`Blocked` rows (`store/action.rs`), so a genuinely `Failed`
    // row structurally cannot be re-claimed or re-blocked — snapshot the
    // row's exact `attempt`/`result` and prove a further drain leaves both
    // byte-identical, not merely that the report stayed quiet.
    let before = store(&home)
        .actions_for(&id)
        .unwrap()
        .into_iter()
        .find(|a| a.action_key == action_key)
        .unwrap();
    let third = drain_actions(&home, rt.handle(), t0()).unwrap();
    assert_eq!(
        third,
        ActionReport::default(),
        "nothing is left for this monitor to do"
    );
    let after = store(&home)
        .actions_for(&id)
        .unwrap()
        .into_iter()
        .find(|a| a.action_key == action_key)
        .unwrap();
    assert_eq!(after.state, before.state, "the row must not come back");
    assert_eq!(
        after.attempt, before.attempt,
        "the row must not be reprocessed"
    );
    assert_eq!(
        after.result, before.result,
        "the recorded reason must not change"
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

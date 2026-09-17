//! The HITL half of the action-drain suite: parking, the late approval
//! that releases it, the remediation cap, and drift retirement. Split out
//! of `tests/mod.rs` as pure code movement (CLAUDE.md rule 4); the
//! fixtures it uses (`settled_with*`, `approve`, `store`, `event_kinds`,
//! `t0`) still live there and arrive through `use super::*`.

use super::*;

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
    // M1: the action ROW's own counter, which no test looked at. Six drains
    // parked the same request six times and `show` printed "(6 attempts)"
    // for something never attempted; at the daemon's 15 s cadence that is
    // 240 an hour. The gate returns the SAME `hitl_id` each time
    // (`Prior::Pending`), so a re-deferral is the same wait continuing.
    // `== 1`, not `== 0`: the first park is a real event and must count, so
    // a `block_action` that stopped counting altogether goes red here.
    let action = s.actions_for(&id).unwrap().remove(0);
    assert_eq!(
        action.state,
        ActionState::Blocked,
        "still parked, or the counter assertion below is about the wrong state"
    );
    assert_eq!(
        action.attempt, 1,
        "re-gating a parked row is not another attempt"
    );
}

/// The payoff of the whole slice: the answer arrives later and the next
/// tick proceeds without asking again (`gate.rs` proves the gate does
/// this; nothing proved the drain acts on it).
///
/// What a released action can DO is bounded by the fixture: this monitor is
/// a `mur_run`, and `rerun` only works on `github_actions`, so the honest
/// terminal state is `Failed` naming the verb plus a `remediation_failed`
/// event — never silence. (Until the rerun slice, the reason was "no
/// executor" for every gated verb; `rerun` has one now, which is why this
/// asserts on the verb rather than on that phrase.)
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
    // This assertion used to read `contains("no executor")`, written when no
    // verb both gated and had an executor. `rerun` now has one, so an
    // approved `rerun` reaches it — and refuses here only because this
    // fixture's monitor is a `mur_run`, which has nothing to rerun. What the
    // test guards is unchanged: the approval released the action and the
    // reason names the verb, rather than the action sitting Blocked forever.
    assert!(
        result.contains("github_actions"),
        "must prove the approval reached the LIVE executor, not the no-executor \
         arm: only the executor's own refusal names the required source type, \
         while the verb name appears in both messages: {result}"
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

/// M2, whole-branch review. `pending_actions`'s old single query
/// (`ORDER BY created_at ASC LIMIT max`) let BLOCKED rows fill the whole
/// window forever: a blocked row's `created_at` never changes
/// (`block_action` does not touch it), so past
/// `DRAIN_MAX_ACTIONS_PER_TICK` simultaneously-parked actions, the
/// newest ones dropped out of every future Phase 2 pass — an approval
/// written for one of them was never looked at again. The fix gives
/// blocked-row rechecks their own budget
/// (`DRAIN_MAX_BLOCKED_PER_TICK`), independent of and much larger than
/// the fresh-claim budget, so a parked row's position among its peers
/// can never exclude it.
///
/// Eleven monitors, one over `DRAIN_MAX_ACTIONS_PER_TICK` (10): the
/// eleventh is deliberately the one still `ActionPending` after tick 1
/// (Phase 1's budget is spent on the other ten), so tick 2 claims and
/// gates it with a strictly LATER `created_at` than the other ten — the
/// exact ordering an oldest-first `LIMIT` would drop first.
#[test]
fn an_approval_on_the_eleventh_parked_action_still_releases_it() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().to_path_buf();
    let ids: Vec<String> = {
        let s = MonitorStore::open(&home).unwrap();
        (0..11)
            .map(|i| {
                settle_into(
                    &s,
                    &spec_for("mur_run", &["rerun"], Outcome::Failed, &format!("k{i}"), ""),
                    Outcome::Failed,
                )
            })
            .collect()
    };

    let rt = tokio::runtime::Runtime::new().unwrap();
    // Tick 1: Phase 1's budget (10) claims and gates ten of the eleven,
    // leaving exactly one monitor still `ActionPending`.
    drain_actions(&home, rt.handle(), t0()).unwrap();
    let s = store(&home);
    let still_pending = s
        .list(&ListFilter {
            state: Some(MonitorState::ActionPending),
            include_completed: true,
        })
        .unwrap();
    assert_eq!(
        still_pending.len(),
        1,
        "the fixture must have more parked actions than one tick's budget"
    );
    let last_id = still_pending[0].id.clone();

    // Tick 2, a later `now`: claims and gates the eleventh action, giving
    // it a `created_at` strictly after the other ten's.
    let later = t0() + chrono::Duration::seconds(1);
    drain_actions(&home, rt.handle(), later).unwrap();
    let s = store(&home);
    for id in &ids {
        assert_eq!(
            s.actions_for(id).unwrap().remove(0).state,
            ActionState::Blocked,
            "all eleven must be parked before the approval: {id}"
        );
    }

    // Approve only the eleventh (last-in-line) action.
    let last_row = s.get(&last_id).unwrap().unwrap();
    approve(&home, &last_row, "rerun", 0);
    drain_actions(&home, rt.handle(), later + chrono::Duration::seconds(1)).unwrap();

    let s = store(&home);
    let released = s.actions_for(&last_id).unwrap().remove(0);
    assert_eq!(
        released.state,
        ActionState::Failed,
        "the approved action, even though it is last in line, must still be released"
    );
    let result = released.result.unwrap_or_default();
    assert!(
        result.contains("github_actions"),
        "must prove the approval reached the LIVE executor, not the no-executor \
         arm: only the executor's own refusal names the required source type, \
         while the verb name appears in both messages: {result}"
    );
    assert!(
        event_kinds(&s, &last_id).contains(&"remediation_failed".to_string()),
        "approval then silence is exactly the starvation this test guards against"
    );

    // Negative control: a drain that "releases everything regardless"
    // would also satisfy every assertion above. The other ten were never
    // approved and must still be sitting there untouched.
    for id in &ids {
        if id == &last_id {
            continue;
        }
        assert_eq!(
            s.actions_for(id).unwrap().remove(0).state,
            ActionState::Blocked,
            "an un-approved action must not be released just because \
             a budget fix shipped: {id}"
        );
    }
}

/// The write grant the fixtures below carry. A `SecretRef`-shaped string,
/// because the executor forwards this field verbatim to the adapter and
/// `FakeGithubRerunThroughTheDrain` asserts on it — a bare value here would
/// make the forwarding assertion read like a token echo.
const TEST_WRITE_GRANT: &str = "env:MUR_TEST_GH_WRITE_GRANT";
/// A run the fake adapter reruns successfully, and one it refuses. Keyed on
/// the reference rather than on a flag so one registry serves both cases.
const RERUN_OK_REFERENCE: &str = "o/r/42";
const RERUN_REFUSED_REFERENCE: &str = "o/r/FORBIDDEN";

/// Stands in for `GithubActionsAdapter` inside a REAL `drain_actions` pass.
///
/// This double exists because the drain's happy path for a gated action is
/// otherwise untestable: `rerun` is the only verb that is both above `Read`
/// tier and has an executor, and the real adapter's success path is an HTTP
/// POST. `observe` panics — the drain must never observe, and a fixture
/// that silently fell back to polling would make the assertions below mean
/// something else.
struct FakeGithubRerunThroughTheDrain;

impl SourceAdapter for FakeGithubRerunThroughTheDrain {
    fn source_type(&self) -> SourceType {
        SourceType::GithubActions
    }
    fn validate_reference(&self, _reference: &str) -> Result<(), String> {
        Ok(())
    }
    fn observe(&self, _reference: &str, _credential_ref: Option<&str>) -> Observation {
        panic!("the action drain must never observe a settled monitor")
    }
    fn rerun(&self, reference: &str, write_credential_ref: Option<&str>) -> Result<String, String> {
        assert_eq!(
            write_credential_ref,
            Some(TEST_WRITE_GRANT),
            "the drain must forward the spec's write grant, never the read credential"
        );
        if reference == RERUN_REFUSED_REFERENCE {
            Err("forbidden (403) — the credential needs actions:write to rerun jobs".to_string())
        } else {
            Ok("rerun requested (http 201)".to_string())
        }
    }
}

fn github_registry() -> AdapterRegistry {
    let mut r = AdapterRegistry::new();
    r.register(Box::new(FakeGithubRerunThroughTheDrain));
    r
}

/// A settled-`Failed` `github_actions` monitor whose single `on_failure`
/// action is `rerun`, carrying the write grant that action needs.
/// `spec_for` cannot build this: it hardcodes the reference and has no
/// `write_credential_ref`.
fn settled_github_rerun(
    reference: &str,
    max_attempts: u32,
) -> (tempfile::TempDir, std::path::PathBuf, String) {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().to_path_buf();
    let spec = MonitorSpec::from_yaml(&format!(
        "schema_version: 1\nname: t\n\
         source: {{ type: github_actions, reference: {reference}, \
         write_credential_ref: {TEST_WRITE_GRANT} }}\n\
         actions:\n  on_failure:\n    - type: rerun\n\
         policy:\n  max_remediation_attempts: {max_attempts}\n\
         idempotency_key: k\ncreated_by: {{ actor: user:test }}\n"
    ))
    .unwrap();
    let id = {
        let s = MonitorStore::open(&home).unwrap();
        settle_into(&s, &spec, Outcome::Failed)
    };
    (d, home, id)
}

/// Park the monitor's single `rerun`, approve it, and let the next tick
/// run it — the drive shared by the two tests below, which differ only in
/// what the adapter does when it is finally reached.
fn park_approve_and_run(
    home: &std::path::Path,
    id: &str,
) -> (ActionReport, MonitorRow, mur_monitor::store::ActionRow) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let reg = github_registry();
    let first = drain_actions_with(home, rt.handle(), t0(), &reg).unwrap();
    assert_eq!(
        (first.executed, first.blocked, first.failed),
        (0, 1, 0),
        "a gated action must park before anything approves it"
    );
    {
        let s = store(home);
        let row = s.get(id).unwrap().unwrap();
        assert_eq!(
            row.remediation_attempts, 0,
            "waiting for a human is not remediating"
        );
        approve(home, &row, "rerun", 0);
    }
    let second = drain_actions_with(home, rt.handle(), t0(), &reg).unwrap();
    let s = store(home);
    let row = s.get(id).unwrap().unwrap();
    let action = s.actions_for(id).unwrap().remove(0);
    (second, row, action)
}

/// F1, fix round 1. The feature's happy path through the drain — approval
/// → attempt counted → executor `Ok` → post-attempt cap check →
/// `maybe_complete_monitor` — which nothing on this branch or on `main`
/// exercised, because `rerun` is the first verb that is both gated and
/// runnable.
///
/// `max_remediation_attempts: 1` with `on_failure: [rerun]` is the natural
/// spelling of "try exactly one rerun". The post-attempt cap check ran on
/// every gated arm including `Ok`, so the one attempt hit the cap, the
/// monitor was set `Exhausted` — final, since `maybe_complete_monitor`
/// early-returns on any other state — an `exhausted` event was appended,
/// and because `exhausted` is in `NOTIFIABLE` the user got a desktop
/// notification saying MUR gave up, on the one occasion it did not.
///
/// `Completed` alone would NOT prove the fix: an action that ends `Failed`
/// is also "settled", so a rerun that never reached the adapter would reach
/// `Completed` too on a build where the cap check was simply deleted.
/// Hence all four together — `Done`, the adapter's own confirmation in the
/// result, no `exhausted` event, and the attempt still counted — plus the
/// negative control below, which is the same fixture and the same cap with
/// a refusing adapter and must still exhaust.
#[test]
fn a_gated_remedy_that_worked_completes_the_monitor_instead_of_saying_mur_gave_up() {
    let (_d, home, id) = settled_github_rerun(RERUN_OK_REFERENCE, 1);
    let (rep, row, action) = park_approve_and_run(&home, &id);

    assert_eq!(
        (rep.executed, rep.exhausted),
        (1, 0),
        "an approved remedy that worked is executed, not given up on"
    );
    assert_eq!(
        action.state,
        ActionState::Done,
        "the approved rerun must reach the executor and succeed"
    );
    let result = action.result.unwrap_or_default();
    assert!(
        result.contains("rerun requested") && result.contains(RERUN_OK_REFERENCE),
        "must carry the adapter's own confirmation and name the run, so a Done \
         written without ever calling the adapter cannot pass: {result}"
    );
    assert_eq!(
        row.remediation_attempts, row.spec.policy.max_remediation_attempts,
        "the attempt itself is still counted — the fix is about the verdict, \
         not about the count"
    );
    assert_eq!(
        row.state,
        MonitorState::Completed,
        "a monitor whose only remedy succeeded is Completed, never Exhausted"
    );
    let kinds = event_kinds(&store(&home), &id);
    assert!(
        !kinds.contains(&"exhausted".to_string()),
        "no `exhausted` event may be written after a remedy that worked — it \
         is notifiable and tells the user MUR gave up: {kinds:?}"
    );
    assert!(
        !kinds.contains(&"remediation_failed".to_string()),
        "nothing failed to remediate here: {kinds:?}"
    );
}

/// The negative control for the test above, and the reason it cannot pass
/// on a build that simply deleted the cap check: identical fixture,
/// identical cap of 1, identical approval — only the adapter refuses. The
/// budget must still be spent and the monitor must still end `Exhausted`
/// with the event that tells a human to `mur monitor retry`.
#[test]
fn a_gated_remedy_that_failed_still_exhausts_the_monitor_at_the_same_cap() {
    let (_d, home, id) = settled_github_rerun(RERUN_REFUSED_REFERENCE, 1);
    let (rep, row, action) = park_approve_and_run(&home, &id);

    assert_eq!(
        (rep.executed, rep.failed, rep.exhausted),
        (0, 1, 1),
        "a remedy that did not remediate spends the budget"
    );
    assert_eq!(action.state, ActionState::Failed);
    let result = action.result.unwrap_or_default();
    assert!(
        result.contains("actions:write"),
        "the reason must be the adapter's own refusal, not a generic one: {result}"
    );
    assert_eq!(row.remediation_attempts, 1);
    assert_eq!(
        row.state,
        MonitorState::Exhausted,
        "the cap must still be enforced on the arms that failed to remediate"
    );
    let kinds = event_kinds(&store(&home), &id);
    assert!(
        kinds.contains(&"exhausted".to_string())
            && kinds.contains(&"remediation_failed".to_string()),
        "a human must be told both that the remedy failed and that MUR stopped \
         trying: {kinds:?}"
    );
}

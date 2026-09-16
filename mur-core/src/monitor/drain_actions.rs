//! Drain monitors the scheduler parked in `ActionPending` — the piece that
//! makes the actions built in Tasks 1-4 actually run (spec §行動執行器).
//!
//! Split out of `service.rs` as pure code movement (CLAUDE.md rule 4, single
//! source file ≤ 800 lines): the public interface stays `service::{
//! drain_actions, ActionReport, DRAIN_MAX_ACTIONS_PER_TICK }` via the
//! re-export in `service.rs`, so callers (the daemon) see no change.

use std::path::Path;

use anyhow::Result;
use chrono::{DateTime, Utc};
use mur_common::hitl::RiskTier;
use mur_monitor::action::{ActionState, action_key, risk};
use mur_monitor::adapter::AdapterRegistry;
use mur_monitor::spec::Action;
use mur_monitor::state::{MonitorState, Outcome};
use mur_monitor::store::{ListFilter, MonitorRow, MonitorStore};

use super::actions::gate::{decide, expected_hash};
use super::actions::{ActionCtx, executor_for};

/// Bounds one `drain_actions` call's total actions attempted — claims made
/// fresh in Phase 1 plus retries/crash-recovery in Phase 2 — spec §行動執行器
/// rule 6. Mirrors `DRAIN_MAX_PER_TICK`'s reasoning for notifications: a
/// backlog after downtime spreads across ticks instead of running an
/// unbounded number of actions in one pass.
pub const DRAIN_MAX_ACTIONS_PER_TICK: usize = 10;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ActionReport {
    pub executed: usize,
    pub blocked: usize,
    pub failed: usize,
    pub exhausted: usize,
}

/// Which action list a settled monitor's outcome resolves to (spec
/// §行動執行器). `Unknown` resolves to no actions at all — never
/// `on_failure` — because a monitor that could not read its source is not
/// work that failed; that is the spec's central invariant, carried down to
/// the action layer.
///
/// The task-5 brief's routing table names an `on_unknown` list, but
/// `Outcome::Unknown` can never reach this function through the real
/// pipeline: `Outcome::is_terminal()` excludes `Unknown`, and
/// `scheduler::plan_cycle`'s terminal branch — the only place that ever
/// assigns `MonitorState::ActionPending` — is gated on
/// `obs.outcome.is_terminal()`. Routing `on_unknown` here would be dead
/// code; `backoff::unknown_delay` already handles the unknown case (see
/// `actions::local::Reschedule`). The `Unknown => &[]` arm below is this
/// function's own independent defence against ever treating an unreadable
/// source as a failure — verified by
/// `an_unknown_outcome_produces_no_actions_at_all`, which hand-builds a
/// `CycleUpdate` since the real scheduler cannot produce the combination.
fn actions_for_outcome(row: &MonitorRow) -> &[Action] {
    match row.outcome {
        Outcome::Succeeded => &row.spec.actions.on_success,
        Outcome::Failed | Outcome::Cancelled => &row.spec.actions.on_failure,
        Outcome::Pending | Outcome::Unknown => &[],
    }
}

/// Recovers `(verb, action_index)` from an `ActionRow::action_key`
/// (`<monitor-id>:<cycle-id>:<version>:<verb>:<index>`) for Phase 2's retry
/// path — the row carries neither as its own column. Safe because
/// `action_key`'s own doc guarantees no field may contain `:` (ids are
/// UUIDs), so the last segment is always the index and the
/// second-from-last always the verb.
///
/// The verb is not decoration: Phase 2 resolves the list from the monitor's
/// CURRENT outcome, which can have flipped under a parked row, so index N
/// may now name a different verb. Writing `Done` onto the old key would
/// record in the action ledger — the spec's evidence (§冪等與事件紀錄) —
/// that a remedy completed when it never ran.
fn verb_and_index_from_key(key: &str) -> Option<(&str, usize)> {
    let mut segments = key.rsplit(':');
    let index = segments.next()?.parse().ok()?;
    let verb = segments.next()?;
    Some((verb, index))
}

/// Rule 5: once every action row for the monitor's current cycle has
/// settled (`Done` or `Failed` — none `Claimed` or `Blocked`), the monitor
/// leaves `ActionPending`/`AwaitingApproval` for `Completed`.
///
/// Compares against the length of the RESOLVED action list, not merely the
/// rows that happen to exist yet: a `DRAIN_MAX_ACTIONS_PER_TICK`-truncated
/// pass can claim and finish action 0 of a multi-action list while running
/// out of budget before claiming the rest, and completing the monitor on
/// "the one row I have is settled" would silently skip the unclaimed tail.
fn maybe_complete_monitor(
    store: &MonitorStore,
    row: &MonitorRow,
    now: DateTime<Utc>,
) -> Result<()> {
    if !matches!(
        row.state,
        MonitorState::ActionPending | MonitorState::AwaitingApproval
    ) {
        return Ok(());
    }
    let list = actions_for_outcome(row);
    if list.is_empty() {
        return Ok(());
    }
    let cycle_rows: Vec<_> = store
        .actions_for(&row.id)?
        .into_iter()
        .filter(|a| a.cycle_id == row.cycle_id)
        .collect();
    if cycle_rows.len() < list.len() {
        // The tail of the list has not even been claimed yet this episode.
        return Ok(());
    }
    let settled = cycle_rows
        .iter()
        .all(|a| matches!(a.state, ActionState::Done | ActionState::Failed));
    if settled {
        store.set_state(&row.id, MonitorState::Completed, now)?;
    }
    Ok(())
}

/// Rule 2's stop condition: the remediation budget is spent, so the monitor
/// needs a human (`mur monitor retry`). A plain `UPDATE`/`INSERT OR IGNORE`
/// underneath, so calling this on an already-`Exhausted` row is harmless —
/// both the pre-attempt defence-in-depth check and the post-attempt cap
/// check in `attempt_action` can reach here. `dedup: true` mirrors the
/// scheduler's own hard-deadline `exhausted` event: at most once per
/// (monitor, cycle).
fn exhaust(store: &MonitorStore, row: &MonitorRow, now: DateTime<Utc>) -> Result<()> {
    store.set_state(&row.id, MonitorState::Exhausted, now)?;
    store.append_event(
        &row.id,
        &row.cycle_id,
        "exhausted",
        serde_json::json!({ "reason": "remediation attempts exhausted" }),
        true,
        now,
    )?;
    Ok(())
}

/// Required addition beyond the brief (task-5 prompt correction): the
/// notifier's `NOTIFIABLE` list already carries `approval_required`, added
/// by a prior task, but nothing had ever appended one. `dedup: true` — once
/// per (monitor, cycle) is enough; a human does not need one banner per
/// blocked action.
fn record_approval_required(
    store: &MonitorStore,
    row: &MonitorRow,
    now: DateTime<Utc>,
) -> Result<()> {
    store.append_event(
        &row.id,
        &row.cycle_id,
        "approval_required",
        serde_json::json!({}),
        true,
        now,
    )?;
    Ok(())
}

/// Same correction, other half: `remediation_failed` for a gated
/// (above-`Read`-tier) action that passed the gate and then did not
/// remediate — its executor returned `Err`, or this build has no executor
/// for that verb at all. A `Read`-tier executor failure (see
/// `a_failing_executor_does_not_fail_the_tick`) is ordinary polling noise,
/// not a remedy gone wrong, so this is never called for those. `reason` is
/// payload only: `insert_event` derives the dedup key from `kind`, so
/// `dedup: true` still means one event per (monitor, cycle).
fn record_remediation_failed(
    store: &MonitorStore,
    row: &MonitorRow,
    action_type: &str,
    reason: &str,
    now: DateTime<Utc>,
) -> Result<()> {
    store.append_event(
        &row.id,
        &row.cycle_id,
        "remediation_failed",
        serde_json::json!({ "action": action_type, "reason": reason }),
        true,
        now,
    )?;
    Ok(())
}

/// Run (or block, or fail) one already-claimed action. Every branch ends by
/// writing exactly one terminal-or-parked state for `key` (`finish_action`
/// or `block_action`) except the Rule 2 pre-check, which exits before
/// touching `key` at all — that row is left `Claimed` for a human to
/// unblock the monitor via `mur monitor retry` before it can be reached
/// again.
#[allow(clippy::too_many_arguments)]
fn attempt_action(
    store: &MonitorStore,
    registry: &AdapterRegistry,
    handle: &tokio::runtime::Handle,
    mur_home: &Path,
    row: &MonitorRow,
    key: &str,
    action_type: &str,
    action_index: usize,
    params: &serde_json::Map<String, serde_json::Value>,
    now: DateTime<Utc>,
    rep: &mut ActionReport,
) -> Result<()> {
    let tier = risk::classify(action_type);
    let cap = row.spec.policy.max_remediation_attempts;
    let gated = tier > RiskTier::Read;

    // Rule 2, defence-in-depth: never attempt a remediation past the
    // budget. The running total is read from the STORE, never from `row` —
    // `row` is the snapshot Phase 1 took from `store.list()` BEFORE the
    // loop, so every sibling in the same list would see the count as it was
    // before any of them incremented it, and a list longer than the cap
    // would overshoot by its tail (spec §行動執行器 line 412: no 4th
    // automatic remedy). Phase 2 re-`get`s per row already; this makes
    // Phase 1 agree with it.
    if gated {
        let attempts = store
            .get(&row.id)?
            .map_or(row.remediation_attempts, |fresh| fresh.remediation_attempts);
        if attempts >= cap {
            exhaust(store, row, now)?;
            rep.exhausted += 1;
            return Ok(());
        }
    }

    let decision = decide(
        handle,
        mur_home,
        row,
        action_type,
        action_index,
        params,
        now,
    )?;

    // The new remediation total, set ONLY by the arm that actually
    // attempted a remedy. `record_remediation_attempt` returns it so the
    // post-check below needs no extra `get` — which is what it was written
    // to do (`store/mod.rs`).
    let mut attempts_after: Option<u32> = None;

    if decision.deferred {
        store.block_action(key, &decision.action_hash)?;
        rep.blocked += 1;
        store.set_state(&row.id, MonitorState::AwaitingApproval, now)?;
        record_approval_required(store, row, now)?;
    } else if !decision.allow {
        store.finish_action(key, ActionState::Failed, &decision.reason)?;
        rep.failed += 1;
    } else {
        // Rule 1: re-verify the pin immediately before executing, from the
        // current inputs, fail-closed on drift (the executor re-verifies
        // the hash at the execute boundary — spec).
        let expected = expected_hash(row, action_type, action_index, params);
        if expected != decision.action_hash {
            store.finish_action(key, ActionState::Failed, "action changed after approval")?;
            rep.failed += 1;
        } else {
            // The execution boundary, and the ONLY place a remediation
            // attempt is counted (spec §行動執行器 rule 2). A deferral does
            // not count: parking a `HitlRequest` because no human has
            // answered is waiting, not remediating, and counting it
            // exhausted a gated monitor after three 15-second ticks — the
            // exact inversion of CLAUDE.md's "unattended approvals defer,
            // they do not time out … an approval given hours later releases
            // the gate". Being refused does not count either (explicit
            // denial, or the pin no longer matching): nothing was
            // attempted, and both arms are terminal, so neither can loop.
            if gated {
                attempts_after = Some(store.record_remediation_attempt(&row.id)?);
            }
            match executor_for(action_type) {
                None => {
                    // `executor_for` covers exactly the three `Read`-tier
                    // verbs, so every GATED verb — `rerun`,
                    // `start_downstream`, `apply_known_remedy` — lands here:
                    // deliberately out of scope for this slice (no
                    // credential scope for an external write, no remedy
                    // catalogue). Say so instead of pretending. Approval
                    // followed by silence is the precise failure this
                    // feature exists to remove, so the reason names the verb
                    // and the notifiable `remediation_failed` event tells
                    // the human their approval led to no remedy.
                    let reason =
                        format!("this build has no executor for `{action_type}`; it did not run");
                    store.finish_action(key, ActionState::Failed, &reason)?;
                    rep.failed += 1;
                    if gated {
                        record_remediation_failed(store, row, action_type, &reason, now)?;
                    }
                }
                Some(exec) => {
                    let ctx = ActionCtx {
                        store,
                        row,
                        now,
                        registry,
                    };
                    match exec.run(&ctx, params) {
                        Ok(summary) => {
                            store.finish_action(key, ActionState::Done, &summary)?;
                            rep.executed += 1;
                        }
                        Err(reason) => {
                            store.finish_action(key, ActionState::Failed, &reason)?;
                            rep.failed += 1;
                            if gated {
                                record_remediation_failed(store, row, action_type, &reason, now)?;
                            }
                        }
                    }
                }
            }
        }
    }

    if let Some(total) = attempts_after
        && total >= cap
        && let Some(fresh) = store.get(&row.id)?
        && fresh.state != MonitorState::Exhausted
    {
        exhaust(store, &fresh, now)?;
        rep.exhausted += 1;
    }

    Ok(())
}

/// Drain monitors the scheduler parked in `ActionPending` — the piece that
/// makes the actions built in Tasks 1-4 actually run (spec §行動執行器).
/// Never fails the tick and never creates a store, same rule as
/// `drain_notifications`: an absent database means a user who has never
/// run `mur monitor add`, and draining must not be the thing that
/// materialises it.
///
/// Two phases, because `ActionPending` is not the only state with owed
/// work:
/// - Phase 1 walks monitors currently `ActionPending`, resolving their
///   action list fresh and claiming indices starting at 0.
/// - Phase 2 walks `pending_actions` (every `Claimed`/`Blocked` row, any
///   monitor) to catch a crash between claim and finish, and to retry an
///   `AwaitingApproval` monitor — Phase 1's `ActionPending` filter no
///   longer sees it once it has been blocked once.
///
/// Bounded by `DRAIN_MAX_ACTIONS_PER_TICK` across both phases combined
/// (Rule 6).
pub fn drain_actions(
    mur_home: &Path,
    handle: &tokio::runtime::Handle,
    now: DateTime<Utc>,
) -> Result<ActionReport> {
    let Some(store) = MonitorStore::open_existing(mur_home)? else {
        return Ok(ActionReport::default());
    };
    let registry = super::registry(mur_home);
    let mut rep = ActionReport::default();
    let mut budget = DRAIN_MAX_ACTIONS_PER_TICK;
    // Keys phase 1 has already attempted this tick. Phase 2 reads
    // `pending_actions`, which returns every `Claimed`/`Blocked` row — and a
    // row phase 1 just created and blocked is exactly that. Without this set
    // a freshly gated action is attempted twice in one pass: `attempt` is
    // bumped twice, so the retry budget burns at double rate and the report
    // double-counts it.
    let mut attempted_this_tick: std::collections::HashSet<String> =
        std::collections::HashSet::new();

    'monitors: for row in store.list(&ListFilter {
        state: Some(MonitorState::ActionPending),
        include_completed: true,
    })? {
        let list = actions_for_outcome(&row);
        for (index, action) in list.iter().enumerate() {
            if budget == 0 {
                break 'monitors;
            }
            let key = action_key(&row.id, &row.cycle_id, row.fence, &action.r#type, index);
            if !store.claim_action(
                &key,
                &row.id,
                &row.cycle_id,
                risk::classify(&action.r#type),
                now,
            )? {
                // Already has a row from a prior tick — Phase 2 owns it now.
                continue;
            }
            budget -= 1;
            attempt_action(
                &store,
                &registry,
                handle,
                mur_home,
                &row,
                &key,
                &action.r#type,
                index,
                &action.params,
                now,
                &mut rep,
            )?;
            attempted_this_tick.insert(key);
        }
        if let Some(fresh) = store.get(&row.id)? {
            maybe_complete_monitor(&store, &fresh, now)?;
        }
    }

    for action_row in store.pending_actions(now, DRAIN_MAX_ACTIONS_PER_TICK)? {
        if budget == 0 {
            break;
        }
        if attempted_this_tick.contains(&action_row.action_key) {
            // Phase 1 created and attempted this row moments ago.
            continue;
        }
        let Some(row) = store.get(&action_row.monitor_id)? else {
            continue;
        };
        if matches!(row.state, MonitorState::Completed | MonitorState::Exhausted) {
            // Settled since this row was claimed — nothing left to retry.
            continue;
        }
        let Some((verb, index)) = verb_and_index_from_key(&action_row.action_key) else {
            continue;
        };
        if action_row.cycle_id != row.cycle_id {
            // A different episode: the monitor settled a new cycle since
            // this row was claimed, so the list index would resolve against
            // is not the list the row came from.
            continue;
        }
        let Some(action) = actions_for_outcome(&row).get(index) else {
            continue;
        };
        if action.r#type != verb {
            // Same cycle, different list: the outcome flipped under this
            // parked row (`reschedule_monitor` returns it to `Sleeping`,
            // the re-poll settles `Succeeded`, `on_success` is resolved
            // instead). Running index N now and writing the result onto
            // THIS key would record that `verb` completed when it never
            // ran, and would run the verb at that slot twice in one tick —
            // once under its own key from Phase 1, once under this one.
            continue;
        }
        budget -= 1;
        attempt_action(
            &store,
            &registry,
            handle,
            mur_home,
            &row,
            &action_row.action_key,
            &action.r#type,
            index,
            &action.params,
            now,
            &mut rep,
        )?;
        if let Some(fresh) = store.get(&row.id)? {
            maybe_complete_monitor(&store, &fresh, now)?;
        }
    }

    Ok(rep)
}

#[cfg(test)]
mod tests {
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

    #[test]
    fn a_key_yields_both_its_verb_and_its_index() {
        // Phase 2's only handle on what a parked row was FOR: the index
        // alone resolves to whatever verb now sits at that slot.
        let key = action_key("mon-1", "cyc-2", 7, "rerun", 3);
        assert_eq!(verb_and_index_from_key(&key), Some(("rerun", 3)));
        assert_eq!(verb_and_index_from_key("nonsense"), None);
    }
}

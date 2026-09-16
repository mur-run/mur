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
// The key format's parser lives beside the format's writer
// (`mur_monitor::action`); re-exported so `cmd::monitor::show` and this
// module's Phase 2 keep one path to it.
pub(crate) use mur_monitor::action::verb_and_index_from_key;
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

/// Bounds how many BLOCKED (parked-awaiting-approval) rows Phase 2
/// re-checks each tick — separate from, and not decremented by,
/// `DRAIN_MAX_ACTIONS_PER_TICK` (M2, whole-branch review).
///
/// Re-checking a blocked row is a cheap gate lookup that almost always
/// defers again (waiting is not attempting — see `block_action`'s doc); it
/// is not "an action attempted" in rule 6's sense, so it must not compete
/// with fresh claims for the same ten-row window. Sharing one budget/limit
/// meant that past `DRAIN_MAX_ACTIONS_PER_TICK` simultaneously-parked
/// actions, the oldest ten occupied `pending_actions`'s window on every
/// tick forever and an approval written for the eleventh was never looked
/// at again — the same starvation class fix round 2 closed for drifted
/// rows, but blocked rows are permanent occupants by design, not a bug to
/// retire. A much larger, still-bounded cap (rather than none at all) keeps
/// the query and the tick itself from growing unbounded if the table were
/// ever driven pathologically large.
pub const DRAIN_MAX_BLOCKED_PER_TICK: usize = 500;

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
/// code; `mur_monitor::backoff::unknown_delay` already handles the unknown
/// case inside the scheduler itself. The `Unknown => &[]` arm below is this
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

/// Retires a Phase-2 row whose key names a verb/index the monitor's CURRENT
/// action list no longer agrees with (fix round 2, following on from
/// `verb_and_index_from_key`'s doc above): the outcome flipped out from
/// under a `Claimed`/`Blocked` row (there is no `mur monitor edit` to have
/// changed the spec itself), so the index this key recorded can never again
/// resolve to the verb it named. Left `Claimed`/`Blocked`, that row would
/// keep occupying one of the ten oldest-first `pending_actions` slots on
/// every future tick forever — starving real work behind a row that
/// structurally cannot make progress — so it is retired terminally
/// (`Failed`) instead of skipped again. `reason` names the drift for the
/// human reading `mur monitor show`, stating only what the key named and
/// what is at that index now — it must not assert a cause it cannot know
/// (L3, whole-branch review).
fn retire_drifted_action(
    store: &MonitorStore,
    row: &MonitorRow,
    key: &str,
    reason: &str,
    now: DateTime<Utc>,
    rep: &mut ActionReport,
) -> Result<()> {
    store.finish_action(key, ActionState::Failed, reason)?;
    rep.failed += 1;
    if let Some(fresh) = store.get(&row.id)? {
        maybe_complete_monitor(store, &fresh, now)?;
    }
    Ok(())
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
///
/// `reason` is redacted here (L4, whole-branch review), same chokepoint
/// `store_result` uses for `finish_action`'s `result` column: today only a
/// fixed, secret-free string reaches this call (`executor_for(action_type)
/// == None`), but the moment a gated verb gets a real executor, `reason`
/// becomes that executor's own `Err` text — untrusted the same way
/// `CollectLogs`'s evidence is — and `append_event` does no redaction of
/// its own. Defence in depth, not reliance on every future executor
/// remembering to redact its own errors.
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
        serde_json::json!({
            "action": action_type,
            "reason": mur_common::redact::redact_secrets(reason).into_owned(),
        }),
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

    // spec §錯誤處理: a failure in one unit is recorded and retried, never
    // propagated. `decide` was the one `?` in this subsystem that did
    // propagate, and the daemon only catches it at the TICK boundary — so a
    // persistent fault on ONE monitor's channel (an unreadable event file, a
    // signing-key failure in `append_as_writer`) aborted the drain at the
    // same monitor every tick and no monitor's actions ever ran again. The
    // row keeps its `Claimed`/`Blocked` state, so it is retried next tick
    // like any other owed work, and the reason is on the row for
    // `mur monitor show`.
    let decision = match decide(
        handle,
        mur_home,
        row,
        action_type,
        action_index,
        params,
        now,
    ) {
        Ok(d) => d,
        Err(error) => {
            tracing::warn!(
                monitor = %row.id,
                action = action_type,
                %error,
                "monitor: approval gate failed; this action is retried next tick"
            );
            store.record_action_error(key, &format!("approval gate failed: {error}"))?;
            return Ok(());
        }
    };

    // The new remediation total, set ONLY by the arm that actually
    // attempted a remedy. `record_remediation_attempt` returns it so the
    // post-check below needs no extra `get` — which is what it was written
    // to do (`store/mod.rs`).
    let mut attempts_after: Option<u32> = None;

    if decision.deferred {
        // The `hitl_id`, never the `action_hash`. `mur channel approve`
        // matches strictly on the id the gate minted, so storing the hash
        // here gives `show` nothing to print but a command the approve path
        // rejects — a monitor parked with no reachable way to release it.
        // `hitl_id` is always `Some` on the deferred branch; the fallback
        // keeps a future gate change from silently writing an empty string.
        let approval_id = decision
            .hitl_id
            .as_deref()
            .unwrap_or(decision.action_hash.as_str());
        store.block_action(key, approval_id)?;
        rep.blocked += 1;
        store.set_state(&row.id, MonitorState::AwaitingApproval, now)?;
        record_approval_required(store, row, now)?;
    } else if !decision.allow {
        store.finish_action(key, ActionState::Failed, &decision.reason)?;
        rep.failed += 1;
    } else {
        // Rule 1's pin re-check. This recomputes `expected_hash` from the
        // SAME `row`/`action_type`/`action_index`/`params` that `decide`
        // was just called with, in the same function, so the two values are
        // equal by construction — this branch cannot fail today, and is
        // untested for that reason. It is a cheap tripwire against a future
        // refactor that separates deciding from executing (e.g. re-reading
        // `params` from the store between the two, or moving the execute
        // call to a later tick) and forgets to re-pin when it does. The
        // real fail-closed guarantee is `gate::scan_prior`'s hash-keyed
        // lookup — an approval never carries over to a different action,
        // and drift is denied — verified by
        // `an_approval_does_not_carry_to_a_different_action` and
        // `drift_denies_fail_closed` in `gate.rs`'s tests.
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
            // Containment, same rule as the `decide` arm inside
            // `attempt_action`: whatever went wrong belongs to THIS monitor.
            // A `?` here would abort the whole drain mid-loop and, if the
            // fault is persistent, would do so at the same monitor on every
            // tick — leaving every other monitor's actions permanently
            // unrun. The row keeps its claimed state and is retried.
            if let Err(error) = attempt_action(
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
            ) {
                tracing::warn!(
                    monitor = %row.id,
                    action = %action.r#type,
                    %error,
                    "monitor: action failed to process; other monitors continue"
                );
            }
            attempted_this_tick.insert(key);
        }
        if let Some(fresh) = store.get(&row.id)? {
            maybe_complete_monitor(&store, &fresh, now)?;
        }
    }

    let mut blocked_budget = DRAIN_MAX_BLOCKED_PER_TICK;
    for action_row in
        store.pending_actions(now, DRAIN_MAX_ACTIONS_PER_TICK, DRAIN_MAX_BLOCKED_PER_TICK)?
    {
        let spend = if action_row.state == ActionState::Blocked {
            &mut blocked_budget
        } else {
            &mut budget
        };
        if *spend == 0 {
            // Not `break`: rows governed by the OTHER budget can appear
            // later in this concatenated vec (blocked rows are appended
            // after claimed rows — see `pending_actions`), and exhausting
            // this budget must not skip them (M2, whole-branch review).
            continue;
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
        let current_list = actions_for_outcome(&row);
        let Some(action) = current_list.get(index) else {
            // The index this key recorded no longer exists in the current
            // list at all — never recoverable (see `retire_drifted_action`).
            retire_drifted_action(
                &store,
                &row,
                &action_row.action_key,
                &format!(
                    "action key named verb `{verb}` at index {index}, but the current \
                     action list for this outcome now has only {} action(s) — this row \
                     can never resolve against it again",
                    current_list.len()
                ),
                now,
                &mut rep,
            )?;
            continue;
        };
        if action.r#type != verb {
            // Same cycle, different list: the outcome flipped under this
            // parked row, so `actions_for_outcome` now resolves the OTHER
            // list (a `Failed` monitor re-observed as `Succeeded` while a
            // row sat blocked — rare, since a settled monitor is not
            // claimable, but reachable through `mur monitor retry` and
            // through a lease recovered mid-cycle). Running index N now and
            // writing the result onto THIS key would record that `verb`
            // completed when it never ran, and would run the verb at that
            // slot twice in one tick — once under its own key from Phase 1,
            // once under this one.
            // Never recoverable either: index N in THIS cycle's list will
            // never again name `verb` (see `retire_drifted_action`).
            retire_drifted_action(
                &store,
                &row,
                &action_row.action_key,
                &format!(
                    "action key named verb `{verb}` at index {index}, but the current \
                     action list now has `{}` there — this row can never resolve \
                     against it again",
                    action.r#type
                ),
                now,
                &mut rep,
            )?;
            continue;
        }
        *spend -= 1;
        // Contained per row, for the same reason as Phase 1 above.
        if let Err(error) = attempt_action(
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
        ) {
            tracing::warn!(
                monitor = %row.id,
                action = %action.r#type,
                %error,
                "monitor: action failed to process; other monitors continue"
            );
        }
        if let Some(fresh) = store.get(&row.id)? {
            maybe_complete_monitor(&store, &fresh, now)?;
        }
    }

    Ok(rep)
}

#[cfg(test)]
mod tests;

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

mod attempt;
mod consult;
#[allow(unused_imports)]
use attempt::*;
#[allow(unused_imports)]
use consult::*;

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

/// Consecutive ticks the approval gate may fail for one action before that
/// action is retired. The asymmetry decides the number: retiring too early
/// marks `Failed` something that would have recovered, which costs a user
/// an action they asked for; retiring too late costs one of
/// `DRAIN_MAX_ACTIONS_PER_TICK` slots. Ten ticks is ~2.5 minutes of
/// unbroken failure — long enough to ride out a disk hiccup or a brief file
/// lock, short enough that a genuinely broken channel stops squatting.
pub const GATE_ERROR_MAX: u32 = 10;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ActionReport {
    pub executed: usize,
    pub blocked: usize,
    pub failed: usize,
    pub exhausted: usize,
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
/// Two budgets, not one: `DRAIN_MAX_ACTIONS_PER_TICK` for work that runs a
/// side effect, and `DRAIN_MAX_BLOCKED_PER_TICK` for re-checking rows already
/// parked on a human. They are separate because sharing one lets fresh claims
/// crowd out the re-checks, and a re-check that never happens is an approval
/// that never takes effect.
pub fn drain_actions(
    mur_home: &Path,
    handle: &tokio::runtime::Handle,
    now: DateTime<Utc>,
) -> Result<ActionReport> {
    drain_actions_with(mur_home, handle, now, &super::registry(mur_home))
}

/// The body of `drain_actions`, with the adapter registry supplied rather
/// than built from `mur_home`.
///
/// The seam exists for one reason: the only verb that is both gated and has
/// an executor is `rerun`, and its executor's success path goes through
/// `GithubActionsAdapter`, which would make a real HTTP request. Without
/// this parameter the drain's own happy path for a gated action — approval
/// → attempt counted → executor `Ok` → `maybe_complete_monitor` — is
/// unreachable from a test at any price, which is how F1 (a *successful*
/// gated remedy marking the monitor `Exhausted`) shipped unnoticed. Test
/// code passes a double; production has exactly one caller, above.
pub(crate) fn drain_actions_with(
    mur_home: &Path,
    handle: &tokio::runtime::Handle,
    now: DateTime<Utc>,
    registry: &AdapterRegistry,
) -> Result<ActionReport> {
    let Some(store) = MonitorStore::open_existing(mur_home)? else {
        return Ok(ActionReport::default());
    };
    // Step 3 before Phase 1: a cycle whose remedies all failed gets one
    // consultation, and is stamped either way so the settle guard can let it
    // go. Contained — a resolver fault never fails the drain.
    if let Err(error) = consult_failed_cycles(&store, mur_home, handle, now) {
        tracing::warn!(%error, "monitor: consultation pass failed; the drain continues");
    }
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

    let resolver_enabled =
        mur_common::config::Config::load_or_default(&mur_home.join("config.yaml"))
            .monitor_resolver
            .enabled;
    'monitors: for row in store.list(&ListFilter {
        state: Some(MonitorState::ActionPending),
        include_completed: true,
    })? {
        let list = actions_for_outcome(&row);
        // Mark the cycle as owed a consultation, before any of its actions
        // run. Doing it here rather than when they finish is what keeps a
        // disabled resolver free: with it off nothing is stamped, so the
        // settle guard never holds anything. `dedup: true` — once per
        // (monitor, cycle).
        if resolver_enabled && !list.is_empty() {
            store.append_event(
                &row.id,
                &row.cycle_id,
                RESOLVER_PENDING_EVENT,
                serde_json::json!({}),
                true,
                now,
            )?;
        }
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
                registry,
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
        // A resolver-proposed row carries its own action, because the
        // index in its key names no slot in the spec's list — resolving it
        // below would retire it as drifted on the first tick after it was
        // claimed. Handled before the list lookup for that reason, not as an
        // optimisation.
        if let Some(json) = action_row.proposed_action.as_deref() {
            let proposed: mur_monitor::spec::Action = match serde_json::from_str(json) {
                Ok(a) => a,
                Err(e) => {
                    // Unreadable proposal: retire rather than guess. The row
                    // can never resolve — its action is the only record of
                    // what it was for, and re-consulting the model here
                    // would spend a second call on a cycle already budgeted
                    // for one.
                    retire_drifted_action(
                        &store,
                        &row,
                        &action_row.action_key,
                        &format!("stored proposal could not be read back: {e}"),
                        now,
                        &mut rep,
                    )?;
                    continue;
                }
            };
            if proposed.r#type != verb {
                // The key and the stored action disagree. Nothing writes the
                // two separately, so this is corruption rather than drift —
                // still not something to resolve by picking one.
                retire_drifted_action(
                    &store,
                    &row,
                    &action_row.action_key,
                    &format!(
                        "action key named verb `{verb}` but the stored proposal says                          `{}`",
                        proposed.r#type
                    ),
                    now,
                    &mut rep,
                )?;
                continue;
            }
            *spend -= 1;
            if let Err(error) = attempt_action(
                &store,
                registry,
                handle,
                mur_home,
                &row,
                &action_row.action_key,
                &proposed.r#type,
                index,
                &proposed.params,
                now,
                &mut rep,
            ) {
                tracing::warn!(
                    monitor = %row.id,
                    action = %verb,
                    %error,
                    "monitor: proposed action failed to process; other monitors continue"
                );
            }
            // Mirrors the tail of the list-resolved path below: `continue`
            // would otherwise skip it, leaving a monitor whose last action
            // just settled sitting in `ActionPending` until the next tick.
            if let Some(fresh) = store.get(&row.id)? {
                maybe_complete_monitor(&store, &fresh, now)?;
            }
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
            registry,
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

//! Per-action attempt and settle helpers, split out of
//! `drain_actions/mod.rs` as pure code movement (CLAUDE.md rule 5).

use super::*;

pub(super) fn actions_for_outcome(row: &MonitorRow) -> &[Action] {
    match row.outcome {
        Outcome::Succeeded => &row.spec.actions.on_success,
        Outcome::Failed | Outcome::Cancelled => &row.spec.actions.on_failure,
        // `Abandoned` (#1622): the work's fate is unknown — same as `Unknown`.
        Outcome::Pending | Outcome::Unknown | Outcome::Abandoned => &[],
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
pub(super) fn retire_drifted_action(
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
pub(super) fn maybe_complete_monitor(
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
    if !settled {
        return Ok(());
    }
    // Hold a failed cycle open until the consultation pass has stamped it.
    //
    // Spec §混合處置策略 step 3 reaches the resolver only when the rules did
    // not settle the failure, and "every action in the list failed" is
    // exactly that — but without this guard the monitor would be
    // `Completed` in the same tick its last action failed, so the pass would
    // never see a candidate.
    //
    // This reads one fact and takes no new argument on purpose. Whether the
    // resolver is enabled is the caller's business: the pass stamps every
    // candidate cycle either way (see `RESOLVER_CONSULTED_EVENT`), so a
    // disabled resolver costs one event and completion proceeds on the next
    // tick rather than hanging.
    if cycle_rows
        .iter()
        .any(|a| matches!(a.state, ActionState::Failed))
        && cycle_event(store, row, RESOLVER_PENDING_EVENT)?
        && !cycle_event(store, row, RESOLVER_CONSULTED_EVENT)?
    {
        return Ok(());
    }
    store.set_state(&row.id, MonitorState::Completed, now)?;
    Ok(())
}

/// Rule 2's stop condition: the remediation budget is spent, so the monitor
/// needs a human (`mur monitor retry`). A plain `UPDATE`/`INSERT OR IGNORE`
/// underneath, so calling this on an already-`Exhausted` row is harmless —
/// both the pre-attempt defence-in-depth check and the post-attempt cap
/// check in `attempt_action` can reach here. `dedup: true` mirrors the
/// scheduler's own hard-deadline `exhausted` event: at most once per
/// (monitor, cycle).
pub(super) fn exhaust(store: &MonitorStore, row: &MonitorRow, now: DateTime<Utc>) -> Result<()> {
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
pub(super) fn record_approval_required(
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
/// `store_result` uses for `finish_action`'s `result` column. When this was
/// written only a fixed, secret-free string could reach it
/// (`executor_for(action_type) == None`); `rerun` has an executor now, so
/// `reason` is also that executor's own `Err` text — untrusted the same way
/// `CollectLogs`'s evidence is, and carrying whatever the GitHub adapter
/// put in it — while `append_event` does no redaction of its own. Defence
/// in depth, not reliance on every future executor remembering to redact
/// its own errors.
pub(super) fn record_remediation_failed(
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
pub(super) fn attempt_action(
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
        Ok(d) => {
            // The gate answered, so any earlier failures were not
            // consecutive. A no-op when the count is already zero — see the
            // `!= 0` guard in `clear_action_errors` — so this costs nothing
            // on the ordinary path.
            store.clear_action_errors(key)?;
            d
        }
        Err(error) => {
            let reason = format!("approval gate failed: {error}");
            let consecutive = store.record_action_error(key, &reason)?;
            if consecutive < GATE_ERROR_MAX {
                tracing::warn!(
                    monitor = %row.id,
                    action = action_type,
                    %error,
                    consecutive,
                    "monitor: approval gate failed; this action is retried next tick"
                );
                return Ok(());
            }
            // Retried `GATE_ERROR_MAX` ticks running and the gate has not
            // once answered. Left `Claimed` it would retry forever while
            // holding one of the slots `pending_actions` returns, so a
            // single unreadable channel would quietly stop other monitors'
            // actions too. Settle it, say why, and — if it was gated — tell
            // the human, because an approval they gave must not vanish in
            // silence.
            let gave_up = format!(
                "{reason} — gave up after {consecutive} consecutive failures; \
                 fix the channel and `mur monitor retry` to try again"
            );
            tracing::error!(
                monitor = %row.id,
                action = action_type,
                consecutive,
                "monitor: approval gate has failed every tick; retiring this action"
            );
            store.finish_action(key, ActionState::Failed, &gave_up)?;
            rep.failed += 1;
            if gated {
                record_remediation_failed(store, row, action_type, &gave_up, now)?;
            }
            return Ok(());
        }
    };

    // The new remediation total, carried to the post-attempt cap check
    // ONLY by an arm that attempted a remedy and did not get one — the
    // executor's `Err`, or no executor for the verb at all.
    // `record_remediation_attempt` returns it so the post-check below needs
    // no extra `get` — which is what it was written to do (`store/mod.rs`).
    //
    // A gated action that DID remediate deliberately leaves this `None`.
    // The count is of attempts (rule 2 counts the deferral-free attempt,
    // not the failure), but `Exhausted` is a verdict about giving up, and
    // it is terminal: `maybe_complete_monitor` early-returns on any state
    // that is not `ActionPending`/`AwaitingApproval`, so an `Exhausted`
    // written here can never be corrected to `Completed`. With
    // `max_remediation_attempts: 1` and `on_failure: [rerun]` that turned a
    // rerun MUR successfully dispatched into an `exhausted` event and a
    // desktop notification saying MUR gave up, pointing the user at
    // `mur monitor retry`. Spec §混合處置策略 rule 6 is about stopping
    // FURTHER remediation; when nothing further is owed and the last remedy
    // worked, `Completed` is the honest terminal.
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
            let attempted = if gated {
                Some(store.record_remediation_attempt(&row.id)?)
            } else {
                None
            };
            match executor_for(action_type) {
                None => {
                    // `executor_for` covers `notify`, `collect_logs` and
                    // `rerun`. Everything else lands here: the gated verbs
                    // still without one (`start_downstream`,
                    // `apply_known_remedy`) and `reschedule_monitor`, which
                    // is `Read`-tier but deliberately has no executor —
                    // returning a settled monitor to a claimable state
                    // un-freezes its fence and re-runs the whole list:
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
                    attempts_after = attempted;
                    if gated {
                        record_remediation_failed(store, row, action_type, &reason, now)?;
                    }
                }
                Some(exec) => {
                    let ctx = ActionCtx {
                        events: crate::monitor::actions::EventWriter::new(store, row, now),
                        row,
                        now,
                        registry,
                    };
                    match exec.run(&ctx, params) {
                        Ok(summary) => {
                            // No cap check on this arm: the remedy worked,
                            // so the monitor falls through to
                            // `maybe_complete_monitor` and settles
                            // `Completed`. See `attempts_after` above.
                            store.finish_action(key, ActionState::Done, &summary)?;
                            rep.executed += 1;
                        }
                        Err(reason) => {
                            store.finish_action(key, ActionState::Failed, &reason)?;
                            rep.failed += 1;
                            attempts_after = attempted;
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

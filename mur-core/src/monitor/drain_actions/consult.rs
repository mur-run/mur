//! Resolver consultation pass (spec §混合處置策略 step 3), split out of
//! `drain_actions/mod.rs` as pure code movement (CLAUDE.md rule 5).

use super::*;

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
/// Spec §混合處置策略 step 3, the caller side: ask a model what to do about
/// a failure the spec's own action list could not settle.
///
/// Runs before Phase 1 and keeps `maybe_complete_monitor`'s signature out of
/// it: everything config- and runtime-shaped lives here, where `mur_home`
/// and the tokio handle already are, and the only thing crossing to the
/// settle guard is a stamped event.
///
/// Candidate = a monitor whose current cycle has actions, all of them
/// settled, at least one `Failed`, and no stamp yet. That is "已知补救
/// failed" precisely. The other half of step 3 — a terminal failure whose
/// `on_failure` list was EMPTY — is not handled here: such a monitor never
/// enters `ActionPending`, so it needs a different candidate query, and
/// guessing at one would be guessing at which state the scheduler left it in.
pub(super) fn consult_failed_cycles(
    store: &MonitorStore,
    mur_home: &Path,
    handle: &tokio::runtime::Handle,
    now: DateTime<Utc>,
) -> Result<()> {
    let cfg = mur_common::config::Config::load_or_default(&mur_home.join("config.yaml"));
    let enabled = cfg.monitor_resolver.enabled;

    for row in store.list(&ListFilter {
        state: Some(MonitorState::ActionPending),
        include_completed: false,
    })? {
        let cycle_rows: Vec<_> = store
            .actions_for(&row.id)?
            .into_iter()
            .filter(|a| a.cycle_id == row.cycle_id)
            .collect();
        if cycle_rows.is_empty() {
            continue;
        }
        let all_settled = cycle_rows
            .iter()
            .all(|a| matches!(a.state, ActionState::Done | ActionState::Failed));
        let any_failed = cycle_rows
            .iter()
            .any(|a| matches!(a.state, ActionState::Failed));
        if !(all_settled && any_failed) {
            continue;
        }

        // Only cycles opened while the resolver was enabled are owed a
        // consultation, and the guard is holding exactly those.
        if !cycle_event(store, &row, RESOLVER_PENDING_EVENT)? {
            continue;
        }
        // Stamp BEFORE calling out, so a crash mid-call cannot buy a second
        // call next tick — and so the guard releases the monitor either way.
        if !store.append_event(
            &row.id,
            &row.cycle_id,
            RESOLVER_CONSULTED_EVENT,
            serde_json::json!({}),
            true,
            now,
        )? {
            continue;
        }

        // Containment, the same rule as everywhere else in this file: a
        // fault belongs to THIS monitor. A `?` here would abort the drain
        // and, if the fault is persistent, do it at the same monitor every
        // tick — except the stamp is already written, so completion still
        // proceeds. Failing to consult is never failing the monitor.
        match consult_one(store, mur_home, handle, &row, &cycle_rows, &cfg, now) {
            // No reopen on this path: the monitor is already `ActionPending`,
            // held there by the settle guard.
            Ok(Some(proposal)) => {
                apply_proposal(store, &row, &proposal, now)?;
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(
                    monitor = %row.id,
                    %error,
                    "monitor: resolver consultation failed; the monitor settles without it"
                );
            }
        }
    }

    // The other half of step 3: "規則未涵蓋" — a terminal failure whose
    // `on_failure` list was empty. The scheduler completes such a monitor in
    // the same update that records the observation (`mur-monitor`'s
    // `scheduler.rs`: an empty list means `Completed`, a non-empty one means
    // `ActionPending`), so it never passes through the state the loop above
    // watches and the settle guard never sees it.
    //
    // Reopening it to `ActionPending` is safe, and this is the whole reason
    // it is allowed: `is_claimable` is `Active | Sleeping` only, so
    // `ActionPending` does NOT unfreeze the fence. That is precisely what
    // `reschedule_monitor` got wrong — it wrote `Sleeping`, which is
    // claimable, and re-ran the entire action list on every poll.
    //
    // No `resolver_pending` marker here: nothing is being held open, so
    // there is nothing for the settle guard to release. The gate is live
    // config, and it is read before anything is written — with the resolver
    // off this loop touches no rows at all.
    if !enabled {
        return Ok(());
    }
    for row in store.list(&ListFilter {
        state: Some(MonitorState::Completed),
        include_completed: true,
    })? {
        if !matches!(row.outcome, Outcome::Failed | Outcome::Cancelled) {
            continue;
        }
        if !actions_for_outcome(&row).is_empty() {
            // It had a rule; the loop above owns it.
            continue;
        }
        if !store.append_event(
            &row.id,
            &row.cycle_id,
            RESOLVER_CONSULTED_EVENT,
            serde_json::json!({ "trigger": "empty_action_list" }),
            true,
            now,
        )? {
            continue;
        }
        match consult_one(store, mur_home, handle, &row, &[], &cfg, now) {
            Ok(Some(proposal)) => {
                apply_proposal_reopening(store, &row, &proposal, now)?;
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(
                    monitor = %row.id,
                    %error,
                    "monitor: resolver consultation failed; the monitor stays completed"
                );
            }
        }
    }
    Ok(())
}

/// One consultation, already stamped and known to be enabled. Returns the
/// proposal WITHOUT applying it: the two callers apply it differently — the
/// empty-list one has to reopen a `Completed` monitor as well — and keeping
/// the split here is also what gives the reopen a test seam, since this
/// function builds its own model adapter and cannot be driven from a test.
pub(super) fn consult_one(
    store: &MonitorStore,
    mur_home: &Path,
    handle: &tokio::runtime::Handle,
    row: &MonitorRow,
    cycle_rows: &[mur_monitor::store::ActionRow],
    cfg: &mur_common::config::Config,
    now: DateTime<Utc>,
) -> Result<Option<crate::monitor::resolver::Proposal>> {
    use crate::monitor::resolver;

    let attempted: Vec<String> = cycle_rows
        .iter()
        .filter_map(|a| {
            let (verb, _) = verb_and_index_from_key(&a.action_key)?;
            Some(format!("{verb}: {}", a.state.as_str()))
        })
        .collect();
    // Whatever a `collect_logs` action managed to fetch is the only log text
    // this process has; it is already redacted and truncated by the store,
    // and the prompt builder redacts again rather than trusting that.
    let log_tail = cycle_rows
        .iter()
        .find(|a| a.action_key.contains("collect_logs"))
        .and_then(|a| a.result.clone());
    let ctx = resolver::prompt::Context {
        source_type: row.spec.source.r#type.as_str().to_string(),
        outcome: format!("{:?}", row.outcome).to_lowercase(),
        error: store
            .observations(&row.id, 1)?
            .first()
            .and_then(|o| o.adapter_error.clone()),
        attempted,
        log_tail,
    };

    let adapter = crate::conversations::backend::adapter::build_chat_adapter(
        mur_home,
        cfg.monitor_resolver.model.as_deref(),
        "monitor_resolver",
    )?;
    let proposal = match handle.block_on(resolver::ask(&adapter, &ctx)) {
        Ok(p) => p,
        Err(e) => {
            // Recorded, not acted on. `resolver_declined` is deliberately
            // not in `NOTIFIABLE`: the monitor's own terminal event already
            // told the human what happened to their work.
            store.append_event(
                &row.id,
                &row.cycle_id,
                "resolver_declined",
                serde_json::json!({ "reason": e.to_string() }),
                true,
                now,
            )?;
            return Ok(None);
        }
    };

    Ok(Some(proposal))
}

/// Claim a proposal and record it. `Ok(true)` only for the call that created
/// the row — same `INSERT OR IGNORE` contract as every other action, so a
/// restart does not re-run the side effect.
pub(super) fn apply_proposal(
    store: &MonitorStore,
    row: &MonitorRow,
    proposal: &crate::monitor::resolver::Proposal,
    now: DateTime<Utc>,
) -> Result<bool> {
    // Index one past the spec list, so the key cannot collide with a
    // spec-produced row. Phase 2 never resolves this index against the list
    // anyway — it sees `proposed_action` first — but the key must still be
    // unique per cycle.
    let index = actions_for_outcome(row).len();
    let key = action_key(
        &row.id,
        &row.cycle_id,
        row.fence,
        &proposal.action.r#type,
        index,
    );
    let json = serde_json::to_string(&proposal.action)
        .map_err(|e| anyhow::anyhow!("serialize proposed action: {e}"))?;
    if !store.claim_proposed_action(
        &key,
        &row.id,
        &row.cycle_id,
        risk::classify(&proposal.action.r#type),
        &json,
        now,
    )? {
        return Ok(false);
    }
    store.append_event(
        &row.id,
        &row.cycle_id,
        "resolver_proposed",
        serde_json::json!({
            "action_type": proposal.action.r#type,
            "reason": proposal.reason,
        }),
        true,
        now,
    )?;
    Ok(true)
}

/// The empty-list path's tail: claim, then reopen the monitor so the action
/// can actually run.
///
/// The monitor is `Completed` — the scheduler settles an empty-action-list
/// terminal outright — and Phase 2 skips `Completed` rows, so without the
/// reopen the proposal would sit claimed and unrunnable forever.
///
/// Reopening to `ActionPending` is safe for one reason and it is worth
/// stating where the code does it: `is_claimable` is `Active | Sleeping`
/// only, so this does NOT hand the monitor back to the scheduler.
/// `reschedule_monitor` wrote `Sleeping` — which is claimable — and re-ran
/// the whole action list on every poll. `maybe_complete_monitor` settles it
/// again once the action finishes.
pub(crate) fn apply_proposal_reopening(
    store: &MonitorStore,
    row: &MonitorRow,
    proposal: &crate::monitor::resolver::Proposal,
    now: DateTime<Utc>,
) -> Result<bool> {
    let claimed = apply_proposal(store, row, proposal, now)?;
    if claimed {
        store.set_state(&row.id, MonitorState::ActionPending, now)?;
    }
    Ok(claimed)
}

/// Stamped once per (monitor, cycle) by the consultation pass — including
/// when the resolver is switched off, which is the load-bearing half.
///
/// The settle guard below holds a failed cycle open until this exists. If
/// the pass only stamped after a real consultation, a monitor on a machine
/// with the resolver disabled would never complete: the guard cannot read
/// the config (it has no business doing file I/O on a path that runs every
/// ten seconds), so "disabled" has to be a fact in the log rather than an
/// inference. The payload says which it was, so the reason a monitor
/// completed is readable afterwards.
pub(crate) const RESOLVER_CONSULTED_EVENT: &str = "resolver_consulted";

/// Stamped when a cycle's actions are first claimed, and ONLY while the
/// resolver is enabled. It is what lets the settle guard answer "is the
/// resolver on?" without reading the config: a cycle carries the answer
/// that was true when it opened.
///
/// That is also the more correct reading. A config edit halfway through a
/// cycle should not retroactively change whether that cycle was owed a
/// consultation, and — the reason this exists rather than a live config
/// read — with the resolver off nothing is stamped, so nothing is held and
/// a disabled feature costs a user nothing.
pub(crate) const RESOLVER_PENDING_EVENT: &str = "resolver_pending";

/// Whether the consultation pass has already stamped this monitor's current
/// cycle. Read from the event log rather than a new column: the stamp is
/// written with `dedup: true`, so the event IS the once-per-cycle bound and
/// the crash guard, and a column beside it would be a second thing to keep
/// in step.
pub(super) fn cycle_event(store: &MonitorStore, row: &MonitorRow, kind: &str) -> Result<bool> {
    Ok(store
        .events(&row.id)?
        .iter()
        .any(|e| e.kind == kind && e.cycle_id == row.cycle_id))
}

//! The check loop (spec §排程, §daemon 恢復). `plan_cycle` is the entire
//! state machine as a pure function of (row, observation, now) — every rule
//! about backoff, deadlines, terminal settlement and health lives there and
//! is table-testable. `tick` is the thin I/O wrapper the daemon calls.

use std::time::{Duration, Instant};

use anyhow::Result;
use chrono::{DateTime, Utc};

use crate::adapter::{AdapterRegistry, Observation};
use crate::backoff::{
    RETAIN_INTERVAL, UNHEALTHY_AFTER_UNKNOWN, clamp_recommended, pending_delay, seed,
    unknown_delay, with_jitter,
};
use crate::deadline;
use crate::state::{MonitorState, Outcome};
use crate::store::{CycleUpdate, Event, ListFilter, MonitorRow, MonitorStore};

/// Longer than any single `observe` may take (GitHub client timeout is 30 s,
/// the others are local reads). A batch of `max_claims` claims is NOT
/// covered by one lease each staying alive for the whole batch — `tick`
/// beats each monitor's own lease immediately before its `observe` call, so
/// this only ever has to outlast one observe, not the sum of several.
pub const DEFAULT_LEASE: Duration = Duration::from_secs(120);

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TickReport {
    pub claimed: usize,
    pub observed: usize,
    pub completed: usize,
    pub action_pending: usize,
    pub exhausted: usize,
    pub unknown: usize,
    pub stale_fence: usize,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RecoveryReport {
    pub recovered_leases: Vec<String>,
    pub overdue: usize,
}

fn ev(kind: &'static str, payload: serde_json::Value) -> Event {
    Event {
        kind,
        payload,
        dedup: true,
        dedup_key: None,
    }
}

/// Same as `ev`, but dedups on `dedup_key` instead of the bare `kind` — for
/// an event that can legitimately recur within one (non-rotating) cycle.
fn ev_keyed(kind: &'static str, dedup_key: String, payload: serde_json::Value) -> Event {
    Event {
        kind,
        payload,
        dedup: true,
        dedup_key: Some(dedup_key),
    }
}

fn plus(now: DateTime<Utc>, d: Duration) -> DateTime<Utc> {
    now + chrono::Duration::from_std(d).unwrap_or(chrono::Duration::MAX)
}

/// The entire scheduling state machine, as a pure function of
/// `(row, observation, now)`. No I/O, no clock read — every rule about
/// backoff, deadlines, terminal settlement and health lives here so all ten
/// tests in this module can address it directly.
pub fn plan_cycle(row: &MonitorRow, obs: Observation, now: DateTime<Utc>) -> CycleUpdate {
    let policy = &row.spec.policy;
    let mut events = Vec::new();
    let mut u = CycleUpdate {
        outcome: obs.outcome,
        observation: obs.clone(),
        observed_at: now,
        new_state: MonitorState::Sleeping,
        next_check_at: now,
        pending_attempts: row.pending_attempts,
        unknown_streak: row.unknown_streak,
        last_progress_at: row.last_progress_at,
        progress_token: row.progress_token.clone(),
        stalled_since: row.stalled_since,
        soft_notified: row.soft_notified,
        hard_reached: row.hard_reached,
        finish_cycle: false,
        events: Vec::new(),
    };

    if obs.outcome.is_terminal() {
        // Settlement. With no actions to run the monitor is done; with some,
        // it parks for the executor (plan-2). Either way it is never claimed
        // again (`MonitorState::is_claimable`), which is what makes the
        // terminal side-effect fire exactly once — requirement 3.
        u.unknown_streak = 0;
        let actions = match obs.outcome {
            Outcome::Succeeded => &row.spec.actions.on_success,
            _ => &row.spec.actions.on_failure,
        };
        u.new_state = if actions.is_empty() {
            MonitorState::Completed
        } else {
            MonitorState::ActionPending
        };
        u.finish_cycle = true;
        events.push(ev(
            "terminal",
            serde_json::json!({ "outcome": obs.outcome.as_str(), "evidence": obs.evidence }),
        ));
        u.events = events;
        return u;
    }

    let (progress_at, token) = deadline::advance_progress(
        row.last_progress_at,
        row.progress_token.as_deref(),
        obs.progress_token.as_deref(),
        now,
    );
    u.last_progress_at = progress_at;
    u.progress_token = token;
    let v = deadline::evaluate(
        policy,
        row.work_started_at,
        progress_at,
        row.stalled_since,
        row.soft_notified,
        row.hard_reached,
        now,
    );
    u.stalled_since = v.stalled_since;
    u.soft_notified = v.soft_notified;
    u.hard_reached = v.hard_reached;
    // dedup is per (monitor, cycle, kind): a second stall inside the same
    // cycle after a recovery is not re-announced. Acceptable for MVP; a
    // per-stall dedup key is the upgrade if that proves noisy in practice.
    if v.stalled_newly {
        events.push(ev(
            "stalled",
            serde_json::json!({ "since": v.stalled_since }),
        ));
    }
    if v.recovered {
        events.push(ev("stalled_recovered", serde_json::json!({ "at": now })));
    }
    if v.soft_newly {
        events.push(ev("soft_deadline", serde_json::json!({ "at": now })));
    }
    if v.hard_newly {
        events.push(ev("hard_deadline", serde_json::json!({ "at": now })));
    }

    // requirement 4: `unknown` gets its own (shorter) backoff table and never
    // settles the monitor; requirement 6: the seed uses the NEW attempt
    // count (post-increment) while the delay TABLE lookup uses the OLD
    // attempt/streak — matching the deterministic-jitter test formulas.
    let (base, attempt_seed) = if obs.outcome == Outcome::Unknown {
        u.unknown_streak = row.unknown_streak + 1;
        if u.unknown_streak == UNHEALTHY_AFTER_UNKNOWN {
            // `cycle_id` never rotates for a monitor that keeps getting
            // reobserved, so a bare-`kind` dedup key (like `stalled`'s
            // above) would announce `monitor_unhealthy` at most once ever
            // per monitor — a recovery followed by a second run of bad luck
            // would never be seen. Keying on the streak episode instead
            // means a later, higher streak announces again while repeated
            // ticks at the *same* streak (there are none — it only equals
            // the ceiling on the one tick it's first reached) stay quiet.
            events.push(ev_keyed(
                "monitor_unhealthy",
                format!("monitor_unhealthy:{}", u.unknown_streak),
                serde_json::json!({ "streak": u.unknown_streak, "error": obs.adapter_error }),
            ));
        }
        (unknown_delay(u.unknown_streak - 1), u.unknown_streak)
    } else {
        u.unknown_streak = 0;
        u.pending_attempts = row.pending_attempts + 1;
        (pending_delay(row.pending_attempts), u.pending_attempts)
    };

    // requirement 5: the hard deadline is not a failure verdict. Past it, a
    // monitor whose policy retains monitoring keeps polling read-only at
    // `RETAIN_INTERVAL`; one that does not goes to `Exhausted`. Neither
    // marks the work failed — `obs.outcome` is untouched either way. This
    // guard used to also require `obs.outcome == Outcome::Pending`, which
    // silently excluded `Unknown` (the only other non-terminal outcome by
    // construction: `obs.outcome.is_terminal()` returns at the top of this
    // function, so everything reaching here is `Pending` or `Unknown` and
    // there is no third case for the dropped clause to have been guarding
    // against). With `retain: true` that left an `Unknown` past the hard
    // deadline polling at the `unknown` cap (5m) instead of dropping to
    // `RETAIN_INTERVAL` (2h); with `retain: false` it never reached
    // `Exhausted` at all and polled forever — exactly the "automatic work
    // stops after the hard deadline" guarantee the spec's MVP criterion 5
    // makes, broken for a source that has gone unreadable.
    let base = if v.hard_reached {
        if policy.retain_monitoring_after_hard_deadline {
            RETAIN_INTERVAL
        } else {
            u.new_state = MonitorState::Exhausted;
            events.push(ev(
                "exhausted",
                serde_json::json!({ "reason": "hard deadline, monitoring not retained" }),
            ));
            base
        }
    } else {
        base
    };

    // Clamp BEFORE jitter, not after: every base above comes from a table
    // (`pending_delay`/`unknown_delay`/`RETAIN_INTERVAL`) that is already
    // >= MIN_INTERVAL, so `clamp_recommended` is a no-op here and jitter is
    // free to move within its full +/-20% band, including below the floor
    // for `unknown_delay(0) == MIN_INTERVAL`. Clamping AFTER jitter would
    // re-floor exactly that low tail and silently disagree with the
    // deterministic `with_jitter(base, seed)` the tests assert against. An
    // adapter's `recommended_poll_after`, when present, still gets floored
    // here before jitter spreads it.
    let delay = with_jitter(
        clamp_recommended(obs.recommended_poll_after, base),
        seed(&row.id, attempt_seed),
    );
    u.next_check_at = plus(now, delay);
    u.events = events;
    u
}

/// Thin I/O wrapper: claim due monitors, observe each one, plan the cycle,
/// write it back exactly once (requirement 2 — never call `apply_cycle`
/// twice for the same claim; there is no retry loop here).
pub fn tick(
    store: &MonitorStore,
    registry: &AdapterRegistry,
    now: DateTime<Utc>,
    owner: &str,
    max_claims: usize,
) -> Result<TickReport> {
    // Sweep expired leases before claiming. `recover` also calls this, but
    // only runs once at daemon startup — a monitor whose lease expires while
    // the daemon keeps running (a transient error earlier in `tick` left it
    // `checking` with a live lease, or a worker genuinely died mid-check) is
    // otherwise stranded forever: `claim_due`'s `WHERE state IN (claimable)`
    // excludes `checking` outright, so an expired-but-unswept lease is never
    // picked back up no matter how long `next_check_at` sits in the past.
    // Cheap and idempotent — it only touches rows whose lease has genuinely
    // expired.
    store.expire_leases(now)?;
    let claimed = store.claim_due(now, owner, DEFAULT_LEASE, max_claims)?;
    let mut rep = TickReport {
        claimed: claimed.len(),
        ..Default::default()
    };
    let started = Instant::now();
    for c in claimed {
        // Refresh this monitor's own lease right before its observe, so the
        // lease only has to cover one observe (worst case ~30s for the
        // GitHub adapter) rather than the whole batch of `max_claims`
        // observes running serially. `now` advanced by real elapsed time
        // keeps the stored expiry honest about wall-clock progress through
        // the batch while `now` itself stays the logical clock the rest of
        // this function (and `plan_cycle`) reasons with.
        let beat_at = now
            + chrono::Duration::from_std(started.elapsed())
                .unwrap_or_else(|_| chrono::Duration::zero());
        if !store.heartbeat(&c.row.id, c.fence, beat_at, DEFAULT_LEASE)? {
            // Someone else already reclaimed this monitor (its lease
            // expired and another worker took it, bumping the fence).
            // Observing it now would be wasted work: `apply_cycle` would
            // reject the write with the same stale fence anyway. Skip
            // straight to the next claim without calling the adapter.
            rep.stale_fence += 1;
            tracing::warn!(monitor = %c.row.id, fence = c.fence, "monitor: stale fence before observe, skipped");
            continue;
        }
        // requirement 4: a missing adapter is an `unknown` observation, not
        // a crash — `registry.get()` returning `None` never panics or bails.
        let obs = match registry.get(c.row.source_type) {
            Some(a) => a.observe(
                &c.row.reference,
                c.row.spec.source.credential_ref.as_deref(),
            ),
            None => Observation::unknown(format!(
                "no adapter enabled for {}",
                c.row.source_type.as_str()
            )),
        }
        .redacted();
        let u = plan_cycle(&c.row, obs, now);
        // requirement 2: exactly one `apply_cycle` call per claim, no retry.
        // The semantic counters below only reflect a cycle that ACTUALLY
        // LANDED: a stale fence (another worker reclaimed this monitor
        // mid-cycle) means `apply_cycle` wrote nothing at all — no
        // observation row, no column change, no event, no cycle finish —
        // so counting `u.new_state`/`u.outcome` in that case would report a
        // write that never happened on disk.
        if store.apply_cycle(&c.row.id, c.fence, &u)? {
            rep.observed += 1;
            match u.new_state {
                MonitorState::Completed => rep.completed += 1,
                MonitorState::ActionPending => rep.action_pending += 1,
                MonitorState::Exhausted => rep.exhausted += 1,
                _ => {}
            }
            if u.outcome == Outcome::Unknown {
                rep.unknown += 1;
            }
        } else {
            rep.stale_fence += 1;
            tracing::warn!(monitor = %c.row.id, fence = c.fence, "monitor: stale fence, cycle dropped");
        }
    }
    Ok(rep)
}

/// Daemon start (spec §daemon 恢復 steps 2 and 4). Step 3 (outbox) and
/// step 5 (claimed actions) are plan-2.
pub fn recover(store: &MonitorStore, now: DateTime<Utc>) -> Result<RecoveryReport> {
    let recovered_leases = store.expire_leases(now)?;
    let overdue = store
        .list(&ListFilter::default())?
        .iter()
        .filter(|r| r.state.is_claimable() && r.next_check_at <= now)
        .count();
    Ok(RecoveryReport {
        recovered_leases,
        overdue,
    })
}

#[cfg(test)]
#[path = "scheduler_tests.rs"]
mod tests;

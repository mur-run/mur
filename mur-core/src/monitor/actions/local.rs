//! The three action executors this build ships (spec §行動執行器, plan-2
//! Task 3): `notify`, `collect_logs`, `reschedule_monitor`. All three are
//! Read-risk-tier and runnable without HITL — see
//! `mur_monitor::action::risk::classify`.

use serde_json::{Map, Value};

use mur_monitor::backoff::{MIN_INTERVAL, clamp_recommended, seed, unknown_delay, with_jitter};

use super::{ActionCtx, ActionExecutor};

/// `now + d`, saturating rather than panicking on overflow — same idiom as
/// `mur_monitor::scheduler`'s private `plus` helper, duplicated here because
/// that one is not `pub`.
fn plus(
    now: chrono::DateTime<chrono::Utc>,
    d: std::time::Duration,
) -> chrono::DateTime<chrono::Utc> {
    now + chrono::Duration::from_std(d).unwrap_or(chrono::Duration::MAX)
}

pub struct Notify;

impl ActionExecutor for Notify {
    fn verb(&self) -> &'static str {
        "notify"
    }

    /// Appends an `action_notify` event; never calls a notification channel
    /// directly. The shipped delivery path is
    /// `append_event` → transition guard → queue → drain (see
    /// `mur-core/src/monitor/notify.rs`), and calling a channel from here
    /// would bypass that path's delivery-state tracking and dedup.
    fn run(&self, ctx: &ActionCtx<'_>, params: &Map<String, Value>) -> Result<String, String> {
        let payload = Value::Object(params.clone());
        ctx.store
            .append_event(
                &ctx.row.id,
                &ctx.row.cycle_id,
                "action_notify",
                payload,
                false,
                ctx.now,
            )
            .map_err(|e| e.to_string())?;
        Ok(format!("queued action_notify for monitor {}", ctx.row.id))
    }
}

pub struct CollectLogs;

impl ActionExecutor for CollectLogs {
    fn verb(&self) -> &'static str {
        "collect_logs"
    }

    /// Read-only: looks up the registered adapter for `row.source_type` and
    /// calls its `observe`, exactly like an ordinary check cycle — no new
    /// network or credential scope. Redacts the evidence itself, in
    /// addition to (not instead of) whatever redaction the store applies,
    /// because this string becomes the action's stored result immediately,
    /// before any store-side pass runs on it.
    fn run(&self, ctx: &ActionCtx<'_>, _params: &Map<String, Value>) -> Result<String, String> {
        let adapter = ctx
            .registry
            .get(ctx.row.source_type)
            .ok_or_else(|| format!("no adapter registered for {:?}", ctx.row.source_type))?;
        let credential_ref = ctx.row.spec.source.credential_ref.as_deref();
        let observation = adapter.observe(&ctx.row.reference, credential_ref);
        Ok(mur_common::redact::redact_secrets(&observation.evidence).into_owned())
    }
}

pub struct Reschedule;

impl ActionExecutor for Reschedule {
    fn verb(&self) -> &'static str {
        "reschedule_monitor"
    }

    /// The `on_unknown` remedy: push `next_check_at` out using the existing
    /// `unknown` backoff schedule (never a literal duration) and return the
    /// monitor to `Sleeping` — the only verb that keeps a monitor alive.
    fn run(&self, ctx: &ActionCtx<'_>, _params: &Map<String, Value>) -> Result<String, String> {
        let base = unknown_delay(ctx.row.unknown_streak);
        let jittered = with_jitter(base, seed(&ctx.row.id, ctx.row.unknown_streak));
        let delay = clamp_recommended(None, jittered).max(MIN_INTERVAL);
        let next_check_at = plus(ctx.now, delay);
        let ok = ctx
            .store
            .reschedule(&ctx.row.id, next_check_at, ctx.now)
            .map_err(|e| e.to_string())?;
        if !ok {
            return Err(format!("monitor {} no longer exists", ctx.row.id));
        }
        Ok(format!("rescheduled to {next_check_at}, state Sleeping"))
    }
}

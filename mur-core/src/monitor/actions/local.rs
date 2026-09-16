//! The action executors this build can actually run (spec §行動執行器,
//! plan-2 Task 3): `notify` and `collect_logs`. Both are Read-risk-tier and
//! runnable without HITL — see `mur_monitor::action::risk::classify`.
//!
//! `reschedule_monitor` used to live here as a third executor and was
//! removed by the whole-branch review's H1: it wrote `Sleeping` onto a
//! monitor the scheduler had already settled, and `MonitorState::is_claimable`
//! is `Active | Sleeping`, so the next tick re-claimed the monitor, bumped
//! its fence, re-observed the SAME terminal and re-parked it in
//! `ActionPending` — with a new fence, hence new action keys, hence the
//! whole action list running again every poll interval, forever. See
//! `super::executor_for` for why nothing in this module may ever write a
//! claimable monitor state again.

use serde_json::{Map, Value};

use super::{ActionCtx, ActionExecutor};

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

//! The LOCAL action executors (spec §行動執行器, plan-2 Task 3): `notify`
//! and `collect_logs`. Both are Read-risk-tier and runnable without HITL —
//! see `mur_monitor::action::risk::classify`. They are not the whole set
//! this build can run: `rerun` is a Write-tier executor and lives in the
//! sibling `rerun.rs` precisely because it leaves the machine. The
//! authoritative list is `executor_for` in this module's parent.
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
    ///
    /// `params` comes straight from the monitor's spec YAML — a user- or
    /// integration-authored file that can contain anything, including a
    /// secret pasted into a notify param by mistake. `append_event` does no
    /// redaction of its own (spec §安全與隱私: secrets and un-redacted logs
    /// must never reach history), so this is the one chokepoint before the
    /// payload lands in `monitor_events`, which `show --history` prints
    /// raw. Walks the whole JSON tree (`redact_value`, not `redact_secrets`
    /// on a single string), since a param can nest objects/arrays.
    fn run(&self, ctx: &ActionCtx<'_>, params: &Map<String, Value>) -> Result<String, String> {
        let mut payload = Value::Object(params.clone());
        mur_common::redact::redact_value(&mut payload);
        ctx.events.append("action_notify", payload)?;
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
    ///
    /// spec §錯誤處理's central distinction, carried down from the monitor
    /// layer to the action layer (L6, whole-branch review): a source the
    /// adapter could not read is a MONITOR problem, not a work problem, and
    /// must not be stored as a successful collection. `Observation` already
    /// carries this signal two ways — `adapter_error` (monitor-side reason)
    /// and `outcome == Unknown` (the same fact, for a caller that only
    /// checks the enum) — so both are checked; either one fails the action
    /// instead of returning `Ok` with the error text sitting in the
    /// `evidence` field as if it were collected evidence.
    fn run(&self, ctx: &ActionCtx<'_>, _params: &Map<String, Value>) -> Result<String, String> {
        let adapter = ctx
            .registry
            .get(ctx.row.source_type)
            .ok_or_else(|| format!("no adapter registered for {:?}", ctx.row.source_type))?;
        let credential_ref = ctx.row.spec.source.credential_ref.as_deref();
        let observation = adapter.observe(&ctx.row.reference, credential_ref);
        if let Some(error) = &observation.adapter_error {
            return Err(mur_common::redact::redact_secrets(&format!(
                "the source could not be read: {error}"
            ))
            .into_owned());
        }
        if observation.outcome == mur_monitor::state::Outcome::Unknown {
            return Err(
                "the source could not be read: adapter returned an unknown outcome".to_string(),
            );
        }
        Ok(mur_common::redact::redact_secrets(&observation.evidence).into_owned())
    }
}

//! Bridges the monitor tick's plain OS thread to the repo's one risk-tiered
//! HITL gate (`crate::hitl::gate`, v3c) — spec §行動執行器 / §風險政策.
//!
//! `gate()` is `async`; the monitor tick never runs on the tokio runtime
//! (`mur-daemon/src/monitor_tick.rs`'s module doc: rusqlite is synchronous
//! and the adapters block, so this never runs on the tokio runtime).
//! `decide` bridges the two worlds with `Handle::block_on`, which is the
//! documented, correct way to drive a future from a thread that is not
//! itself a runtime worker. The forbidden direction is the mirror image —
//! calling `block_on` FROM a runtime worker, or building a blocking client
//! inside an async context — see the comment on
//! `mur-core/src/monitor/adapters/github_actions.rs` for the CRITICAL bug
//! that shipped from getting this backwards. `handle` is threaded down by
//! the caller (Task 5), sourced from `Handle::current()` inside
//! `mur-daemon/src/main.rs`'s `#[tokio::main]`.
//!
//! Two invariants that must never drift, because this path is unattended by
//! definition:
//! - `GatePolicy.yes` is always `false` — `--yes` is reserved for an
//!   explicit human choice, never for a background poll.
//! - `unanswered` is always `Unanswered::Defer` — with no TTY, `Wait` would
//!   block the whole monitor thread for the gate's timeout and stop every
//!   other monitor from being polled in the meantime.
//!
//! The channel id is DERIVED (`monitor-<id>`), never stored: `action_hash`
//! already folds the channel id in, so a stable derivation is all a later
//! approval needs to find its way back to the right gate — and it means an
//! approval survives anything that rewrites the monitor row.

use std::path::Path;

use anyhow::Result;
use chrono::{DateTime, Utc};
use mur_common::hitl::Unanswered;
use mur_monitor::action::risk;
use mur_monitor::store::MonitorRow;

use crate::hitl::gate::{ActionRequest, GateDecision, GatePolicy, gate};
use crate::hitl::pin::action_hash;

/// The channel a monitor's HITL traffic lives on. Derived, not stored, so an
/// approval survives anything that rewrites the monitor row (spec: "the
/// channel id is derived, not stored").
pub fn channel_id_for(monitor_id: &str) -> String {
    format!("monitor-{monitor_id}")
}

/// Decide whether one proposed action may run, routing it through the same
/// risk-tiered gate every other unattended surface (fleet loop, workflow
/// executor) uses. Never blocks the calling OS thread past what
/// `Handle::block_on` itself costs: an Ask-tier action with nobody watching
/// PARKS a durable `HitlRequest` and returns at once (`GateDecision.deferred`)
/// — it does not sit out the gate's wait timeout.
///
/// `now` is accepted for signature symmetry with `ActionCtx` (Task 3) and to
/// leave room for a future deterministic stamp; the gate itself resolves
/// approval TTLs against wall-clock time and is not handed `now` here, so
/// this parameter is currently unused (renamed `_now` to say so honestly
/// rather than pretend a value flows through that does not).
pub fn decide(
    handle: &tokio::runtime::Handle,
    mur_home: &Path,
    row: &MonitorRow,
    action_type: &str,
    action_index: usize,
    params: &serde_json::Map<String, serde_json::Value>,
    _now: DateTime<Utc>,
) -> Result<GateDecision> {
    let req = ActionRequest {
        tier: risk::classify(action_type),
        tool_name: format!("monitor:{action_type}"),
        tool_input: serde_json::Value::Object(params.clone()),
        step_or_call_id: format!("{}:{action_index}", row.cycle_id),
        agent_id: format!("monitor:{}", row.id),
        summary: format!(
            "monitor {} ({} {}): {action_type}",
            row.name,
            row.source_type.as_str(),
            row.reference
        ),
    };
    let policy = GatePolicy {
        // Never `true` on this path: an unattended monitor tick must never
        // auto-approve every Ask-tier action the way an explicit `--yes`
        // would.
        yes: false,
        // Park and return at once; a non-TTY thread that instead `Wait`ed
        // would stall every other monitor behind this one gate.
        unanswered: Unanswered::Defer,
        // No config source exists yet for a monitor's own pre-approved
        // tiers (unlike `fleet.yaml`'s `hitl.auto_approve_tiers`). Until one
        // is wired up, every Ask-tier action asks — empty, not a guess.
        auto_approve_tiers: Vec::new(),
    };
    let channel_id = channel_id_for(&row.id);
    handle.block_on(gate(mur_home, &channel_id, &req, &policy, None, None))
}

/// Recompute the pin for `(row, action_type, action_index, params)`, using
/// the exact same five field derivations `decide` feeds into
/// `ActionRequest` and `gate` in turn feeds into `action_hash`. Duplicated
/// rather than routed back through `decide` (which needs a `handle` and
/// calls `gate`, an `async` I/O function) because the drain's Rule 1
/// re-verification step must recompute the pin FROM the current inputs
/// without going through the gate again — that is what "re-verify" means.
///
/// Task 5's drain calls this immediately before executing an approved
/// action; a mismatch against `GateDecision::action_hash` means the action
/// changed after approval and the caller must fail closed rather than run
/// it (spec: the executor re-verifies the hash at the execute boundary).
pub fn expected_hash(
    row: &MonitorRow,
    action_type: &str,
    action_index: usize,
    params: &serde_json::Map<String, serde_json::Value>,
) -> String {
    action_hash(
        &format!("monitor:{action_type}"),
        &serde_json::Value::Object(params.clone()),
        &channel_id_for(&row.id),
        &format!("{}:{action_index}", row.cycle_id),
        &format!("monitor:{}", row.id),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use mur_channel::ChannelService;
    use mur_common::channel::{ChannelActor, EventKind};
    use mur_common::hitl::{HitlRequest, HitlResponse};
    use mur_monitor::spec::MonitorSpec;
    use mur_monitor::store::MonitorStore;
    use serde_json::json;
    use std::path::PathBuf;

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 16, 9, 0, 0).unwrap()
    }

    fn empty_params() -> serde_json::Map<String, serde_json::Value> {
        serde_json::Map::new()
    }

    fn params_with_job(job: &str) -> serde_json::Map<String, serde_json::Value> {
        let mut m = serde_json::Map::new();
        m.insert("job".to_string(), json!(job));
        m
    }

    /// A single monitor row plus its own `mur_home`, so `MonitorStore` and
    /// `ChannelService` share a root the way the daemon does.
    fn fixture() -> (tempfile::TempDir, PathBuf, MonitorRow) {
        let d = tempfile::tempdir().unwrap();
        let home = d.path().to_path_buf();
        let s = MonitorStore::open(&home).unwrap();
        let spec = MonitorSpec::from_yaml(
            "schema_version: 1\nname: t\nsource: { type: github_actions, reference: r1 }\n\
             idempotency_key: k1\ncreated_by: { actor: user:test }\n",
        )
        .unwrap();
        let created = s.create(&spec, t0(), None).unwrap();
        let row = s.get(&created.id).unwrap().unwrap();
        (d, home, row)
    }

    /// Write a `HitlResponse` approving the parked request for `action_hash`,
    /// the same shape `mur channel approve` writes and the same unsigned
    /// `ChannelService::append` path the existing gate tests' `answer()`
    /// helper uses (`mur-core/src/hitl/gate.rs`). Looked up by `action_hash`
    /// (not a known `hitl_id`) because that is all a caller of `decide` ever
    /// gets back.
    fn approve_on_channel(home: &Path, channel_id: &str, action_hash: &str) {
        let svc = ChannelService::open(home).unwrap();
        let hitl_id = svc
            .load_events(channel_id)
            .unwrap()
            .iter()
            .filter(|e| e.kind == EventKind::HitlRequest)
            .filter_map(|e| serde_json::from_value::<HitlRequest>(e.payload.clone()).ok())
            .find(|r| r.action_hash == action_hash)
            .map(|r| r.hitl_id)
            .expect("a HitlRequest for this action_hash exists");
        let resp = HitlResponse {
            hitl_id,
            action_hash: action_hash.to_string(),
            allow: true,
            reason: "test".into(),
            surface: "cli".into(),
        };
        svc.append(
            channel_id,
            ChannelActor::System,
            EventKind::HitlResponse,
            serde_json::to_value(&resp).unwrap(),
            None,
        )
        .unwrap();
    }

    #[test]
    fn a_read_tier_action_is_allowed_without_asking_anyone() {
        let (_d, home, row) = fixture();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let d = decide(rt.handle(), &home, &row, "notify", 0, &empty_params(), t0()).unwrap();
        assert!(d.allow, "{}", d.reason);
        assert!(!d.deferred);
    }

    #[test]
    fn a_write_tier_action_defers_instead_of_blocking_the_thread() {
        // The property that keeps one gated action from stopping every other
        // monitor: no TTY means park and return at once.
        let (_d, home, row) = fixture();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let started = std::time::Instant::now();
        let d = decide(rt.handle(), &home, &row, "rerun", 0, &empty_params(), t0()).unwrap();
        assert!(!d.allow);
        assert!(d.deferred, "unattended must defer, not wait: {}", d.reason);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "deferring must not wait on the gate timeout"
        );
        // Positive evidence the gate was actually invoked, not stubbed: a
        // hardcoded `deferred: true` with no channel write would satisfy
        // every assertion above. Require a real HitlRequest, for this exact
        // action_hash, to be parked on the derived channel.
        let svc = ChannelService::open(&home).unwrap();
        let parked = svc
            .load_events(&channel_id_for(&row.id))
            .unwrap()
            .iter()
            .any(|e| {
                e.kind == EventKind::HitlRequest
                    && e.payload.get("action_hash").and_then(|v| v.as_str())
                        == Some(d.action_hash.as_str())
            });
        assert!(
            parked,
            "deferring must actually park a HitlRequest, not just report deferred"
        );
    }

    #[test]
    fn the_decision_carries_a_hash_the_caller_can_re_verify() {
        let (_d, home, row) = fixture();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let a = decide(
            rt.handle(),
            &home,
            &row,
            "rerun",
            0,
            &params_with_job("x"),
            t0(),
        )
        .unwrap();
        let b = decide(
            rt.handle(),
            &home,
            &row,
            "rerun",
            0,
            &params_with_job("x"),
            t0(),
        )
        .unwrap();
        let c = decide(
            rt.handle(),
            &home,
            &row,
            "rerun",
            0,
            &params_with_job("y"),
            t0(),
        )
        .unwrap();
        assert_eq!(a.action_hash, b.action_hash, "same action, same pin");
        assert_ne!(
            a.action_hash, c.action_hash,
            "changing the params must invalidate the pin"
        );
        assert!(!a.action_hash.is_empty());
    }

    #[test]
    fn an_approval_already_on_the_channel_releases_the_gate() {
        // Write a HitlResponse for the exact action_hash, then gate again.
        // This is the whole point of deferring: the answer arrives later and
        // the NEXT tick proceeds without asking again.
        let (_d, home, row) = fixture();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let first = decide(rt.handle(), &home, &row, "rerun", 0, &empty_params(), t0()).unwrap();
        approve_on_channel(&home, &channel_id_for(&row.id), &first.action_hash);
        let second = decide(rt.handle(), &home, &row, "rerun", 0, &empty_params(), t0()).unwrap();
        assert!(
            second.allow,
            "a settled approval must release the gate: {}",
            second.reason
        );
    }

    #[test]
    fn the_channel_id_is_derived_from_the_monitor_id() {
        assert_eq!(channel_id_for("01a0a75e-c532"), "monitor-01a0a75e-c532");
    }

    #[test]
    fn expected_hash_matches_what_decide_actually_computed() {
        // The re-verification helper must derive the identical pin `decide`
        // got back — not merely "a" hash. A formula that silently diverges
        // (e.g. forgetting `action_index`) would make Rule 1's re-check
        // either always-fail (breaking every approved action) or a no-op
        // that only coincidentally passes.
        let (_d, home, row) = fixture();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let d = decide(
            rt.handle(),
            &home,
            &row,
            "rerun",
            0,
            &params_with_job("x"),
            t0(),
        )
        .unwrap();
        assert_eq!(
            expected_hash(&row, "rerun", 0, &params_with_job("x")),
            d.action_hash
        );
        assert_ne!(
            expected_hash(&row, "rerun", 0, &params_with_job("y")),
            d.action_hash,
            "a changed input must not re-verify against the old pin"
        );
    }
}

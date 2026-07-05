//! `mur channel approve <channel_id> <hitl_id> [--deny] [--reason ...]` — append
//! a HitlResponse to a channel, releasing a gate that is waiting (v3c). This is
//! the CLI/headless responder; Hub/iOS approval UIs are additive follow-ons.

use std::path::Path;

use anyhow::{Context, Result};
use mur_channel::ChannelService;
use mur_common::channel::{ChannelActor, ChannelEvent, EventKind};
use mur_common::hitl::{HitlRequest, HitlResponse, RiskTier};

use crate::channel_writer::ROUTER_AGENT;

/// One unresolved HITL gate, surfaced to a UI (Hub Home inbox, mobile, etc).
/// Deliberately flat/serde-friendly rather than re-exporting `HitlRequest`
/// directly, so callers aren't coupled to the channel-event payload shape.
#[derive(Debug, Clone, serde::Serialize)]
#[allow(dead_code)] // consumed by mur-hub-gui (workspace-excluded Home inbox)
pub struct PendingHitlGate {
    pub channel_id: String,
    pub hitl_id: String,
    pub agent: String,
    pub summary: String,
    pub risk: RiskTier,
    pub ts: chrono::DateTime<chrono::Utc>,
}

/// Fold every channel's events and return `HitlRequest`s with no matching
/// `HitlResponse` in the same channel — i.e. gates still waiting on a human.
/// Read errors on an individual channel are skipped (fail-open: a corrupt or
/// unreadable channel must not hide gates in every other channel).
#[allow(dead_code)] // consumed by mur-hub-gui (workspace-excluded Home inbox)
pub fn pending_hitl_gates(mur_home: &Path) -> Result<Vec<PendingHitlGate>> {
    let svc = ChannelService::open(mur_home)?;
    let ids = svc.store().list_ids()?;

    let mut out = Vec::new();
    for channel_id in ids {
        let Ok(events) = svc.load_events(&channel_id) else {
            continue;
        };
        out.extend(unresolved_gates_in(&channel_id, &events));
    }
    Ok(out)
}

/// Pure helper (no I/O) so the fold logic is unit-testable without a real
/// `ChannelService` fixture beyond a temp dir of event files.
fn unresolved_gates_in(channel_id: &str, events: &[ChannelEvent]) -> Vec<PendingHitlGate> {
    let responded: std::collections::HashSet<String> = events
        .iter()
        .filter(|e| e.kind == EventKind::HitlResponse)
        .filter_map(|e| serde_json::from_value::<HitlResponse>(e.payload.clone()).ok())
        .map(|r| r.hitl_id)
        .collect();

    events
        .iter()
        .filter(|e| e.kind == EventKind::HitlRequest)
        .filter_map(|e| {
            let req: HitlRequest = serde_json::from_value(e.payload.clone()).ok()?;
            if responded.contains(&req.hitl_id) {
                return None;
            }
            Some(PendingHitlGate {
                channel_id: channel_id.to_string(),
                hitl_id: req.hitl_id,
                agent: req.agent_id,
                summary: req.summary,
                risk: req.tier,
                ts: e.ts,
            })
        })
        .collect()
}

pub fn approve(channel_id: &str, hitl_id: &str, deny: bool, reason: Option<String>) -> Result<()> {
    let home = crate::paths::mur_root(None);
    let svc = ChannelService::open(&home)?;

    // Find the matching HitlRequest to echo its action_hash (so the gate's
    // re-verify passes). Refuse if there is no such pending request.
    let evs = svc.load_events(channel_id)?;
    let request: HitlRequest = evs
        .iter()
        .rev()
        .filter(|e| e.kind == EventKind::HitlRequest)
        .find_map(|e| serde_json::from_value::<HitlRequest>(e.payload.clone()).ok())
        .filter(|r| r.hitl_id == hitl_id)
        .with_context(|| format!("no pending HitlRequest {hitl_id} in channel {channel_id}"))?;

    let resp = HitlResponse {
        hitl_id: request.hitl_id,
        action_hash: request.action_hash,
        allow: !deny,
        reason: reason.unwrap_or_default(),
        surface: "cli".into(),
    };
    // The actor is the local human who approved, but the channel's WRITER signs
    // the event (v3d) so the gate can verify authority before releasing — a
    // forged HitlResponse from a non-router key is rejected. The router for
    // workflow/HITL channels is the concierge "mur".
    crate::channel_writer::append_as_writer(
        &svc,
        &home,
        channel_id,
        ROUTER_AGENT,
        ChannelActor::local_human(),
        EventKind::HitlResponse,
        serde_json::to_value(&resp)?,
        None,
    )?;
    println!(
        "{} {hitl_id} on channel {channel_id}",
        if deny { "denied" } else { "approved" }
    );
    Ok(())
}

#[cfg(test)]
mod pending_hitl_tests {
    use super::*;
    use chrono::Utc;
    use mur_common::channel::ChannelActor;

    fn hitl_request_event(seq: u64, hitl_id: &str) -> ChannelEvent {
        let req = HitlRequest {
            hitl_id: hitl_id.to_string(),
            action_hash: "deadbeef".into(),
            tier: RiskTier::Write,
            tool_name: "fs_write".into(),
            tool_input: serde_json::json!({}),
            step_or_call_id: "step-1".into(),
            agent_id: "buildy".into(),
            timeout_ms: 60_000,
            summary: "write config.yaml".into(),
        };
        ChannelEvent {
            seq,
            ts: Utc::now(),
            actor: ChannelActor::System,
            kind: EventKind::HitlRequest,
            payload: serde_json::to_value(&req).unwrap(),
            idempotency_key: None,
            sig: None,
            key_version: None,
        }
    }

    fn hitl_response_event(seq: u64, hitl_id: &str) -> ChannelEvent {
        let resp = HitlResponse {
            hitl_id: hitl_id.to_string(),
            action_hash: "deadbeef".into(),
            allow: true,
            reason: String::new(),
            surface: "cli".into(),
        };
        ChannelEvent {
            seq,
            ts: Utc::now(),
            actor: ChannelActor::local_human(),
            kind: EventKind::HitlResponse,
            payload: serde_json::to_value(&resp).unwrap(),
            idempotency_key: None,
            sig: None,
            key_version: None,
        }
    }

    fn write_events_jsonl(mur_home: &Path, channel_id: &str, events: &[ChannelEvent]) {
        let dir = mur_home.join("channels").join(channel_id);
        std::fs::create_dir_all(&dir).unwrap();
        let mut body = String::new();
        for e in events {
            body.push_str(&serde_json::to_string(e).unwrap());
            body.push('\n');
        }
        std::fs::write(dir.join("events.jsonl"), body).unwrap();
    }

    #[test]
    fn unresolved_gate_included_resolved_gate_excluded() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();

        // Channel A: one unresolved request.
        write_events_jsonl(home, "chan-a", &[hitl_request_event(1, "hitl-open")]);
        // Channel B: a request later resolved by a matching response.
        write_events_jsonl(
            home,
            "chan-b",
            &[
                hitl_request_event(1, "hitl-closed"),
                hitl_response_event(2, "hitl-closed"),
            ],
        );

        let gates = pending_hitl_gates(home).unwrap();
        assert_eq!(gates.len(), 1);
        assert_eq!(gates[0].hitl_id, "hitl-open");
        assert_eq!(gates[0].channel_id, "chan-a");
        assert_eq!(gates[0].agent, "buildy");
        assert_eq!(gates[0].risk, RiskTier::Write);
    }

    #[test]
    fn unresolved_gates_in_pure_helper() {
        let events = vec![
            hitl_request_event(1, "hitl-open"),
            hitl_request_event(2, "hitl-closed"),
            hitl_response_event(3, "hitl-closed"),
        ];
        let gates = unresolved_gates_in("chan-x", &events);
        assert_eq!(gates.len(), 1);
        assert_eq!(gates[0].hitl_id, "hitl-open");
    }
}

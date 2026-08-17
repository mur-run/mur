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

/// What a backfill run did (or would do).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct BackfillReport {
    /// Legacy manifests that would be corrected (dry run).
    pub would_change: usize,
    /// Manifests actually written (apply).
    pub changed: usize,
    /// Manifests already carrying an explicit purpose.
    pub already_set: usize,
}

/// Classify legacy channels and, with `apply`, persist the inferred purpose.
///
/// This is the ONLY path that writes an inferred purpose. Read paths resolve
/// purpose in memory precisely so a listing can never produce an unauditable
/// migration write.
pub fn backfill_purpose(home: &Path, apply: bool, limit: usize) -> Result<BackfillReport> {
    let svc = ChannelService::open(home)?;
    let mut report = BackfillReport::default();

    for id in svc.store().list_ids()? {
        if report.changed >= limit || report.would_change >= limit {
            break;
        }
        let Ok(mut ch) = svc.store().load_manifest(&id) else {
            continue;
        };
        if ch.purpose.is_some() {
            report.already_set += 1;
            continue;
        }
        let inferred = mur_channel::purpose::effective_purpose(&ch);
        if apply {
            ch.purpose = Some(inferred);
            svc.store().save_manifest(&ch)?;
            svc.index().upsert(&ch)?;
            report.changed += 1;
            println!("  {id} → {inferred:?}");
        } else {
            report.would_change += 1;
            println!("  {id} → {inferred:?} (dry run)");
        }
    }

    if apply {
        println!(
            "backfilled {} channel(s); {} already had a purpose",
            report.changed, report.already_set
        );
    } else {
        println!(
            "would backfill {} channel(s); {} already have a purpose — re-run with --apply",
            report.would_change, report.already_set
        );
    }
    Ok(report)
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

#[cfg(test)]
mod backfill_tests {
    use super::*;
    use mur_channel::ChannelService;
    use mur_common::channel::{ChannelActor, ChannelPurpose, EventKind};
    use tempfile::TempDir;

    /// Strip `purpose` from a manifest on disk, simulating a legacy channel.
    /// Manifests are YAML (`channel.yaml`), written by `ChannelStore::save_manifest`.
    fn make_legacy(home: &std::path::Path, id: &str) {
        let path = home.join("channels").join(id).join("channel.yaml");
        let mut v: serde_yaml::Value =
            serde_yaml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        v.as_mapping_mut()
            .unwrap()
            .remove(serde_yaml::Value::String("purpose".into()));
        std::fs::write(&path, serde_yaml::to_string(&v).unwrap()).unwrap();
    }

    #[test]
    fn dry_run_reports_but_writes_nothing() {
        // The fixture MUST make the index's recorded purpose disagree with
        // what fresh inference would produce from the (legacy) manifest.
        // `create_for_agent` + `make_legacy` alone is NOT enough: it strips
        // `purpose` but leaves a UUID id and a plain title, so
        // `effective_purpose` still infers `Conversation` — the exact same
        // string the index already holds ("conversation"). Under that
        // fixture, a regression that let dry-run call `index().upsert(&ch)`
        // would recompute "conversation" and write back the identical value,
        // and the "index unchanged" assertion below would pass with the bug
        // present. So instead we create a *workflow* channel (index row =
        // "workflow-run"), then simulate the legacy state by clearing
        // `purpose` AND stripping the "workflow: " title prefix, so a fresh
        // inference lands on `Conversation` ("conversation") while the index
        // still says "workflow-run". Only then does an untouched index prove
        // dry-run truly wrote nothing — a premature upsert would visibly
        // flip it to "conversation".
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_workflow("release").unwrap();
        svc.append_message(
            &ch.id,
            ChannelActor::local_human(),
            EventKind::Message,
            "hi",
            None,
        )
        .unwrap();

        let mut manifest = svc.store().load_manifest(&ch.id).unwrap();
        manifest.purpose = None;
        manifest.title = "hello".to_string();
        svc.store().save_manifest(&manifest).unwrap();

        let index_purpose_before = svc
            .index()
            .list(100)
            .unwrap()
            .into_iter()
            .find(|r| r.id == ch.id)
            .unwrap()
            .purpose;
        assert_eq!(
            index_purpose_before, "workflow-run",
            "fixture sanity: the index must still hold the value written at creation"
        );

        let report = backfill_purpose(tmp.path(), false, 100).unwrap();

        assert_eq!(report.would_change, 1);
        assert_eq!(report.changed, 0);
        assert_eq!(
            svc.store().load_manifest(&ch.id).unwrap().purpose,
            None,
            "a dry run must not touch disk"
        );
        let index_purpose_after = svc
            .index()
            .list(100)
            .unwrap()
            .into_iter()
            .find(|r| r.id == ch.id)
            .unwrap()
            .purpose;
        assert_eq!(
            index_purpose_after, "workflow-run",
            "a dry run must not touch the SQLite index either — a premature \
             index.upsert would have recomputed effective_purpose from the \
             now-legacy manifest and rewritten this to \"conversation\""
        );
    }

    #[test]
    fn apply_writes_the_inferred_purpose() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("mur").unwrap();
        svc.append_message(
            &ch.id,
            ChannelActor::local_human(),
            EventKind::Message,
            "hi",
            None,
        )
        .unwrap();
        make_legacy(tmp.path(), &ch.id);

        let report = backfill_purpose(tmp.path(), true, 100).unwrap();

        assert_eq!(report.changed, 1);
        assert_eq!(
            svc.store().load_manifest(&ch.id).unwrap().purpose,
            Some(ChannelPurpose::Conversation)
        );
    }

    #[test]
    fn apply_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("mur").unwrap();
        svc.append_message(
            &ch.id,
            ChannelActor::local_human(),
            EventKind::Message,
            "hi",
            None,
        )
        .unwrap();
        make_legacy(tmp.path(), &ch.id);

        backfill_purpose(tmp.path(), true, 100).unwrap();
        let second = backfill_purpose(tmp.path(), true, 100).unwrap();

        assert_eq!(second.changed, 0, "a second run must find nothing to do");
    }

    #[test]
    fn an_explicit_purpose_is_never_overwritten() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        // A fleet-shaped id (`fleet-projectx`) that inference would classify
        // as FleetRun, but whose stored purpose was explicitly recorded as a
        // conversation. Only this id/purpose mismatch can distinguish "left
        // alone" from "re-derived and happened to match".
        let ch = svc
            .create_for_fleet("projectx", "router", &["worker".to_string()])
            .unwrap();
        let mut manifest = svc.store().load_manifest(&ch.id).unwrap();
        manifest.purpose = Some(ChannelPurpose::Conversation);
        svc.store().save_manifest(&manifest).unwrap();

        let report = backfill_purpose(tmp.path(), true, 100).unwrap();

        assert_eq!(report.changed, 0);
        assert_eq!(
            svc.store().load_manifest(&ch.id).unwrap().purpose,
            Some(ChannelPurpose::Conversation),
            "inference must not reclassify an explicitly-set purpose as FleetRun"
        );
    }

    #[test]
    fn limit_bounds_the_batch() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        for _ in 0..3 {
            let ch = svc.create_for_agent("mur").unwrap();
            svc.append_message(
                &ch.id,
                ChannelActor::local_human(),
                EventKind::Message,
                "hi",
                None,
            )
            .unwrap();
            make_legacy(tmp.path(), &ch.id);
        }

        let report = backfill_purpose(tmp.path(), true, 2).unwrap();
        assert_eq!(report.changed, 2);
    }
}

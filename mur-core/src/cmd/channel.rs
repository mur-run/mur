//! `mur channel approve <channel_id> <hitl_id> [--deny] [--reason ...]` — append
//! a HitlResponse to a channel, releasing a gate that is waiting (v3c). This is
//! the CLI/headless responder; Hub/iOS approval UIs are additive follow-ons.

use anyhow::{Context, Result};
use mur_channel::ChannelService;
use mur_common::channel::{ChannelActor, EventKind};
use mur_common::hitl::{HitlRequest, HitlResponse};

use crate::channel_writer::ROUTER_AGENT;

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

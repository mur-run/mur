use mur_core::a2a_dial::{DialMode, dial_method};
use serde::Serialize;
use serde_json::json;

#[tauri::command]
pub fn agent_hitl_respond(
    name: String,
    hitl_id: String,
    allow: bool,
    reason: Option<String>,
) -> Result<(), String> {
    let home = crate::mur_home_path();
    let mut payload = json!({ "hitl_id": hitl_id, "allow": allow });
    if let Some(r) = reason {
        payload["reason"] = json!(r);
    }
    dial_method(
        &home,
        &name,
        "tool/hitl_respond",
        payload,
        DialMode::RequireRunning,
    )
    .map(|_| ())
    .map_err(|e| format!("{e:#}"))
}

/// Respond to a CHANNEL/workflow HITL gate (risk-tiered v3c gate) from the
/// Activity panel — appends a signed HitlResponse to the channel, the same
/// thing `mur channel approve` does on the CLI. Distinct from
/// `agent_hitl_respond`, which answers an agent's in-chat tool gate.
#[tauri::command]
pub fn channel_hitl_respond(
    channel_id: String,
    hitl_id: String,
    allow: bool,
    reason: Option<String>,
) -> Result<(), String> {
    // `approve` takes `deny`, so invert.
    mur_core::cmd::channel::approve(&channel_id, &hitl_id, !allow, reason)
        .map_err(|e| format!("{e:#}"))
}

/// One unresolved HITL gate for the Home unified inbox.
#[derive(Debug, Clone, Serialize)]
pub struct HitlRequestView {
    pub channel_id: String,
    pub hitl_id: String,
    pub agent: String,
    pub summary: String,
    pub risk: String,
    pub ts: String,
}

impl From<mur_core::cmd::channel::PendingHitlGate> for HitlRequestView {
    fn from(g: mur_core::cmd::channel::PendingHitlGate) -> Self {
        Self {
            channel_id: g.channel_id,
            hitl_id: g.hitl_id,
            agent: g.agent,
            summary: g.summary,
            risk: serde_json::to_value(g.risk)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_else(|| "write".into()),
            ts: g.ts.to_rfc3339(),
        }
    }
}

/// Pure(ish): list every unresolved HITL gate across all channels. Fail-open:
/// any read error yields an empty list rather than propagating (the per-agent
/// chat views remain the authoritative HITL surface; this is a convenience
/// rollup). Shared by `hitl_pending_list` (Home inbox) and `panel_activities`
/// (Activities tab, filtered by agent).
pub fn pending_views() -> Vec<HitlRequestView> {
    let home = crate::mur_home_path();
    match mur_core::cmd::channel::pending_hitl_gates(&home) {
        Ok(gates) => gates.into_iter().map(HitlRequestView::from).collect(),
        Err(e) => {
            tracing::warn!("pending_views: failed to fold channels: {e:#}");
            vec![]
        }
    }
}

/// List every unresolved HITL gate across all channels, for the Home unified
/// inbox.
#[tauri::command]
pub fn hitl_pending_list() -> Result<Vec<HitlRequestView>, String> {
    Ok(pending_views())
}

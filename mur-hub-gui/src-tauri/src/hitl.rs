use mur_core::a2a_dial::{DialMode, dial_method};
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

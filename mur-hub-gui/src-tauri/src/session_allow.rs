//! "Don't ask again this session" for the Hub chat.
//!
//! WHAT a session answer may cover is not decided here: it is
//! `mur_core::cmd::agent::cli::session_grant::session_grant`, the same door the
//! CLI's `session_tool_allow` goes through. This module only remembers the keys
//! the operator granted and answers "is this whole batch already granted?".
//!
//! Scope: a key is remembered per `(agent, channel_id)`, in memory only, so a
//! Hub restart forgets every answer. `agent_chat_send` resolves every turn to a
//! concrete channel before it starts (the explicit pick, else the latest, else a
//! fresh one), so even an agent's first turn is scoped. Only a turn whose channel
//! store fails stays unscoped: it is never auto-allowed and never offered the
//! session row.
//!
//! An auto-allow is never silent: the batch is still emitted with
//! `auto_allowed: true` (the chat shows an info card) and logged.

use mur_common::hitl::RiskTier;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

type Scope = (String, String);

/// Granted session keys per `(agent, channel_id)`.
#[derive(Default)]
pub struct SessionAllow(Mutex<HashMap<Scope, HashSet<String>>>);

impl SessionAllow {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Scope, HashSet<String>>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn grant(&self, agent: &str, channel: &str, key: &str) {
        self.lock()
            .entry((agent.to_string(), channel.to_string()))
            .or_default()
            .insert(key.to_string());
    }

    pub fn contains(&self, agent: &str, channel: &str, key: &str) -> bool {
        self.lock()
            .get(&(agent.to_string(), channel.to_string()))
            .is_some_and(|s| s.contains(key))
    }

    /// Forget every answer for `agent`, across all its channels.
    pub fn reset(&self, agent: &str) {
        self.lock().retain(|(a, _), _| a != agent);
    }
}

/// Tauri-managed wrapper around [`SessionAllow`].
#[derive(Default)]
pub struct SessionAllowState(pub SessionAllow);

/// One call of a batch, decorated with what a session answer would cover.
#[derive(Debug, Clone, PartialEq)]
pub struct DecoratedCall {
    pub wire: Value,
    pub hitl_id: String,
    /// `(key, label)`; `None` = this call must keep asking.
    pub grant: Option<(String, String)>,
}

/// What to do with one `tool/approval_needed` batch.
#[derive(Debug, PartialEq)]
pub enum Plan {
    /// Every call carries a key the operator already granted in this scope.
    AutoAllow(Vec<DecoratedCall>),
    /// At least one call is not covered: the operator decides the whole batch.
    Ask(Vec<DecoratedCall>),
}

/// The batch's calls, or the legacy single call when `calls` is absent.
fn calls_of(params: &Value) -> Vec<Value> {
    match params.get("calls").and_then(Value::as_array) {
        Some(c) if !c.is_empty() => c.clone(),
        _ => vec![json!({
            "hitl_id": params.get("hitl_id"),
            "step_id": params.get("step_id"),
            "tool_name": params.get("tool_name"),
            "tool_input": params.get("tool_input"),
            "action_hash": params.get("action_hash"),
            "risk": params.get("risk"),
        })],
    }
}

fn decorate(call: Value, scoped: bool) -> DecoratedCall {
    let tool = call.get("tool_name").and_then(Value::as_str).unwrap_or("");
    let input = call.get("tool_input").cloned().unwrap_or(Value::Null);
    let declared: Option<RiskTier> = call
        .get("risk")
        .and_then(|r| serde_json::from_value(r.clone()).ok());
    let grant = scoped
        .then(|| mur_core::cmd::agent::cli::session_grant::session_grant(tool, &input, declared))
        .flatten()
        .map(|g| (g.key, g.label));
    let hitl_id = call
        .get("hitl_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    DecoratedCall {
        wire: call,
        hitl_id,
        grant,
    }
}

/// Decide a batch. All-or-nothing: a partly covered batch asks for every call,
/// so the Hub never has to show a half-answered batch.
pub fn plan(store: &SessionAllow, agent: &str, channel: Option<&str>, params: &Value) -> Plan {
    let calls: Vec<DecoratedCall> = calls_of(params)
        .into_iter()
        .map(|c| decorate(c, channel.is_some()))
        .collect();
    let covered = channel.is_some_and(|ch| {
        !calls.is_empty()
            && calls.iter().all(|c| {
                !c.hitl_id.is_empty()
                    && c.grant
                        .as_ref()
                        .is_some_and(|(k, _)| store.contains(agent, ch, k))
            })
    });
    if covered {
        Plan::AutoAllow(calls)
    } else {
        Plan::Ask(calls)
    }
}

/// The `hitl-approval-needed` payload for a planned batch.
pub fn event_payload(
    agent: &str,
    channel: Option<&str>,
    params: &Value,
    calls: &[DecoratedCall],
    auto_allowed: bool,
) -> Value {
    let wire: Vec<Value> = calls
        .iter()
        .map(|c| {
            let mut w = c.wire.clone();
            if let (Some(obj), Some((key, label))) = (w.as_object_mut(), &c.grant) {
                obj.insert(
                    "session_grant".into(),
                    json!({ "key": key, "label": label }),
                );
            }
            w
        })
        .collect();
    json!({
        "agent": agent,
        "channel_id": channel,
        "hitl_id": params.get("hitl_id"),
        "tool_name": params.get("tool_name"),
        "tool_input": params.get("tool_input"),
        "prompt": params.get("prompt"),
        "timeout_ms": params.get("timeout_ms"),
        // P3: the batch. Legacy fields above stay so listeners that only read
        // `agent` keep working.
        "batch_id": params.get("batch_id"),
        "calls": wire,
        "auto_allowed": auto_allowed,
    })
}

/// Remember the operator's "don't ask again this session" for `key`.
#[tauri::command]
pub fn hitl_session_allow(
    state: tauri::State<'_, SessionAllowState>,
    name: String,
    channel_id: String,
    key: String,
) {
    state.0.grant(&name, &channel_id, &key);
}

/// Forget every session answer for `name` (the chat switched agent or channel).
#[tauri::command]
pub fn hitl_session_reset(state: tauri::State<'_, SessionAllowState>, name: String) {
    state.0.reset(&name);
}

#[cfg(test)]
mod tests;

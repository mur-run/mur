//! Two-way chat with an agent over A2A `message/send` (H1), with token streaming.
//!
//! When the agent is running we dial its socket and stream token deltas to the
//! frontend as `chat-delta` Tauri events as they generate; otherwise we fall
//! back to a one-shot ephemeral dial. Multi-turn context is threaded by passing
//! the previous turn's task id back as `context.task_id`.

use mur_channel::ChannelService;
use mur_common::channel::{ChannelActor, ChannelEvent, EventKind};
use mur_core::a2a_dial::{DialMode, dial_message_streaming, dial_method};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter};

/// Maps an agent name to the task id of its current in-flight chat turn, so a
/// Stop action can cancel by an id the Hub already holds. Single source of
/// truth for the cancel path.
#[derive(Default)]
pub struct ChatRegistry(Mutex<HashMap<String, String>>);

impl ChatRegistry {
    pub fn set(&self, agent: &str, task_id: &str) {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(agent.to_string(), task_id.to_string());
    }
    pub fn get(&self, agent: &str) -> Option<String> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(agent)
            .cloned()
    }
    pub fn clear(&self, agent: &str) {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(agent);
    }
}

/// Tauri-managed wrapper around [`ChatRegistry`].
#[derive(Default)]
pub struct ChatRegistryState(pub ChatRegistry);

#[derive(Serialize, Clone)]
pub struct ChatDelta {
    pub agent: String,
    pub text: String,
    /// True for the model's transient reasoning (shown as a "thinking"
    /// indicator), false for the user-facing answer.
    pub thinking: bool,
    /// The turn id the runtime stamps on each delta, so the UI can correlate a
    /// delta to its conversation (empty for agents predating per-connection
    /// routing).
    pub task_id: String,
}

#[derive(Serialize)]
pub struct ChatReply {
    /// The agent's full reply text.
    pub reply: String,
    /// The task id of this turn — pass it back as `context_task_id` next turn.
    pub task_id: String,
    /// Whether the reply was streamed token-by-token (vs a one-shot fallback).
    pub streamed: bool,
    /// Raw `Task.usage` JSON from the runtime (Task 6: `model_ref` +
    /// `route_reason`, among other fields), passed through verbatim so the Hub
    /// chat's decision caption (Task 8) can render without a re-hydrate.
    /// `None` for stub/misconfigured backends or older runtimes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Value>,
}

/// Send `text` to agent `name` and return its reply, streaming token deltas as
/// `chat-delta` events while it generates.
#[tauri::command]
pub async fn agent_chat_send(
    app: AppHandle,
    registry: tauri::State<'_, ChatRegistryState>,
    name: String,
    text: String,
    task_id: String,
    context_task_id: Option<String>,
) -> Result<ChatReply, String> {
    let home = crate::mur_home_path();
    // Register the in-flight id synchronously so a Stop pressed mid-turn can
    // cancel by it. Cleared once the turn returns (below).
    registry.0.set(&name, &task_id);
    let name_clear = name.clone();

    // Capture what we need to persist the exchange AFTER a successful reply,
    // before `text`/`task_id`/`home`/`name` are moved into the dial below. We do
    // NOT persist the user turn up front: writing both turns together, only on
    // success, avoids orphaned user messages (on dial error / empty reply) and
    // duplicate user turns on retry, and pins both halves to one channel.
    let persist_home = home.clone();
    let persist_name = name.clone();
    let persist_user_text = text.clone();
    let user_task_id = task_id.clone();

    let mut params = json!({
        "message": { "role": "user", "parts": [{ "kind": "text", "text": text }] },
        "task_id": task_id,
    });
    if let Some(tid) = context_task_id {
        params["context"] = json!({ "task_id": tid });
    }

    let dialed = tokio::task::spawn_blocking(move || {
        let agent = name.clone();
        let app2 = app.clone();
        // Stream over the running agent's socket; fall back to a one-shot
        // ephemeral dial if it isn't running.
        match dial_message_streaming(
            &home,
            &name,
            params.clone(),
            |delta, thinking, task_id| {
                let _ = app.emit(
                    "chat-delta",
                    ChatDelta {
                        agent: agent.clone(),
                        text: delta.to_string(),
                        thinking,
                        task_id: task_id.to_string(),
                    },
                );
            },
            |hitl_params| {
                let _ = app2.emit(
                    "hitl-approval-needed",
                    serde_json::json!({
                        "agent": name,
                        "hitl_id": hitl_params.get("hitl_id"),
                        "tool_name": hitl_params.get("tool_name"),
                        "tool_input": hitl_params.get("tool_input"),
                        "prompt": hitl_params.get("prompt"),
                        "timeout_ms": hitl_params.get("timeout_ms"),
                    }),
                );
            },
            |_step| {},
        ) {
            Ok(v) => Ok((v, true)),
            Err(e) if e.to_string().contains("is not running") => {
                dial_method(&home, &name, "message/send", params, DialMode::Auto)
                    .map(|v| (v, false))
            }
            Err(e) => Err(e),
        }
    })
    .await
    .map_err(|e| format!("chat task panicked: {e}"))?;

    // The turn is over (success or failure) — clear the in-flight id so a late
    // Stop doesn't cancel an unrelated future turn.
    registry.0.clear(&name_clear);

    let (task, streamed) = dialed.map_err(|e| e.to_string())?;
    let task_id = task
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let reply = task
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|msgs| {
            msgs.iter()
                .rev()
                .find(|m| m.get("role").and_then(Value::as_str) == Some("agent"))
        })
        .map(extract_text)
        .unwrap_or_default();

    if reply.is_empty() {
        return Err("the agent returned no reply".into());
    }
    // Task 6: the runtime attaches `model_ref`/`route_reason` (plus token
    // counts) to `Task.usage`. Pass it through verbatim — both back to the
    // caller (so the just-sent bubble can render the caption immediately) and
    // into the persisted channel event (so it survives a reload).
    let usage = task.get("usage").cloned();
    // Persist the whole exchange atomically into ONE channel (best-effort).
    // `task_id` here is the response task id; the user turn keeps its request id.
    persist_exchange(
        &persist_home,
        &persist_name,
        &persist_user_text,
        Some(&user_task_id),
        &reply,
        Some(&task_id),
        usage.clone(),
    );
    Ok(ChatReply {
        reply,
        task_id,
        streamed,
        usage,
    })
}

/// Cancel agent `name`'s in-flight chat turn by dialing `tasks/cancel` with the
/// id the Hub stored when the send began. No-ops if nothing is in flight; a
/// benign "already finished / not running" error is swallowed (the turn is over).
#[tauri::command]
pub async fn agent_chat_cancel(
    registry: tauri::State<'_, ChatRegistryState>,
    name: String,
) -> Result<(), String> {
    let Some(task_id) = registry.0.get(&name) else {
        return Ok(()); // nothing in flight — nothing to cancel
    };
    let home = crate::mur_home_path();
    let params = json!({ "id": task_id });
    tokio::task::spawn_blocking(move || {
        // Separate connection so it doesn't fight the in-progress streaming read.
        match dial_method(
            &home,
            &name,
            "tasks/cancel",
            params,
            DialMode::RequireRunning,
        ) {
            Ok(_) => Ok(()),
            // The turn may have just finished, or the agent stopped — benign for
            // a cancel. Surface only genuine/unexpected failures.
            Err(e)
                if {
                    let s = e.to_string();
                    s.contains("not cancellable")
                        || s.contains("not found")
                        || s.contains("is not running")
                } =>
            {
                Ok(())
            }
            Err(e) => Err(e.to_string()),
        }
    })
    .await
    .map_err(|e| format!("cancel task panicked: {e}"))?
}

/// Concatenate the text parts of an A2A message.
fn extract_text(message: &Value) -> String {
    message
        .get("parts")
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .filter_map(|p| p.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

// ─── Channel persistence ───────────────────────────────────────────────────

/// Persist one user→agent exchange into the agent's channel, resolving the
/// channel ONCE so both halves land together (never split across channels if a
/// newer channel appears mid-turn). Best-effort: failures are logged, never
/// surfaced to the chat. The channel is created here on the first real exchange,
/// so a failed/empty turn writes nothing (no orphaned user message).
#[allow(clippy::too_many_arguments)]
fn persist_exchange(
    home: &std::path::Path,
    agent: &str,
    user_text: &str,
    user_task_id: Option<&str>,
    agent_text: &str,
    agent_task_id: Option<&str>,
    agent_usage: Option<Value>,
) {
    let res = (|| -> anyhow::Result<()> {
        let svc = ChannelService::open(home)?;
        let id = match svc.latest_for_agent(agent)? {
            Some(id) => id,
            None => svc.create_for_agent(agent)?.id,
        };
        svc.append_message(
            &id,
            ChannelActor::local_human(),
            EventKind::Message,
            user_text,
            user_task_id,
        )?;
        // Build the agent payload by hand (rather than `append_message`) so we
        // can attach the turn's usage (Task 6: `model_ref`/`route_reason`) when
        // present — the Hub chat decision caption (Task 8) reads it back on
        // reload via `channel_load`.
        let mut agent_payload = serde_json::json!({ "text": agent_text });
        if let Some(t) = agent_task_id {
            agent_payload["task_id"] = serde_json::Value::String(t.to_string());
        }
        if let Some(u) = agent_usage {
            agent_payload["usage"] = u;
        }
        svc.append(
            &id,
            ChannelActor::Agent {
                id: agent.to_string(),
            },
            EventKind::Message,
            agent_payload,
            None,
        )?;
        Ok(())
    })();
    if let Err(e) = res {
        tracing::warn!("channel persist failed for {agent}: {e:#}");
    }
}

/// Tauri command: load the agent's latest channel events for hydration.
#[tauri::command]
pub async fn channel_load(name: String) -> Result<Vec<ChannelEvent>, String> {
    let home = crate::mur_home_path();
    let svc = ChannelService::open(&home).map_err(|e| e.to_string())?;
    let Some(id) = svc.latest_for_agent(&name).map_err(|e| e.to_string())? else {
        return Ok(vec![]);
    };
    svc.load_events(&id).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_set_get_clear() {
        let reg = ChatRegistry::default();
        assert_eq!(reg.get("alice"), None);
        reg.set("alice", "task-1");
        assert_eq!(reg.get("alice").as_deref(), Some("task-1"));
        reg.clear("alice");
        assert_eq!(reg.get("alice"), None);
    }

    #[test]
    fn cancel_lookup_is_none_when_absent() {
        let reg = ChatRegistry::default();
        // No id registered for "ghost" → cancel must no-op (None lookup).
        assert_eq!(reg.get("ghost"), None);
    }
}

#[cfg(test)]
mod channel_tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn persist_exchange_writes_both_turns_to_one_channel() {
        let tmp = TempDir::new().unwrap();
        persist_exchange(
            tmp.path(),
            "qa",
            "the question",
            Some("u-1"),
            "the answer",
            Some("a-1"),
            Some(serde_json::json!({"model_ref": "haiku", "route_reason": "smart-background"})),
        );
        let svc = ChannelService::open(tmp.path()).unwrap();
        let id = svc.latest_for_agent("qa").unwrap().expect("channel");
        let evs = svc.load_events(&id).unwrap();
        // Both halves landed in the same channel, in order.
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[0].payload["text"], "the question");
        assert_eq!(evs[1].payload["text"], "the answer");
        // Usage (Task 6/8: model_ref + route_reason) rides along on the agent
        // half only — the caption reads it back via `channel_load`.
        assert_eq!(evs[1].payload["usage"]["model_ref"], "haiku");
        assert_eq!(evs[1].payload["usage"]["route_reason"], "smart-background");
        assert!(evs[0].payload.get("usage").is_none());
        // A second exchange appends to the SAME channel, not a new one.
        persist_exchange(tmp.path(), "qa", "q2", None, "a2", None, None);
        assert_eq!(svc.list(10).unwrap().len(), 1);
        assert_eq!(svc.load_events(&id).unwrap().len(), 4);
    }
}

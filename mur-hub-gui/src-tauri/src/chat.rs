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

    // Persist the user turn into the agent's channel (best-effort). Done before
    // `text`/`home`/`name` are moved into the dial closure below; clones of
    // `home`/`name` are kept to persist the agent turn after the dial returns.
    persist_turn(&home, &name, "user", &text, Some(&task_id));
    let persist_home = home.clone();
    let persist_name = name.clone();

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
    // Persist the agent turn (best-effort). `task_id` here is the response task
    // id; `reply` is borrowed before both are moved into `ChatReply`.
    persist_turn(
        &persist_home,
        &persist_name,
        "agent",
        &reply,
        Some(&task_id),
    );
    Ok(ChatReply {
        reply,
        task_id,
        streamed,
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

/// Resolve the channel for an agent (latest, or create one), returning its id.
fn channel_for_agent(home: &std::path::Path, agent: &str) -> anyhow::Result<String> {
    let svc = ChannelService::open(home)?;
    if let Some(id) = svc.latest_for_agent(agent)? {
        return Ok(id);
    }
    Ok(svc.create_for_agent(agent)?.id)
}

/// Persist one turn into the agent's channel. `role` ∈ {"user","agent"}.
fn persist_turn(home: &std::path::Path, agent: &str, role: &str, text: &str, task_id: Option<&str>) {
    let res = (|| -> anyhow::Result<()> {
        let svc = ChannelService::open(home)?;
        let id = channel_for_agent(home, agent)?;
        let actor = match role {
            "agent" => ChannelActor::Agent {
                id: agent.to_string(),
            },
            _ => ChannelActor::local_human(),
        };
        svc.append_message(&id, actor, EventKind::Message, text, task_id)?;
        Ok(())
    })();
    if let Err(e) = res {
        eprintln!("channel persist failed for {agent}: {e:#}");
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
    fn persist_turn_writes_channel_event() {
        let tmp = TempDir::new().unwrap();
        persist_turn(tmp.path(), "qa", "user", "hello hub", Some("t-1"));
        let svc = ChannelService::open(tmp.path()).unwrap();
        let id = svc.latest_for_agent("qa").unwrap().expect("channel");
        let evs = svc.load_events(&id).unwrap();
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].payload["text"], "hello hub");
    }
}

//! Two-way chat with an agent over A2A `message/send` (H1), with token streaming.
//!
//! When the agent is running we dial its socket and stream token deltas to the
//! frontend as `chat-delta` Tauri events as they generate; otherwise we fall
//! back to a one-shot ephemeral dial. Multi-turn context is threaded by passing
//! the previous turn's task id back as `context.task_id`.

use mur_core::a2a_dial::{DialMode, dial_message_streaming, dial_method};
use serde::Serialize;
use serde_json::{Value, json};
use tauri::{AppHandle, Emitter};

#[derive(Serialize, Clone)]
pub struct ChatDelta {
    pub agent: String,
    pub text: String,
    /// True for the model's transient reasoning (shown as a "thinking"
    /// indicator), false for the user-facing answer.
    pub thinking: bool,
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
    name: String,
    text: String,
    context_task_id: Option<String>,
) -> Result<ChatReply, String> {
    let home = crate::mur_home_path();
    let mut params = json!({
        "message": { "role": "user", "parts": [{ "kind": "text", "text": text }] }
    });
    if let Some(tid) = context_task_id {
        params["context"] = json!({ "task_id": tid });
    }

    let result = tokio::task::spawn_blocking(move || {
        let agent = name.clone();
        // Stream over the running agent's socket; fall back to a one-shot
        // ephemeral dial if it isn't running.
        match dial_message_streaming(&home, &name, params.clone(), |delta, thinking| {
            let _ = app.emit(
                "chat-delta",
                ChatDelta {
                    agent: agent.clone(),
                    text: delta.to_string(),
                    thinking,
                },
            );
        }, |_hitl| {}) {
            Ok(v) => Ok((v, true)),
            Err(e) if e.to_string().contains("is not running") => {
                dial_method(&home, &name, "message/send", params, DialMode::Auto)
                    .map(|v| (v, false))
            }
            Err(e) => Err(e),
        }
    })
    .await
    .map_err(|e| format!("chat task panicked: {e}"))?
    .map_err(|e| e.to_string())?;

    let (task, streamed) = result;
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
    Ok(ChatReply {
        reply,
        task_id,
        streamed,
    })
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

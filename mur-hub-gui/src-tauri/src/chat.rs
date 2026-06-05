//! Two-way chat with an agent over A2A `message/send` (H1).
//!
//! Uses `mur_core::a2a_dial` in `Auto` mode: if the agent is already running
//! the message goes over its Unix socket (warm model + multi-turn continuity);
//! otherwise an ephemeral runtime is spawned for the turn. Multi-turn context
//! is threaded by passing the previous task's id back as `context.task_id`.

use mur_core::a2a_dial::{DialMode, dial_method};
use serde::Serialize;
use serde_json::{Value, json};

#[derive(Serialize)]
pub struct ChatReply {
    /// The agent's reply text.
    pub reply: String,
    /// The task id of this turn — pass it back as `context_task_id` on the next
    /// turn to continue the same conversation.
    pub task_id: String,
}

/// Send `text` to agent `name` and return its reply.
#[tauri::command]
pub async fn agent_chat_send(
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

    // dial_method does blocking socket / process I/O — keep it off the async
    // reactor so the UI stays responsive while the model thinks.
    let result = tokio::task::spawn_blocking(move || {
        dial_method(&home, &name, "message/send", params, DialMode::Auto)
    })
    .await
    .map_err(|e| format!("chat task panicked: {e}"))?
    .map_err(|e| e.to_string())?;

    let task_id = result
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let reply = result
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
    Ok(ChatReply { reply, task_id })
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

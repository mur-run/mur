//! `RichMessage` → Anthropic `/v1/messages` wire shape.

use serde_json::json;

use crate::llm::RichMessage;

/// Convert `RichMessage` list to Anthropic wire format.
/// Returns `(system_text, conversation_messages, agent_text_for_stream)`.
/// Agent_text is the last assistant text for streaming (unused in non-streaming).
pub(super) fn rich_messages_to_anthropic(
    msgs: &[RichMessage],
) -> (Option<String>, Vec<serde_json::Value>, Option<String>) {
    /// Push a `{role, content}` entry, merging into the last entry when roles
    /// match (Anthropic 400s on consecutive same-role messages).
    fn push_coalesced(convo: &mut Vec<serde_json::Value>, role: &str, content: serde_json::Value) {
        /// Normalize message content to an array of content blocks.
        fn blocks(content: serde_json::Value) -> Vec<serde_json::Value> {
            match content {
                serde_json::Value::Array(a) => a,
                serde_json::Value::String(s) => vec![json!({"type": "text", "text": s})],
                other => vec![other],
            }
        }
        if let Some(last) = convo.last_mut()
            && last["role"] == role
        {
            let mut merged = blocks(last["content"].take());
            merged.extend(blocks(content));
            last["content"] = json!(merged);
            return;
        }
        convo.push(json!({"role": role, "content": content}));
    }

    let mut system_chunks: Vec<String> = Vec::new();
    let mut convo: Vec<serde_json::Value> = Vec::new();

    for m in msgs {
        match m {
            RichMessage::Text { role, content } => {
                if role == "system" {
                    system_chunks.push(content.clone());
                } else {
                    let r = if role == "agent" {
                        "assistant"
                    } else {
                        role.as_str()
                    };
                    push_coalesced(&mut convo, r, json!(content));
                }
            }
            RichMessage::ToolUse { text, calls } => {
                let mut parts: Vec<serde_json::Value> = Vec::new();
                if let Some(t) = text
                    && !t.is_empty()
                {
                    parts.push(json!({"type": "text", "text": t}));
                }
                for c in calls {
                    parts.push(json!({
                        "type": "tool_use",
                        "id": c.call_id,
                        "name": c.tool_name,
                        "input": c.input,
                    }));
                }
                push_coalesced(&mut convo, "assistant", json!(parts));
            }
            RichMessage::ToolResults { results } => {
                let parts: Vec<serde_json::Value> = results
                    .iter()
                    .map(|r| {
                        // `content` stays a bare string when there is no image,
                        // so the overwhelmingly common shape is byte-identical
                        // to what this adapter always sent — a tool result that
                        // gained an empty `images` vec must not change the
                        // request (and must not break the prompt cache).
                        let content = if r.images.is_empty() {
                            json!(r.content)
                        } else {
                            let mut blocks = vec![json!({
                                "type": "text",
                                "text": r.content,
                            })];
                            blocks.extend(r.images.iter().map(|img| {
                                json!({
                                    "type": "image",
                                    "source": {
                                        "type": "base64",
                                        "media_type": img.media_type,
                                        "data": img.data,
                                    },
                                })
                            }));
                            json!(blocks)
                        };
                        json!({
                            "type": "tool_result",
                            "tool_use_id": r.call_id,
                            "content": content,
                            "is_error": r.is_error,
                        })
                    })
                    .collect();
                push_coalesced(&mut convo, "user", json!(parts));
            }
            RichMessage::ImageText {
                role,
                media_type,
                data,
                text,
            } => {
                let r = if role == "agent" {
                    "assistant"
                } else {
                    role.as_str()
                };
                // Image block first (Anthropic's recommended ordering), then
                // the caption — skipped when empty so an image-only paste works.
                let mut parts = vec![json!({
                    "type": "image",
                    "source": {"type": "base64", "media_type": media_type, "data": data},
                })];
                if !text.is_empty() {
                    parts.push(json!({"type": "text", "text": text}));
                }
                push_coalesced(&mut convo, r, json!(parts));
            }
            RichMessage::TurnLedger { turn, memory } => {
                push_coalesced(
                    &mut convo,
                    "user",
                    json!(crate::turn_ledger::render_memory(*turn, memory)),
                );
            }
        }
    }

    let system = if system_chunks.is_empty() {
        None
    } else {
        Some(system_chunks.join("\n\n"))
    };
    (system, convo, None)
}

/// Put a prompt-cache breakpoint on the last content block of the last
/// message. The API walks back from a breakpoint to the longest cached
/// prefix, so one trailing marker per request is enough for every earlier
/// call of the same turn to come back as a cache read; a string body becomes
/// a one-block array because `cache_control` lives on blocks, not messages.
pub(super) fn mark_cache_breakpoint(convo: &mut [serde_json::Value]) {
    let Some(last) = convo.last_mut() else { return };
    let content = &mut last["content"];
    if let Some(s) = content.as_str().map(str::to_owned) {
        *content = json!([{"type": "text", "text": s}]);
    }
    if let Some(block) = content.as_array_mut().and_then(|a| a.last_mut()) {
        block["cache_control"] = json!({"type": "ephemeral"});
    }
}

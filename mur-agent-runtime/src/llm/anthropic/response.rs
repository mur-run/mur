//! Anthropic response handling: the `usage` object, the SSE stream
//! accumulator, and the non-streaming body parser.

use crate::llm::{LlmError, LlmResponse, StopReason, StreamDelta, ToolCallResult};

/// The `usage` object of a response. Every field is optional on the wire:
/// a non-streaming body carries them all, `message_start` carries the
/// prompt side, `message_delta` carries `output_tokens` and on newer API
/// versions repeats the rest — so `merge` only overwrites what is present.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct Usage {
    pub(super) input_tokens: u64,
    pub(super) cache_creation_input_tokens: u64,
    pub(super) cache_read_input_tokens: u64,
    pub(super) output_tokens: u64,
}

impl Usage {
    pub(super) fn merge(&mut self, u: &serde_json::Value) {
        let take = |k: &str, slot: &mut u64| {
            if let Some(n) = u[k].as_u64() {
                *slot = n;
            }
        };
        take("input_tokens", &mut self.input_tokens);
        take(
            "cache_creation_input_tokens",
            &mut self.cache_creation_input_tokens,
        );
        take("cache_read_input_tokens", &mut self.cache_read_input_tokens);
        take("output_tokens", &mut self.output_tokens);
    }

    /// `input_tokens` on the wire is only the uncached remainder; the
    /// response reports the whole prompt (see `LlmResponse::input_tokens`).
    pub(super) fn into_response(
        self,
        text: String,
        model: String,
        tool_calls: Vec<crate::llm::ToolCallResult>,
        stop_reason: StopReason,
    ) -> LlmResponse {
        LlmResponse {
            text,
            input_tokens: self.input_tokens
                + self.cache_creation_input_tokens
                + self.cache_read_input_tokens,
            output_tokens: self.output_tokens,
            cache_creation_input_tokens: self.cache_creation_input_tokens,
            cache_read_input_tokens: self.cache_read_input_tokens,
            model,
            tool_calls,
            stop_reason,
            registry_key: None,
        }
    }
}

/// Accumulator for an Anthropic SSE response while it streams.
pub(super) struct StreamAccum {
    pub(super) text: String,
    pub(super) usage: Usage,
    pub(super) tool_calls: Vec<crate::llm::ToolCallResult>,
    pub(super) stop_reason: StopReason,
    /// The in-progress tool_use block: (id, name, partial-JSON args buffer).
    pub(super) cur_tool: Option<(String, String, String)>,
    /// Evidence for an empty response, so the error can say WHY it was
    /// empty: a model that chose `end_turn` with nothing to say reads very
    /// differently from a stream cut short or an in-band `error` event.
    pub(super) diag: StreamDiag,
}

/// What the stream looked like, kept only to explain an empty result.
#[derive(Default)]
pub(super) struct StreamDiag {
    /// `stop_reason` exactly as sent; `refusal` and friends collapse to
    /// `EndTurn` in `StreamAccum::stop_reason`.
    pub(super) raw_stop_reason: Option<String>,
    /// The terminal `message_stop` event arrived.
    pub(super) message_stop: bool,
    /// `content_block_start` events seen, of any type.
    pub(super) blocks: usize,
    /// `content_block.type` of each block, in order (`?` if missing), so an
    /// empty reply with `blocks>0` says what the tokens went into.
    pub(super) block_types: Vec<String>,
    /// Distinct `delta.type`s that no arm handles and were dropped.
    pub(super) other_deltas: Vec<String>,
    pub(super) thinking_chars: usize,
    /// An in-band `{"type":"error"}` event: `<type>: <message>`.
    pub(super) error: Option<String>,
}

impl Default for StreamAccum {
    fn default() -> Self {
        Self {
            text: String::new(),
            usage: Usage::default(),
            tool_calls: Vec::new(),
            stop_reason: StopReason::EndTurn,
            cur_tool: None,
            diag: StreamDiag::default(),
        }
    }
}

/// Apply one parsed SSE `data:` event to `acc`. Returns a `StreamDelta` to
/// forward to the sink iff this event carried answer text or reasoning.
/// Mirrors the non-stream `parse_response_body` for tool_use + stop_reason.
pub(super) fn apply_sse_event(acc: &mut StreamAccum, v: &serde_json::Value) -> Option<StreamDelta> {
    match v["type"].as_str() {
        Some("content_block_start") => {
            acc.diag.blocks += 1;
            let cb = &v["content_block"];
            acc.diag
                .block_types
                .push(cb["type"].as_str().unwrap_or("?").to_string());
            if cb["type"].as_str() == Some("tool_use") {
                acc.cur_tool = Some((
                    cb["id"].as_str().unwrap_or("").to_string(),
                    cb["name"].as_str().unwrap_or("").to_string(),
                    String::new(),
                ));
            } else {
                acc.cur_tool = None;
            }
            None
        }
        Some("content_block_delta") => {
            let d = &v["delta"];
            match d["type"].as_str() {
                Some("text_delta") => {
                    let t = d["text"].as_str().unwrap_or("");
                    if t.is_empty() {
                        return None;
                    }
                    acc.text.push_str(t);
                    Some(StreamDelta {
                        text: t.to_string(),
                        thinking: false,
                    })
                }
                Some("thinking_delta") => {
                    let t = d["thinking"].as_str().unwrap_or("");
                    if t.is_empty() {
                        return None;
                    }
                    acc.diag.thinking_chars += t.chars().count();
                    Some(StreamDelta {
                        text: t.to_string(),
                        thinking: true,
                    })
                }
                Some("input_json_delta") => {
                    match (acc.cur_tool.as_mut(), d["partial_json"].as_str()) {
                        (Some((_, _, buf)), Some(pj)) => buf.push_str(pj),
                        (None, Some(_)) => {
                            tracing::warn!(
                                "anthropic stream: input_json_delta with no open tool_use block — dropping args fragment"
                            );
                        }
                        _ => {}
                    }
                    None
                }
                other => {
                    let t = other.unwrap_or("?");
                    if !acc.diag.other_deltas.iter().any(|x| x == t) {
                        acc.diag.other_deltas.push(t.to_string());
                    }
                    None
                }
            }
        }
        Some("content_block_stop") => {
            if let Some((id, name, buf)) = acc.cur_tool.take() {
                let input = if buf.trim().is_empty() {
                    serde_json::json!({})
                } else {
                    serde_json::from_str(&buf).unwrap_or_else(|_| serde_json::json!({}))
                };
                acc.tool_calls.push(ToolCallResult {
                    call_id: id,
                    tool_name: name,
                    input,
                });
            }
            None
        }
        Some("message_start") => {
            acc.usage.merge(&v["message"]["usage"]);
            None
        }
        Some("message_delta") => {
            if let Some(sr) = v["delta"]["stop_reason"].as_str() {
                acc.diag.raw_stop_reason = Some(sr.to_string());
                acc.stop_reason = match sr {
                    "tool_use" => StopReason::ToolUse,
                    "max_tokens" => StopReason::MaxTokens,
                    _ => StopReason::EndTurn,
                };
            }
            acc.usage.merge(&v["usage"]);
            None
        }
        Some("message_stop") => {
            acc.diag.message_stop = true;
            None
        }
        Some("error") => {
            let e = &v["error"];
            acc.diag.error = Some(format!(
                "{}: {}",
                e["type"].as_str().unwrap_or("unknown"),
                e["message"].as_str().unwrap_or("")
            ));
            None
        }
        _ => None,
    }
}

/// `empty streamed response (…)` with the evidence of why. The prefix is
/// load-bearing: task_runner matches on it to decide retries.
pub(super) fn empty_stream_error(acc: &StreamAccum) -> String {
    let d = &acc.diag;
    let mut msg = format!(
        "empty streamed response (stop_reason={}, message_stop={}, output_tokens={}, blocks={}, thinking_chars={}",
        d.raw_stop_reason.as_deref().unwrap_or("none"),
        d.message_stop,
        acc.usage.output_tokens,
        d.blocks,
        d.thinking_chars,
    );
    if !d.block_types.is_empty() {
        msg.push_str(", block_types=");
        msg.push_str(&d.block_types.join(","));
    }
    if !d.other_deltas.is_empty() {
        msg.push_str(", other_deltas=");
        msg.push_str(&d.other_deltas.join(","));
    }
    if let Some(e) = &d.error {
        msg.push_str(", error=");
        msg.push_str(e);
    }
    msg.push(')');
    msg
}

/// Turn a fully-drained `StreamAccum` into the final result. A tool-only
/// response legitimately has empty text — only error when BOTH answer text
/// and tool calls are empty. A response that hit the max_tokens ceiling while
/// still inside a thinking block (no text or tool_use ever started) is also
/// legitimate, not malformed — let it through so task_runner's MaxTokens
/// retry path can nudge the model toward a shorter answer instead of
/// surfacing a raw protocol error.
/// `interrupted` = the stream stopped sending rather than ending (#1287).
///
/// Nothing special is needed to protect a half-emitted tool call: `cur_tool`
/// is committed to `acc.tool_calls` only at `content_block_stop`, so a call
/// interrupted mid-arguments is already absent. The model is then told plainly
/// that its call did not happen, instead of being handed arguments it never
/// finished choosing.
pub(super) fn finish_stream(
    acc: StreamAccum,
    model: String,
    interrupted: bool,
) -> Result<LlmResponse, LlmError> {
    if acc.text.is_empty() && acc.tool_calls.is_empty() && acc.stop_reason != StopReason::MaxTokens
    {
        // An interruption with nothing assembled is a timeout, not a malformed
        // response: `classify` makes `Timeout` RetryThenAdvance and
        // `InvalidResponse` Stop, and nothing reached the sink to duplicate.
        if interrupted {
            tracing::warn!(
                blocks = acc.diag.blocks,
                "anthropic stream went idle before any content; reporting a timeout"
            );
            return Err(LlmError::Timeout);
        }
        let msg = empty_stream_error(&acc);
        // Debug, not warn: the provider cannot tell a failure from the model
        // deliberately ending its turn after `suggest_replies` (the normal
        // case). The task runner decides which it is and logs the real
        // failures at warn with this same message attached.
        tracing::debug!(model = %model, "{msg}");
        return Err(LlmError::InvalidResponse(msg));
    }
    let stop_reason = if interrupted {
        StopReason::Interrupted
    } else {
        acc.stop_reason
    };
    Ok(acc
        .usage
        .into_response(acc.text, model, acc.tool_calls, stop_reason))
}

pub(super) fn parse_response_body(
    v: &serde_json::Value,
) -> Result<
    (
        String,
        Vec<crate::llm::ToolCallResult>,
        crate::llm::StopReason,
    ),
    LlmError,
> {
    use crate::llm::{StopReason, ToolCallResult};

    let content = v["content"]
        .as_array()
        .ok_or_else(|| LlmError::InvalidResponse("missing content array".into()))?;

    let text = content
        .iter()
        .filter_map(|b| {
            if b["type"].as_str() == Some("text") {
                b["text"].as_str().map(str::to_string)
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("");

    let tool_calls: Vec<ToolCallResult> = content
        .iter()
        .filter(|b| b["type"].as_str() == Some("tool_use"))
        .map(|b| ToolCallResult {
            call_id: b["id"].as_str().unwrap_or("").to_string(),
            tool_name: b["name"].as_str().unwrap_or("").to_string(),
            input: b["input"].clone(),
        })
        .collect();

    let stop_reason = match v["stop_reason"].as_str() {
        Some("tool_use") => StopReason::ToolUse,
        Some("max_tokens") => StopReason::MaxTokens,
        _ => StopReason::EndTurn,
    };

    Ok((text, tool_calls, stop_reason))
}

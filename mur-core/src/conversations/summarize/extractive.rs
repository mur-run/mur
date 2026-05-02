//! Per-chunk LLM span extraction. See spec §4.2.
//!
//! For each chunk, prompt Ollama for a JSON array of {role, conv_id,
//! line_hint, text} spans. Validate each span:
//!   - text is a verbatim substring of a source message (Jaro-Winkler ≥ 0.95)
//!   - line_hint within chunk.span_range
//!   - role matches source message role
//!
//! Invalid spans silently dropped. Failure degrades to zero spans.

use anyhow::Result;
use mur_common::{Content, Message, Role, Source};
use serde::{Deserialize, Serialize};
use tracing::warn;

use super::super::backend::{ChatBackend, ChatRequest};
use super::chunker::Chunk;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ExtractiveSpan {
    pub role: Role,
    pub conv_id: String,
    pub line_hint: u32,
    pub text: String,
    #[serde(skip)]
    pub src: Source, // resolved from source message during validation
}

#[derive(Debug, Clone, Deserialize)]
struct LlmSpan {
    role: String,
    conv_id: String,
    line_hint: u32,
    text: String,
}

pub async fn extract_chunk(
    backend: &dyn ChatBackend,
    model: &str,
    chunk: &Chunk,
    day_msgs: &[Message],
) -> Result<Vec<ExtractiveSpan>> {
    let prompt = render_prompt(chunk);
    let resp = backend
        .generate(ChatRequest {
            model,
            user: &prompt,
            system: None,
            max_tokens: 1024,
            temperature: Some(0.0),
            stop: vec![],
            cache_system: false,
            cache_user_prefix: None,
        })
        .await;
    let body = match resp {
        Ok(r) => r.text,
        Err(e) => {
            warn!("extractive LLM call failed: {e:#}");
            return Ok(Vec::new());
        }
    };

    let raw: Vec<LlmSpan> = match parse_json_array(&body) {
        Some(v) => v,
        None => {
            warn!("extractive output not a JSON array; returning zero spans");
            return Ok(Vec::new());
        }
    };

    Ok(raw
        .into_iter()
        .filter_map(|s| validate(s, chunk, day_msgs))
        .collect())
}

fn render_prompt(chunk: &Chunk) -> String {
    let mut body = String::new();
    let (start_line, end_line) = chunk.span_range;
    body.push_str(
        "You are reviewing one conversation day for a technical developer's personal archive. \
         Extract the 1-3 most informative spans from this excerpt.\n\n\
         A span is quote-worthy if it:\n\
         - states a decision the user made (\"we'll use X over Y because...\")\n\
         - records a concrete error or failure that shaped subsequent work\n\
         - captures a new idea, technique, or reference the user hadn't seen before\n\
         - quotes an important external fact (API response, spec excerpt, doc)\n\n\
         A span is NOT quote-worthy if it is:\n\
         - boilerplate/greeting/filler\n\
         - tool-result body already citeable by path\n\
         - restated from an earlier span\n\n\
         Output format: JSON array. Each span is {role, conv_id, line_hint, text}.\n\
           - role: one of \"user\" | \"assistant\" | \"system\" | \"tool\"\n\
           - conv_id: the conv value from the source message\n\
           - line_hint: integer line number within the day's raw JSONL\n\
           - text: verbatim quote, 20-400 chars\n\n\
         If the excerpt has nothing quote-worthy, return [].\n\n",
    );
    body.push_str(&format!(
        "Excerpt ({} messages, lines {}..{}):\n",
        chunk.messages.len(),
        start_line,
        end_line
    ));
    for (i, m) in chunk.messages.iter().enumerate() {
        let line_no = start_line + i;
        let role = format!("{:?}", m.role).to_lowercase();
        let text = content_preview(m);
        body.push_str(&format!(
            "L{} [{}] {}/{} ({}): {}\n",
            line_no,
            m.ts.format("%H:%M:%S"),
            m.src.file_prefix(),
            m.conv,
            role,
            text,
        ));
    }
    body
}

fn content_preview(m: &Message) -> String {
    match &m.content {
        Content::Text { value } => value.clone(),
        Content::ToolRef {
            desc, bytes, path, ..
        } => {
            format!("[tool_ref:{} ({}B) @ {}]", desc, bytes, path)
        }
        Content::ImageRef { desc, path, .. } => format!("[image_ref:{} @ {}]", desc, path),
    }
}

fn parse_json_array(body: &str) -> Option<Vec<LlmSpan>> {
    // Tolerate fences / surrounding prose: find first `[` and last `]`.
    let start = body.find('[')?;
    let end = body.rfind(']')?;
    if end <= start {
        return None;
    }
    let slice = &body[start..=end];
    serde_json::from_str::<Vec<LlmSpan>>(slice).ok()
}

fn validate(raw: LlmSpan, chunk: &Chunk, day_msgs: &[Message]) -> Option<ExtractiveSpan> {
    // line_hint within chunk
    let (s, e) = chunk.span_range;
    if raw.line_hint < s as u32 || raw.line_hint > e as u32 {
        return None;
    }
    let role = parse_role(&raw.role)?;
    // Find the source message at line_hint (1-based → 0-based index)
    let idx = raw.line_hint as usize - 1;
    let source_msg = day_msgs.get(idx)?;
    if source_msg.role != role {
        return None;
    }
    if source_msg.conv != raw.conv_id {
        return None;
    }
    // Verbatim check with Jaro-Winkler ≥ 0.95 against source message text
    let source_text = content_preview(source_msg);
    let similarity = jaro_winkler(&raw.text, &source_text);
    if similarity < 0.95 {
        return None;
    }
    Some(ExtractiveSpan {
        role,
        conv_id: raw.conv_id,
        line_hint: raw.line_hint,
        text: raw.text,
        src: source_msg.src,
    })
}

fn parse_role(s: &str) -> Option<Role> {
    match s.to_ascii_lowercase().as_str() {
        "user" => Some(Role::User),
        "assistant" => Some(Role::Assistant),
        "system" => Some(Role::System),
        "tool" => Some(Role::Tool),
        _ => None,
    }
}

/// Small Jaro-Winkler implementation (no extra dep). Char-based, case-sensitive.
fn jaro_winkler(a: &str, b: &str) -> f64 {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let match_distance = (a.len().max(b.len()) / 2).saturating_sub(1);

    let mut a_matched = vec![false; a.len()];
    let mut b_matched = vec![false; b.len()];
    let mut matches = 0usize;

    for i in 0..a.len() {
        let lo = i.saturating_sub(match_distance);
        let hi = (i + match_distance + 1).min(b.len());
        for j in lo..hi {
            if b_matched[j] {
                continue;
            }
            if a[i] == b[j] {
                a_matched[i] = true;
                b_matched[j] = true;
                matches += 1;
                break;
            }
        }
    }
    if matches == 0 {
        return 0.0;
    }
    // transpositions
    let mut k = 0usize;
    let mut transpositions = 0usize;
    for i in 0..a.len() {
        if !a_matched[i] {
            continue;
        }
        while !b_matched[k] {
            k += 1;
        }
        if a[i] != b[k] {
            transpositions += 1;
        }
        k += 1;
    }
    let m = matches as f64;
    let jaro =
        (m / a.len() as f64 + m / b.len() as f64 + (m - transpositions as f64 / 2.0) / m) / 3.0;
    // Winkler common-prefix boost up to 4 chars, p=0.1
    let prefix = a
        .iter()
        .zip(b.iter())
        .take(4)
        .take_while(|(x, y)| x == y)
        .count() as f64;
    jaro + prefix * 0.1 * (1.0 - jaro)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversations::ENV_LOCK;
    use chrono::TimeZone;

    fn mk(ts_min: u32, conv: &str, text: &str, role: Role) -> Message {
        Message {
            v: 1,
            ts: chrono::Utc
                .with_ymd_and_hms(2026, 4, 19, 10, ts_min, 0)
                .unwrap(),
            src: Source::ClaudeCode,
            conv: conv.into(),
            role,
            content: Content::Text { value: text.into() },
            meta: serde_json::Value::Null,
            refs: vec![],
        }
    }

    #[test]
    fn json_array_parsed_with_surrounding_prose() {
        let body = r#"Here are the spans:
```json
[
  {"role":"user","conv_id":"c1","line_hint":1,"text":"hi"}
]
```
That's all."#;
        let v = parse_json_array(body).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].text, "hi");
    }

    #[test]
    fn validate_rejects_out_of_range_line_hint() {
        let msgs = vec![mk(0, "c1", "hello", Role::User)];
        let chunk = Chunk {
            messages: msgs.clone(),
            token_count: 10,
            span_range: (1, 1),
        };
        let raw = LlmSpan {
            role: "user".into(),
            conv_id: "c1".into(),
            line_hint: 99,
            text: "hello".into(),
        };
        assert!(validate(raw, &chunk, &msgs).is_none());
    }

    #[test]
    fn validate_rejects_role_mismatch() {
        let msgs = vec![mk(0, "c1", "hello", Role::User)];
        let chunk = Chunk {
            messages: msgs.clone(),
            token_count: 10,
            span_range: (1, 1),
        };
        let raw = LlmSpan {
            role: "assistant".into(),
            conv_id: "c1".into(),
            line_hint: 1,
            text: "hello".into(),
        };
        assert!(validate(raw, &chunk, &msgs).is_none());
    }

    #[test]
    fn validate_rejects_paraphrase() {
        let msgs = vec![mk(
            0,
            "c1",
            "cargo build failed with error E0001",
            Role::User,
        )];
        let chunk = Chunk {
            messages: msgs.clone(),
            token_count: 10,
            span_range: (1, 1),
        };
        let raw = LlmSpan {
            role: "user".into(),
            conv_id: "c1".into(),
            line_hint: 1,
            text: "build failed".into(),
        };
        assert!(validate(raw, &chunk, &msgs).is_none());
    }

    #[test]
    fn validate_accepts_verbatim() {
        let msgs = vec![mk(
            0,
            "c1",
            "cargo build failed with error E0001",
            Role::User,
        )];
        let chunk = Chunk {
            messages: msgs.clone(),
            token_count: 10,
            span_range: (1, 1),
        };
        let raw = LlmSpan {
            role: "user".into(),
            conv_id: "c1".into(),
            line_hint: 1,
            text: "cargo build failed with error E0001".into(),
        };
        assert!(validate(raw, &chunk, &msgs).is_some());
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn mock_backend_extracts_one_span() {
        use crate::conversations::backend::mock::MockBackend;
        let _env_guard = ENV_LOCK.lock().unwrap();
        // MockBackend reuses ollama::mock_generate's pattern dispatch;
        // the legacy MUR_OLLAMA_MOCK env var still selects it via factory.
        let backend = MockBackend::new();
        let msgs = vec![mk(0, "mock", "mock extractive span", Role::User)];
        let chunk = Chunk {
            messages: msgs.clone(),
            token_count: 10,
            span_range: (1, 1),
        };
        let spans = extract_chunk(&backend, "qwen3:14b", &chunk, &msgs)
            .await
            .unwrap();
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].text, "mock extractive span");
    }
}

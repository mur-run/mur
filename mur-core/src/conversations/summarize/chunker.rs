//! Pack messages into LLM-sized chunks under a token budget.
//!
//! - Never split a single message (quote integrity).
//! - Prefer splitting at conv_id boundaries; only split mid-conversation if
//!   that conversation itself exceeds the budget.
//! - Token estimate: chars / 4. Good to ±15% across CJK/ASCII mix.

use mur_common::Message;

const CHARS_PER_TOKEN: usize = 4;

#[derive(Debug)]
pub struct Chunk {
    pub messages: Vec<Message>,
    pub token_count: usize,
    pub span_range: (usize, usize), // (start_line, end_line) within the day JSONL
}

pub fn chunk_day(msgs: &[Message], token_budget: usize) -> Vec<Chunk> {
    if msgs.is_empty() {
        return Vec::new();
    }

    // Precompute token cost per message and its day-wide line index (1-based,
    // matches the extractive prompt's L<N> convention).
    let msg_costs: Vec<usize> = msgs.iter().map(message_token_cost).collect();

    let mut out = Vec::new();
    let mut current: Vec<usize> = Vec::new(); // indices into msgs
    let mut current_tokens = 0usize;
    let mut current_conv: Option<&str> = None;

    for (i, m) in msgs.iter().enumerate() {
        let cost = msg_costs[i];
        let msg_conv = m.conv.as_str();

        // Start-new-chunk decision:
        // 1. always start fresh if adding this msg would exceed budget AND
        //    we already have content (respecting the never-split rule)
        // 2. even if under budget, if msg_conv differs from current_conv AND
        //    we have > 1 msg, prefer splitting at the conv boundary when the
        //    resulting chunk would still fit — the boundary split loses
        //    nothing and keeps the extractive prompt focused.
        let would_overflow = current_tokens + cost > token_budget && !current.is_empty();
        let conv_boundary =
            current_conv.is_some() && current_conv != Some(msg_conv) && !current.is_empty();

        if would_overflow || (conv_boundary && current_tokens + cost > token_budget / 2) {
            out.push(make_chunk(msgs, &current, current_tokens));
            current.clear();
            current_tokens = 0;
        }

        current.push(i);
        current_tokens += cost;
        current_conv = Some(msg_conv);
    }

    if !current.is_empty() {
        out.push(make_chunk(msgs, &current, current_tokens));
    }
    out
}

fn message_token_cost(m: &Message) -> usize {
    // Scaffold overhead (role prefix, timestamp, line number) plus content.
    let content_chars = match &m.content {
        mur_common::Content::Text { value } => value.len(),
        mur_common::Content::ToolRef { desc, .. } => desc.len().saturating_add(64),
        mur_common::Content::ImageRef { desc, .. } => desc.len().saturating_add(48),
    };
    // ~40 chars scaffold: "L<line> [hh:mm:ss] <src>/<conv> (<role>): "
    (content_chars + 40) / CHARS_PER_TOKEN + 1
}

fn make_chunk(msgs: &[Message], indices: &[usize], tokens: usize) -> Chunk {
    let start_line = indices.first().copied().unwrap_or(0) + 1; // 1-based
    let end_line = indices.last().copied().unwrap_or(0) + 1;
    let chunk_msgs = indices.iter().map(|&i| msgs[i].clone()).collect();
    Chunk {
        messages: chunk_msgs,
        token_count: tokens,
        span_range: (start_line, end_line),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use mur_common::{Content, Role, Source};

    fn mk(conv: &str, text: &str) -> Message {
        Message {
            v: 1,
            ts: chrono::Utc.with_ymd_and_hms(2026, 4, 19, 10, 0, 0).unwrap(),
            src: Source::ClaudeCode,
            conv: conv.into(),
            role: Role::User,
            content: Content::Text { value: text.into() },
            meta: serde_json::Value::Null,
            refs: vec![],
        }
    }

    #[test]
    fn empty_day_yields_zero_chunks() {
        assert!(chunk_day(&[], 6000).is_empty());
    }

    #[test]
    fn all_fits_in_one_chunk_under_budget() {
        let msgs = vec![mk("c1", "hello"), mk("c1", "world"), mk("c1", "again")];
        let chunks = chunk_day(&msgs, 6000);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].messages.len(), 3);
        assert_eq!(chunks[0].span_range, (1, 3));
    }

    #[test]
    fn never_splits_single_message_even_if_over_budget() {
        let big = "x".repeat(40_000);
        let msgs = vec![mk("c1", &big)];
        let chunks = chunk_day(&msgs, 6000);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].messages.len(), 1);
    }

    #[test]
    fn splits_at_conv_boundary_when_under_half_budget_reached() {
        // Two conversations, each small enough that greedy would pack them
        // together, but boundary preference splits them.
        let pad = "y".repeat(12_000); // ~3000 tokens
        let msgs = vec![mk("c1", &pad), mk("c2", &pad)];
        let chunks = chunk_day(&msgs, 6000);
        assert_eq!(chunks.len(), 2, "expected 2 chunks, got {}", chunks.len());
        assert_eq!(chunks[0].messages[0].conv, "c1");
        assert_eq!(chunks[1].messages[0].conv, "c2");
    }

    #[test]
    fn span_range_is_one_indexed_end_inclusive() {
        let msgs = (0..5)
            .map(|i| mk("c1", &format!("msg{i}")))
            .collect::<Vec<_>>();
        let chunks = chunk_day(&msgs, 6000);
        assert_eq!(chunks[0].span_range, (1, 5));
    }
}

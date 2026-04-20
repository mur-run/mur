//! System prompt + context assembly (spec §5.3).

use super::retrieve::ResolvedHit;

pub const SYSTEM_PROMPT: &str = "You answer questions about the user's past AI-assistant conversations, using ONLY the excerpts provided below under \"Context\". Never invent facts not present in the excerpts.

Every factual claim in your answer MUST be followed by an inline citation in the form [cit: <date> <source>/<conv_id>:L<line>]. Use only the citations enumerated in the Context section — one citation per claim. You may use the same citation multiple times.

If the excerpts are insufficient to answer, say so plainly: \"The conversations I have access to don't cover that.\" Do not speculate. Do not use training knowledge to fill gaps.

Format: clear prose, 2-6 sentences per idea, Markdown bullets when listing. Be direct. Don't repeat the question. Don't apologize for not knowing.

When the user mentions a pattern name wrapped in {{pattern: name}} in the excerpts, that refers to a reusable artifact at ~/.mur/patterns/<name>.yaml; you may mention the pattern by name in your answer but do not expand it.";

pub struct RenderedPrompt {
    pub system: String,
    pub user: String,
    pub tokens_est: usize,
    pub valid_citations: Vec<String>, // normalized citation anchors for grounding
}

pub fn render(
    question: &str,
    hits: &[ResolvedHit],
    max_context_tokens: usize,
    response_tokens: usize,
) -> RenderedPrompt {
    let system = SYSTEM_PROMPT.to_string();

    let mut ctx = String::new();
    let mut valid_citations = Vec::new();
    for h in hits.iter() {
        let anchor = cite_anchor(h);
        valid_citations.push(anchor.clone());
        ctx.push_str(&anchor);
        ctx.push('\n');
        ctx.push_str("> ");
        ctx.push_str(&h.snippet.replace('\n', "\n> "));
        ctx.push_str("\n\n");
    }

    let truncated_question = truncate_chars(question, 2000);
    let mut user = format!("Context:\n{ctx}\nQuestion: {truncated_question}");
    let tokens_est = (system.len() + user.len()) / 4 + response_tokens + 120;

    // If we overflow, drop hits from the tail until we fit.
    let mut trimmed_hits = hits.len();
    while tokens_est > max_context_tokens && trimmed_hits > 1 {
        trimmed_hits -= 1;
        let mut ctx2 = String::new();
        valid_citations.clear();
        for h in hits.iter().take(trimmed_hits) {
            let anchor = cite_anchor(h);
            valid_citations.push(anchor.clone());
            ctx2.push_str(&anchor);
            ctx2.push('\n');
            ctx2.push_str("> ");
            ctx2.push_str(&h.snippet.replace('\n', "\n> "));
            ctx2.push_str("\n\n");
        }
        user = format!("Context:\n{ctx2}\nQuestion: {truncated_question}");
    }

    RenderedPrompt {
        system,
        user,
        tokens_est,
        valid_citations,
    }
}

pub fn cite_anchor(h: &ResolvedHit) -> String {
    match (h.layer, h.line_hint, h.span_index_in_summary) {
        (1, _, Some(idx)) => format!(
            "[cit: {} {}/{} @summary-span-{}]",
            h.info.date, h.info.source, h.info.conv_id, idx
        ),
        (_, Some(line), _) => format!(
            "[cit: {} {}/{}:L{}]",
            h.info.date, h.info.source, h.info.conv_id, line
        ),
        _ => format!(
            "[cit: {} {}/{}]",
            h.info.date, h.info.source, h.info.conv_id
        ),
    }
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect::<String>() + "…"
    }
}

#[cfg(test)]
mod tests {
    use super::super::HitInfo;
    use super::*;

    fn hit_raw(conv: &str, text: &str) -> ResolvedHit {
        ResolvedHit {
            layer: 0,
            info: HitInfo {
                layer: 0,
                source: "cc".into(),
                conv_id: conv.into(),
                date: chrono::NaiveDate::from_ymd_opt(2026, 4, 19).unwrap(),
                score: 0.9,
            },
            snippet: text.into(),
            line_hint: Some(42),
            span_index_in_summary: None,
        }
    }

    #[test]
    fn cite_anchor_raw_layer() {
        let h = hit_raw("abc", "hi");
        assert_eq!(cite_anchor(&h), "[cit: 2026-04-19 cc/abc:L42]");
    }

    #[test]
    fn cite_anchor_summary_layer() {
        let mut h = hit_raw("abc", "hi");
        h.layer = 1;
        h.span_index_in_summary = Some(3);
        h.line_hint = None;
        assert_eq!(cite_anchor(&h), "[cit: 2026-04-19 cc/abc @summary-span-3]");
    }

    #[test]
    fn render_shrinks_hits_on_overflow() {
        let hits = (0..20)
            .map(|i| hit_raw(&format!("c{i}"), &"x".repeat(3000)))
            .collect::<Vec<_>>();
        let r = render("question?", &hits, 6000, 1024);
        assert!(r.valid_citations.len() < hits.len());
        assert!(!r.valid_citations.is_empty());
    }

    #[test]
    fn render_lists_valid_citations_in_order() {
        let hits = vec![hit_raw("a", "one"), hit_raw("b", "two")];
        let r = render("q?", &hits, 6000, 1024);
        assert_eq!(r.valid_citations.len(), 2);
        assert!(r.user.contains("one"));
        assert!(r.user.contains("two"));
    }
}

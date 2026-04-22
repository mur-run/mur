//! Output formatting for ask (spec §5.6).
//! Plain: streaming answer + trailing citation block + runtime footer.
//! JSON: buffered AskResponse emitted once at end.

use super::{AskResponse, Citation};

pub fn render_citations_block(citations: &[Citation]) -> String {
    let mut out = String::new();
    if citations.is_empty() {
        return out;
    }
    out.push_str("\nCitations:\n");
    for c in citations {
        let anchor = match (c.line_hint, c.span_index_in_summary) {
            (_, Some(idx)) => format!(
                "[cit: {} {}/{} @summary-span-{}]",
                c.date, c.source, c.conv_id, idx
            ),
            (Some(line), _) => format!("[cit: {} {}/{}:L{}]", c.date, c.source, c.conv_id, line),
            _ => format!("[cit: {} {}/{}]", c.date, c.source, c.conv_id),
        };
        let preview: String = c.snippet.chars().take(120).collect();
        let suffix = if c.compressed == Some(crate::conversations::ask::Compression::Abstractive) {
            " (summarized)"
        } else {
            ""
        };
        out.push_str(&format!("  {anchor}\n    — {preview}{suffix}\n"));
    }
    out
}

pub fn render_footer(resp: &AskResponse) -> String {
    let tag = if resp.degraded_to_mode_b {
        " · Mode B fallback"
    } else {
        ""
    };
    format!(
        "({} hits · {}ms · {}→{} tokens{})\n",
        resp.citations.len(),
        resp.duration_ms,
        resp.tokens_in,
        resp.tokens_out,
        tag,
    )
}

pub fn render_json(resp: &AskResponse) -> String {
    serde_json::to_string_pretty(resp).unwrap_or_else(|_| "{}".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_resp() -> AskResponse {
        AskResponse {
            answer: "Mock answer [cit: 2026-04-19 cc/a:L1]".into(),
            citations: vec![Citation {
                id: 1,
                date: chrono::NaiveDate::from_ymd_opt(2026, 4, 19).unwrap(),
                source: "cc".into(),
                conv_id: "a".into(),
                line_hint: Some(1),
                span_index_in_summary: None,
                snippet: "sample snippet text".into(),
                score: 0.87,
                compressed: None,
            }],
            hits_used: vec![],
            degraded_to_mode_b: false,
            tokens_in: 100,
            tokens_out: 20,
            duration_ms: 500,
            rewritten_question: None,
            rewriter_status: crate::conversations::ask::session::RewriterStatus::Skipped,
            stage_1b: None,
        }
    }

    #[test]
    fn citations_block_contains_anchor_and_preview() {
        let r = sample_resp();
        let block = render_citations_block(&r.citations);
        assert!(block.contains("[cit: 2026-04-19 cc/a:L1]"));
        assert!(block.contains("sample snippet text"));
    }

    #[test]
    fn footer_shows_mode_b_tag_when_degraded() {
        let mut r = sample_resp();
        r.degraded_to_mode_b = true;
        let f = render_footer(&r);
        assert!(f.contains("Mode B fallback"));
    }

    #[test]
    fn json_roundtrip() {
        let r = sample_resp();
        let s = render_json(&r);
        assert!(s.contains("\"answer\""));
        assert!(s.contains("\"citations\""));
    }

    #[test]
    fn citations_block_suffixes_summarized_for_abstractive() {
        let c = vec![
            Citation {
                id: 1,
                date: chrono::NaiveDate::from_ymd_opt(2026, 4, 22).unwrap(),
                source: "cc".into(),
                conv_id: "c1".into(),
                line_hint: Some(1),
                span_index_in_summary: None,
                snippet: "sample".into(),
                score: 0.9,
                compressed: Some(crate::conversations::ask::Compression::Abstractive),
            },
            Citation {
                id: 2,
                date: chrono::NaiveDate::from_ymd_opt(2026, 4, 22).unwrap(),
                source: "cc".into(),
                conv_id: "c2".into(),
                line_hint: Some(1),
                span_index_in_summary: None,
                snippet: "sample2".into(),
                score: 0.9,
                compressed: Some(crate::conversations::ask::Compression::Heuristic),
            },
            Citation {
                id: 3,
                date: chrono::NaiveDate::from_ymd_opt(2026, 4, 22).unwrap(),
                source: "cc".into(),
                conv_id: "c3".into(),
                line_hint: Some(1),
                span_index_in_summary: None,
                snippet: "sample3".into(),
                score: 0.9,
                compressed: None,
            },
        ];
        let block = render_citations_block(&c);
        // Abstractive → (summarized) suffix.
        assert!(
            block.contains("cc/c1:L1") && block.contains("(summarized)"),
            "expected (summarized) next to c1, got:\n{block}"
        );
        // Heuristic → NOT suffixed.
        let lines: Vec<&str> = block.lines().collect();
        let c2_line = lines.iter().find(|l| l.contains("cc/c2:L1")).unwrap();
        assert!(
            !c2_line.contains("(summarized)"),
            "heuristic must NOT be marked summarized in plain mode; got: {c2_line}"
        );
        // None → unchanged.
        let c3_line = lines.iter().find(|l| l.contains("cc/c3:L1")).unwrap();
        assert!(!c3_line.contains("(summarized)"));
    }
}

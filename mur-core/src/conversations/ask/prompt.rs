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
    /// Post-cascade hits (after any compression / no-op). Caller uses this
    /// to build `citations_map` so the `compressed` provenance flag flows
    /// through to `Citation.compressed`. Size equals `trimmed_hits` (<=
    /// input hits.len()).
    pub final_hits: Vec<super::retrieve::ResolvedHit>,
    /// Present only if Stage 1b fired.
    pub stage_1b: Option<super::abstractive::Stage1bSummary>,
}

/// Chars of each prior answer to include in the `## Chat History` section.
/// Matches `rewriter::PRIOR_ANSWER_TRUNCATE_CHARS` to keep behavior consistent.
const HISTORY_ANSWER_TRUNCATE_CHARS: usize = 500;

/// Format the prior-turns block for the generation prompt. Empty string
/// if `turns` is empty (caller decides whether to prepend the `## Chat History`
/// header).
fn render_history_block(turns: &[super::session::TurnRecord]) -> String {
    let mut s = String::new();
    for t in turns {
        s.push_str("User: ");
        s.push_str(&t.question);
        s.push('\n');
        s.push_str("Assistant: ");
        s.push_str(&truncate_chars(&t.answer, HISTORY_ANSWER_TRUNCATE_CHARS));
        s.push('\n');
    }
    s
}

#[allow(clippy::too_many_arguments)]
pub async fn render(
    question: &str,
    prior_turns: &[super::session::TurnRecord],
    hits: Vec<ResolvedHit>,
    max_context_tokens: usize,
    response_tokens: usize,
    compress_enabled: bool,
    summarize_enabled: bool,
    abstractive_ctx: Option<&super::abstractive::AbstractiveCtx<'_>>,
) -> RenderedPrompt {
    let system = SYSTEM_PROMPT.to_string();
    let truncated_question = truncate_chars(question, 2000);

    let mut history_cursor = 0usize;
    let mut trimmed_hits = hits.len();
    let mut active_hits: Vec<super::retrieve::ResolvedHit> = hits;

    let (mut user, mut valid_citations) = render_ctx_and_user(
        &active_hits,
        prior_turns,
        history_cursor,
        trimmed_hits,
        &truncated_question,
    );
    let mut cur_tokens = tokens_est(&system, &user, response_tokens);

    // Stage 1 — Phase 3.4 heuristic compression (unchanged).
    if cur_tokens > max_context_tokens && compress_enabled {
        let overage_chars = cur_tokens
            .saturating_sub(max_context_tokens)
            .saturating_mul(4);
        let total_chars: usize = active_hits.iter().map(|h| h.snippet.len()).sum();
        let ratio = 1.0 - (overage_chars as f64 / total_chars.max(1) as f64).min(0.6);
        let avg = total_chars / active_hits.len().max(1);
        let target = (avg as f64 * ratio) as usize;
        active_hits = super::compress::compress_hits(active_hits, question, target);
        (user, valid_citations) = render_ctx_and_user(
            &active_hits,
            prior_turns,
            history_cursor,
            trimmed_hits,
            &truncated_question,
        );
        cur_tokens = tokens_est(&system, &user, response_tokens);
    }

    // Stage 1b — Phase 3.5 LLM-abstractive compression.
    let mut stage_1b: Option<super::abstractive::Stage1bSummary> = None;
    if cur_tokens > max_context_tokens
        && summarize_enabled
        && let Some(ctx) = abstractive_ctx
    {
        // H1: pass a closure that re-renders the full prompt and runs the
        // real `tokens_est` heuristic so Stage 1b's early-exit uses ground
        // truth, not a per-hit `len/4` delta that ignores system prompt +
        // markdown ceremony + response_tokens floor.
        let system_ref = system.as_str();
        let truncated_question_ref = truncated_question.as_str();
        let summary = super::abstractive::run_stage_1b(
            ctx,
            &mut active_hits,
            cur_tokens,
            max_context_tokens,
            |hs| {
                let (u, _) = render_ctx_and_user(
                    hs,
                    prior_turns,
                    history_cursor,
                    trimmed_hits,
                    truncated_question_ref,
                );
                tokens_est(system_ref, &u, response_tokens)
            },
        )
        .await;
        for (idx, reason) in &summary.skipped {
            tracing::warn!(hit_idx = idx, reason, "stage-1b skipped");
        }
        stage_1b = Some(summary);
        (user, valid_citations) = render_ctx_and_user(
            &active_hits,
            prior_turns,
            history_cursor,
            trimmed_hits,
            &truncated_question,
        );
        cur_tokens = tokens_est(&system, &user, response_tokens);
    }

    // Stage 2 — drop oldest history turns.
    while cur_tokens > max_context_tokens && history_cursor < prior_turns.len() {
        history_cursor += 1;
        (user, valid_citations) = render_ctx_and_user(
            &active_hits,
            prior_turns,
            history_cursor,
            trimmed_hits,
            &truncated_question,
        );
        cur_tokens = tokens_est(&system, &user, response_tokens);
    }

    // Stage 3 — shrink hits from the tail.
    while cur_tokens > max_context_tokens && trimmed_hits > 1 {
        trimmed_hits -= 1;
        (user, valid_citations) = render_ctx_and_user(
            &active_hits,
            prior_turns,
            history_cursor,
            trimmed_hits,
            &truncated_question,
        );
        cur_tokens = tokens_est(&system, &user, response_tokens);
    }

    // Drop hits beyond `trimmed_hits` so `final_hits` mirrors what the prompt
    // actually references. Callers building citations_map from final_hits
    // then only see citations that can be anchored.
    active_hits.truncate(trimmed_hits);

    RenderedPrompt {
        system,
        user,
        tokens_est: cur_tokens,
        valid_citations,
        final_hits: active_hits,
        stage_1b,
    }
}

/// Build the user-section prompt body for a given (hits, history_cursor,
/// trimmed_hits) configuration. Returns (user, valid_citations).
///
/// Extracted for DRY — called by the initial render, each overflow stage
/// (compression, history-drop, hit-shrink).
fn render_ctx_and_user(
    active_hits: &[ResolvedHit],
    prior_turns: &[super::session::TurnRecord],
    history_cursor: usize,
    trimmed_hits: usize,
    truncated_question: &str,
) -> (String, Vec<String>) {
    let mut ctx = String::new();
    let mut valid_citations = Vec::new();
    for h in active_hits.iter().take(trimmed_hits) {
        let anchor = cite_anchor(h);
        valid_citations.push(anchor.clone());
        ctx.push_str(&anchor);
        ctx.push('\n');
        ctx.push_str("> ");
        ctx.push_str(&h.snippet.replace('\n', "\n> "));
        ctx.push_str("\n\n");
    }
    let history_block = if history_cursor >= prior_turns.len() {
        String::new()
    } else {
        format!(
            "## Chat History\n\n{}\n",
            render_history_block(&prior_turns[history_cursor..])
        )
    };
    let user = format!("{history_block}## Context\n\n{ctx}\n## Question\n\n{truncated_question}");
    (user, valid_citations)
}

/// Token-estimate heuristic shared between initial render + overflow stages.
fn tokens_est(system: &str, user: &str, response_tokens: usize) -> usize {
    (system.len() + user.len()) / 4 + response_tokens + 120
}

pub fn cite_anchor(h: &ResolvedHit) -> String {
    match h.layer {
        4 => format!("[cit: {} month/{}]", h.info.date, h.info.conv_id),
        3 => format!("[cit: {} week/{}]", h.info.date, h.info.conv_id),
        _ => match (h.line_hint, h.span_index_in_summary) {
            (_, Some(idx)) if h.layer == 1 => format!(
                "[cit: {} {}/{} @summary-span-{}]",
                h.info.date, h.info.source, h.info.conv_id, idx
            ),
            (Some(line), _) => format!(
                "[cit: {} {}/{}:L{}]",
                h.info.date, h.info.source, h.info.conv_id, line
            ),
            _ => format!(
                "[cit: {} {}/{}]",
                h.info.date, h.info.source, h.info.conv_id
            ),
        },
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
            vector: None,
            compressed: None,
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

    #[tokio::test]
    async fn render_shrinks_hits_on_overflow() {
        let hits: Vec<_> = (0..20)
            .map(|i| hit_raw(&format!("c{i}"), &"x".repeat(3000)))
            .collect();
        let n = hits.len();
        let r = render("question?", &[], hits, 6000, 1024, true, false, None).await;
        assert!(r.valid_citations.len() < n);
        assert!(!r.valid_citations.is_empty());
    }

    #[tokio::test]
    async fn render_lists_valid_citations_in_order() {
        let hits = vec![hit_raw("a", "one"), hit_raw("b", "two")];
        let r = render("q?", &[], hits, 6000, 1024, true, false, None).await;
        assert_eq!(r.valid_citations.len(), 2);
        assert!(r.user.contains("one"));
        assert!(r.user.contains("two"));
    }

    #[test]
    fn cite_anchor_layer_3_week_format() {
        let h = ResolvedHit {
            layer: 3,
            info: HitInfo {
                layer: 3,
                source: "week".into(),
                conv_id: "2026-W16".into(),
                date: chrono::NaiveDate::from_ymd_opt(2026, 4, 13).unwrap(),
                score: 0.9,
            },
            snippet: "this week...".into(),
            line_hint: None,
            span_index_in_summary: None,
            vector: Some(vec![0.1; 16]),
            compressed: None,
        };
        assert_eq!(cite_anchor(&h), "[cit: 2026-04-13 week/2026-W16]");
    }

    #[test]
    fn cite_anchor_layer_4_month_format() {
        let h = ResolvedHit {
            layer: 4,
            info: HitInfo {
                layer: 4,
                source: "month".into(),
                conv_id: "2026-04".into(),
                date: chrono::NaiveDate::from_ymd_opt(2026, 4, 1).unwrap(),
                score: 0.9,
            },
            snippet: "this month...".into(),
            line_hint: None,
            span_index_in_summary: None,
            vector: None,
            compressed: None,
        };
        assert_eq!(cite_anchor(&h), "[cit: 2026-04-01 month/2026-04]");
    }

    fn turn_rec(q: &str, a: &str) -> super::super::session::TurnRecord {
        super::super::session::TurnRecord {
            v: 1,
            turn_id: 1,
            ts: chrono::DateTime::parse_from_rfc3339("2026-04-21T15:30:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
            question: q.into(),
            rewritten_question: None,
            hits_used: vec![],
            answer: a.into(),
            citations: vec![],
            degraded_to_mode_b: false,
            rewriter_status: super::super::session::RewriterStatus::Skipped,
            tokens_in: 0,
            tokens_out: 0,
            duration_ms: 0,
        }
    }

    #[tokio::test]
    async fn render_includes_chat_history_section_when_prior_turns_non_empty() {
        let hits = vec![hit_raw("a", "one")];
        let prior = vec![turn_rec("prev q", "prev a")];
        let r = render("new q?", &prior, hits, 6000, 1024, true, false, None).await;
        assert!(
            r.user.contains("## Chat History"),
            "expected '## Chat History' header, got:\n{}",
            r.user
        );
        assert!(r.user.contains("User: prev q"));
        assert!(r.user.contains("Assistant: prev a"));
        // Current question is still in the user section
        assert!(r.user.contains("new q?"));
    }

    #[tokio::test]
    async fn render_omits_chat_history_section_when_prior_turns_empty() {
        let hits = vec![hit_raw("a", "one")];
        let r = render("q?", &[], hits, 6000, 1024, true, false, None).await;
        assert!(!r.user.contains("## Chat History"));
    }

    #[tokio::test]
    async fn render_drops_oldest_history_first_on_budget_overflow() {
        let hits = vec![hit_raw("a", "unique-hit-content")];
        // Three prior turns, each with a recognizable answer. Budget tight
        // enough that history must drop.
        let prior = vec![
            turn_rec("q1-oldest", "aaaa-oldest-ANSWER"),
            turn_rec("q2-middle", "bbbb-middle-ANSWER"),
            turn_rec("q3-newest", "cccc-newest-ANSWER"),
        ];
        // Budget chosen so history doesn't fit but hit does.
        // compress_enabled=false: isolate the history-drop path (Stage 2),
        // preventing Stage 1 from firing first on this tight budget.
        let r = render("new q?", &prior, hits, 500, 100, false, false, None).await;
        // The hit must survive.
        assert!(r.user.contains("unique-hit-content"));
        // Oldest history turn must be dropped before middle/newest.
        let has_oldest = r.user.contains("aaaa-oldest-ANSWER");
        let has_newest = r.user.contains("cccc-newest-ANSWER");
        assert!(
            !has_oldest || has_newest,
            "if oldest survives, newest should too (invalid ordering)"
        );
        // Under a very tight budget the oldest should be dropped.
        if has_newest {
            // Expected path: newest survived, oldest dropped
            assert!(!has_oldest, "oldest should be dropped first");
        }
    }

    #[tokio::test]
    async fn render_falls_through_to_hit_shrinking_when_history_exhausted() {
        // 5 hits + 2 prior turns + tight budget. Expect: history fully dropped,
        // then hits start shrinking.
        let hits: Vec<_> = (0..5)
            .map(|i| hit_raw(&format!("c{i}"), &"x".repeat(800)))
            .collect();
        let n = hits.len();
        let prior = vec![
            turn_rec("q1", &"yyyyy".repeat(100)),
            turn_rec("q2", &"zzzzz".repeat(100)),
        ];
        let r = render("q?", &prior, hits, 1500, 300, false, false, None).await;
        // History should be entirely gone.
        assert!(
            !r.user.contains("## Chat History"),
            "history should be dropped"
        );
        // Hits should be shrunk (fewer than 5).
        assert!(
            r.valid_citations.len() < n,
            "expected hit count < {n}, got {}",
            r.valid_citations.len()
        );
        assert!(!r.valid_citations.is_empty());
    }

    #[tokio::test]
    async fn render_compresses_hits_on_overflow_when_enabled() {
        // Craft a single long hit (>= COMPRESS_MIN_CHARS = 400 and >= 4
        // sentences) plus a tight budget to force Stage 1 compression.
        let long_snippet = (0..10)
            .map(|i| format!("Fact number {i} with some supporting body detail."))
            .collect::<Vec<_>>()
            .join(" ");
        assert!(long_snippet.len() >= 400);
        let hits = vec![ResolvedHit {
            layer: 0,
            info: HitInfo {
                layer: 0,
                source: "cc".into(),
                conv_id: "c1".into(),
                date: chrono::NaiveDate::from_ymd_opt(2026, 4, 22).unwrap(),
                score: 0.9,
            },
            snippet: long_snippet.clone(),
            line_hint: Some(1),
            span_index_in_summary: None,
            vector: None,
            compressed: None,
        }];
        let prior = vec![turn_rec("prev q", "prev a")];
        // Very tight budget → Stage 1 must fire.
        let r = render("q?", &prior, hits, 400, 100, true, false, None).await;
        // The rendered user message must contain LESS than the original
        // hit's snippet (i.e. compression actually shrank it).
        let context_section = r.user.find("## Context").unwrap();
        let question_section = r.user.find("## Question").unwrap();
        let ctx_slice = &r.user[context_section..question_section];
        assert!(
            ctx_slice.len() < long_snippet.len(),
            "context section ({} chars) should be shorter than original snippet ({} chars)",
            ctx_slice.len(),
            long_snippet.len()
        );
        // Citation survived.
        assert_eq!(r.valid_citations.len(), 1);
    }

    #[tokio::test]
    async fn render_does_not_compress_when_disabled() {
        // Same setup, compress_enabled = false → Stage 1 skipped → fall
        // through to Phase 3.3 behavior (drop oldest history, then shrink
        // hits). With only one hit + no history, Stage 2 and Stage 3 are
        // both no-ops, so the hit body stays INTACT (even over budget).
        let long_snippet = (0..10)
            .map(|i| format!("Fact number {i} with some supporting body detail."))
            .collect::<Vec<_>>()
            .join(" ");
        let hits = vec![ResolvedHit {
            layer: 0,
            info: HitInfo {
                layer: 0,
                source: "cc".into(),
                conv_id: "c1".into(),
                date: chrono::NaiveDate::from_ymd_opt(2026, 4, 22).unwrap(),
                score: 0.9,
            },
            snippet: long_snippet.clone(),
            line_hint: Some(1),
            span_index_in_summary: None,
            vector: None,
            compressed: None,
        }];
        let r = render("q?", &[], hits, 400, 100, false, false, None).await;
        // No compression → the full snippet appears in the context.
        assert!(
            r.user.contains(&long_snippet),
            "with compression disabled, full snippet should be present"
        );
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn render_fires_stage_1b_when_compression_alone_insufficient() {
        use crate::conversations::ENV_LOCK;
        use crate::conversations::ask::abstractive::AbstractiveCtx;
        use crate::conversations::ollama::OllamaClient;
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        unsafe { std::env::remove_var("MUR_ABSTRACTIVE_MOCK_FAIL") };
        let client = OllamaClient::new("http://unused", std::time::Duration::from_secs(1));
        let tmp = tempfile::tempdir().unwrap();
        let ctx = AbstractiveCtx {
            client: &client,
            model: "qwen3:14b",
            timeout: std::time::Duration::from_secs(1),
            root_override: Some(tmp.path().to_str().unwrap()),
        };
        // 3 long hits + tight budget so Stage 1 heuristic compression alone
        // still overruns, forcing Stage 1b. Stage 1 will mark each hit
        // Heuristic; Phase 3.5 review H3 then requires Stage 1b to SKIP them
        // with `already_compressed` (avoids compounding extractive + abstractive
        // loss on the same hit). Stage 2/3 pick up the remaining overflow.
        let big = (0..50)
            .map(|i| format!("Sentence number {i} with plenty of supporting body."))
            .collect::<Vec<_>>()
            .join(" ");
        let hits = vec![hit_raw("a", &big), hit_raw("b", &big), hit_raw("c", &big)];
        let r = super::render("q?", &[], hits, 500, 100, true, true, Some(&ctx)).await;
        assert!(
            r.stage_1b.is_some(),
            "Stage 1b summary must surface when fired"
        );
        let s = r.stage_1b.unwrap();
        // After H3: every hit entering Stage 1b is already Heuristic-compressed,
        // so all are skipped with ALREADY_COMPRESSED. The Stage 1b pass still
        // fires (surfaces a summary) but does not re-compress.
        use crate::conversations::ask::abstractive::skip_reason;
        assert_eq!(
            s.processed, 3,
            "Stage 1b should have evaluated all 3 hits; got {s:?}"
        );
        assert_eq!(
            s.compressed_count, 0,
            "Stage 1b must not re-compress Heuristic extracts; got {s:?}"
        );
        assert!(
            s.skipped
                .iter()
                .all(|(_, reason)| *reason == skip_reason::ALREADY_COMPRESSED),
            "every skip should be ALREADY_COMPRESSED; got {:?}",
            s.skipped
        );
        // All hits remain Heuristic-marked — Stage 1b must not overwrite provenance.
        assert!(
            r.final_hits
                .iter()
                .all(|h| h.compressed == Some(super::super::Compression::Heuristic)),
            "every final hit should retain Compression::Heuristic"
        );
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }

    /// M2 (phase 3.3 review follow-up): render must terminate with a sane
    /// output even when the budget is unachievably tight — specifically
    /// `max_context_tokens = 0`. Exercises the overflow-loop exit guards
    /// (Stage 2 stops when history_cursor reaches prior_turns.len(); Stage 3
    /// stops when trimmed_hits > 1 becomes false). No infinite loop.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn render_terminates_on_zero_budget() {
        use crate::conversations::ENV_LOCK;
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        let hits = vec![hit_raw("a", "answer one"), hit_raw("b", "answer two")];
        let r = super::render(
            "what happened?",
            &[],
            hits,
            /* max_context_tokens = */ 0,
            100,
            false,
            false,
            None,
        )
        .await;
        // Function returns; assertions confirm we exited the loops cleanly
        // and kept at least one hit (Stage 3 guard: trimmed_hits > 1).
        assert_eq!(
            r.final_hits.len(),
            1,
            "stage-3 guard should leave exactly 1 hit on zero-budget"
        );
        assert!(!r.user.is_empty(), "user prompt should not be empty");
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn render_stage_1b_none_when_disabled() {
        use crate::conversations::ENV_LOCK;
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        let big = (0..50)
            .map(|i| format!("Sentence number {i} with plenty of supporting body."))
            .collect::<Vec<_>>()
            .join(" ");
        let hits = vec![hit_raw("a", &big)];
        let r = super::render(
            "q?",
            &[],
            hits,
            500,
            100,
            true,
            /* summarize_enabled */ false,
            None,
        )
        .await;
        assert!(r.stage_1b.is_none());
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }
}

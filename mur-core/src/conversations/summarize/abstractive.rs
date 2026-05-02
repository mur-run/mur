//! Abstractive narrative stage. Single LLM call over all extractive spans.
//! See spec §4.3. Output: 150-400 words, first-person or neutral.

use tracing::warn;

use super::super::backend::{ChatBackend, ChatRequest};
use super::extractive::ExtractiveSpan;

pub struct AbstractiveResult {
    pub narrative: Option<String>, // None iff LLM failed and caller should set the warning
    pub word_count: usize,
}

pub async fn summarize(
    backend: &dyn ChatBackend,
    model: &str,
    spans: &[ExtractiveSpan],
    date: chrono::NaiveDate,
    max_words: u32,
) -> AbstractiveResult {
    if spans.is_empty() {
        return AbstractiveResult {
            narrative: Some("No significant activity on this day.".to_string()),
            word_count: 6,
        };
    }
    let prompt = render_prompt(spans, date, max_words);
    let resp = backend
        .generate(ChatRequest {
            model,
            user: &prompt,
            system: None,
            max_tokens: max_words * 2, // tokens > words; headroom
            temperature: Some(0.2),
            stop: vec![],
            cache_system: false,
            cache_user_prefix: None,
        })
        .await;
    match resp {
        Ok(r) => {
            let narrative = clean_output(&r.text);
            let word_count = narrative.split_whitespace().count();
            AbstractiveResult {
                narrative: Some(narrative),
                word_count,
            }
        }
        Err(e) => {
            warn!("abstractive LLM call failed: {e:#}");
            AbstractiveResult {
                narrative: None,
                word_count: 0,
            }
        }
    }
}

fn render_prompt(spans: &[ExtractiveSpan], date: chrono::NaiveDate, max_words: u32) -> String {
    let min_words = 150.min(max_words / 2);
    let mut body = format!(
        "You are summarizing one day ({}) of a developer's AI-assistant conversations into a \
         narrative paragraph. Use ONLY information present in the spans below.\n\n\
         Output: {}-{} words, first-person or neutral third-person, no bullet lists. \
         Reference each key point by its span index [N]. Do NOT invent details not in the spans. \
         If spans conflict, note the conflict.\n\n\
         Spans:\n",
        date, min_words, max_words
    );
    for (i, s) in spans.iter().enumerate() {
        body.push_str(&format!(
            "[{}] {{{} {}/{} L{}}}: {}\n",
            i + 1,
            date,
            s.src.file_prefix(),
            s.conv_id,
            s.line_hint,
            s.text,
        ));
    }
    body.push_str("\nWrite the narrative.\n");
    body
}

/// Rollup granularity for Phase 3.2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RollupKind {
    Week,
    Month,
}

impl RollupKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            RollupKind::Week => "week",
            RollupKind::Month => "month",
        }
    }
}

/// Input for a rollup abstractive LLM call. `selected_spans` are the
/// cross-window MMR-deduped extractive spans (ground truth for citation).
/// `prior_narratives` are the source day (week rollup) or week (month
/// rollup) narratives — framing context only, do not quote verbatim.
pub struct RollupAbstractiveInput<'a> {
    pub kind: RollupKind,
    pub window_label: &'a str,
    pub selected_spans: &'a [crate::conversations::ask::retrieve::ResolvedHit],
    pub prior_narratives: &'a [(String, String)],
}

pub async fn rollup_narrative(
    client: &crate::conversations::ollama::OllamaClient,
    model: &str,
    input: &RollupAbstractiveInput<'_>,
    max_words: u32,
) -> AbstractiveResult {
    let prompt = render_rollup_prompt(input, max_words);
    let resp = client
        .generate(crate::conversations::ollama::GenerateRequest {
            model,
            prompt: &prompt,
            system: None,
            stream: false,
            options: crate::conversations::ollama::GenerateOptions {
                temperature: Some(0.2),
                top_p: Some(0.9),
                num_predict: Some(max_words * 2),
                stop: vec![],
            },
        })
        .await;
    match resp {
        Ok(r) => {
            let narrative = clean_output(&r.response);
            let word_count = narrative.split_whitespace().count();
            AbstractiveResult {
                narrative: Some(narrative),
                word_count,
            }
        }
        Err(e) => {
            tracing::warn!("rollup abstractive call failed: {e:#}");
            AbstractiveResult {
                narrative: None,
                word_count: 0,
            }
        }
    }
}

fn render_rollup_prompt(input: &RollupAbstractiveInput<'_>, max_words: u32) -> String {
    let min_words = 150.min(max_words / 2);
    let kind_str = match input.kind {
        RollupKind::Week => "one week",
        RollupKind::Month => "one month",
    };
    let mut body = format!(
        "You are summarizing {kind_str} ({window}) of a developer's AI-assistant \
         conversations into a narrative paragraph. Use ONLY information present \
         in the spans below. The prior narratives are context for framing — \
         do NOT quote them verbatim. Reference each key fact by its span index [N].\n\n\
         Output: {min_words}-{max_words} words, first-person or neutral, no bullet \
         lists. Do NOT invent details.\n\n\
         Spans (cross-day, deduplicated):\n",
        window = input.window_label,
    );
    for (i, h) in input.selected_spans.iter().enumerate() {
        body.push_str(&format!(
            "  [{}] {{{} {}/{} L{}}}: \"{}\"\n",
            i + 1,
            h.info.date,
            h.info.source,
            h.info.conv_id,
            h.line_hint.unwrap_or(0),
            h.snippet.replace('\n', " "),
        ));
    }
    body.push_str("\nPrior narratives (context only, do not quote):\n");
    for (label, narrative) in input.prior_narratives {
        body.push_str(&format!("  {label}: {narrative}\n"));
    }
    body.push_str("\nWrite the narrative.\n");
    body
}

fn clean_output(raw: &str) -> String {
    let trimmed = raw.trim();
    // Strip trailing commentary like "Let me know if you'd like..." that some
    // models append after the summary. Heuristic: keep content up to the first
    // double-newline that follows a complete sentence.
    if let Some(idx) = trimmed.find("\n\nLet me") {
        return trimmed[..idx].trim().to_string();
    }
    if let Some(idx) = trimmed.find("\n\nWould you") {
        return trimmed[..idx].trim().to_string();
    }
    trimmed.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversations::ENV_LOCK;
    use mur_common::{Role, Source};

    fn span(idx: u32, text: &str) -> ExtractiveSpan {
        ExtractiveSpan {
            role: Role::User,
            conv_id: "c1".into(),
            line_hint: idx,
            text: text.into(),
            src: Source::ClaudeCode,
        }
    }

    #[tokio::test]
    async fn empty_spans_emit_placeholder() {
        use crate::conversations::backend::mock::MockBackend;
        let backend = MockBackend::new();
        let r = summarize(
            &backend,
            "qwen3:14b",
            &[],
            chrono::NaiveDate::from_ymd_opt(2026, 4, 19).unwrap(),
            400,
        )
        .await;
        assert!(r.narrative.as_deref().unwrap().contains("No significant"));
    }

    #[tokio::test]
    async fn mock_narrative_happy_path() {
        use crate::conversations::backend::mock::MockBackend;
        let backend = MockBackend::new();
        let spans = vec![span(1, "hello world"), span(2, "compression works")];
        let r = summarize(
            &backend,
            "qwen3:14b",
            &spans,
            chrono::NaiveDate::from_ymd_opt(2026, 4, 19).unwrap(),
            400,
        )
        .await;
        assert!(
            r.narrative
                .as_deref()
                .unwrap()
                .starts_with("Mock narrative")
        );
        assert!(r.word_count > 0);
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn summarize_via_chat_backend_mock_returns_prose() {
        use crate::conversations::backend::mock::MockBackend;
        let _env_guard = ENV_LOCK.lock().unwrap();
        // MockBackend reuses ollama::mock_generate; we don't need MUR_OLLAMA_MOCK
        // (it's a direct trait impl). Clear it to make sure we're hitting the
        // backend path, not the legacy env-var fallback.
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
        unsafe { std::env::remove_var("MUR_LLM_MOCK") };
        let backend = MockBackend::new();
        let spans = vec![span(1, "hello world"), span(2, "compression works")];
        let r = summarize(
            &backend,
            "qwen3:14b",
            &spans,
            chrono::NaiveDate::from_ymd_opt(2026, 4, 19).unwrap(),
            400,
        )
        .await;
        assert!(
            r.narrative
                .as_deref()
                .unwrap()
                .starts_with("Mock narrative")
        );
        assert!(r.word_count > 0);
    }

    #[test]
    fn clean_output_strips_trailing_commentary() {
        let raw = "This is the narrative.\n\nLet me know if you'd like more detail!";
        assert_eq!(clean_output(raw), "This is the narrative.");
    }

    #[tokio::test]
    async fn placeholder_word_count_matches_string() {
        // Guard: the empty-day placeholder word_count must match its actual
        // whitespace split. Prior version hardcoded 5 vs the 6-word string
        // "No significant activity on this day."
        use crate::conversations::backend::mock::MockBackend;
        let backend = MockBackend::new();
        let r = summarize(
            &backend,
            "qwen3:14b",
            &[],
            chrono::NaiveDate::from_ymd_opt(2026, 4, 19).unwrap(),
            400,
        )
        .await;
        let actual = r.narrative.as_deref().unwrap().split_whitespace().count();
        assert_eq!(
            r.word_count, actual,
            "word_count ({}) should equal split_whitespace count ({})",
            r.word_count, actual
        );
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn rollup_narrative_week_returns_week_mock() {
        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        let client = crate::conversations::ollama::OllamaClient::new(
            "http://unused",
            std::time::Duration::from_secs(1),
        );
        let input = RollupAbstractiveInput {
            kind: RollupKind::Week,
            window_label: "2026-W16",
            selected_spans: &[],
            prior_narratives: &[],
        };
        let r = rollup_narrative(&client, "qwen3:14b", &input, 500).await;
        let n = r.narrative.expect("should have narrative");
        assert!(n.to_lowercase().contains("this week"), "got: {n}");
        assert!(r.word_count > 0);
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn rollup_narrative_month_returns_month_mock() {
        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        let client = crate::conversations::ollama::OllamaClient::new(
            "http://unused",
            std::time::Duration::from_secs(1),
        );
        let input = RollupAbstractiveInput {
            kind: RollupKind::Month,
            window_label: "2026-04",
            selected_spans: &[],
            prior_narratives: &[],
        };
        let r = rollup_narrative(&client, "qwen3:14b", &input, 700).await;
        let n = r.narrative.expect("should have narrative");
        assert!(n.to_lowercase().contains("this month"), "got: {n}");
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }
}

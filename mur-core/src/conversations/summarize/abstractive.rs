//! Abstractive narrative stage. Single LLM call over all extractive spans.
//! See spec §4.3. Output: 150-400 words, first-person or neutral.

use tracing::warn;

use super::super::ollama::{GenerateOptions, GenerateRequest, OllamaClient};
use super::extractive::ExtractiveSpan;

pub struct AbstractiveResult {
    pub narrative: Option<String>, // None iff LLM failed and caller should set the warning
    pub word_count: usize,
}

pub async fn summarize(
    client: &OllamaClient,
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
    let resp = client
        .generate(GenerateRequest {
            model,
            prompt: &prompt,
            system: None,
            stream: false,
            options: GenerateOptions {
                temperature: Some(0.2),
                top_p: Some(0.9),
                num_predict: Some(max_words * 2), // tokens > words; headroom
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
    #[allow(clippy::await_holding_lock)]
    async fn empty_spans_emit_placeholder() {
        let _env_guard = ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        let client = OllamaClient::new("http://unused", std::time::Duration::from_secs(1));
        let r = summarize(
            &client,
            "qwen3:14b",
            &[],
            chrono::NaiveDate::from_ymd_opt(2026, 4, 19).unwrap(),
            400,
        )
        .await;
        assert!(r.narrative.as_deref().unwrap().contains("No significant"));
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn mock_narrative_happy_path() {
        let _env_guard = ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        let client = OllamaClient::new("http://unused", std::time::Duration::from_secs(1));
        let spans = vec![span(1, "hello world"), span(2, "compression works")];
        let r = summarize(
            &client,
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
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }

    #[test]
    fn clean_output_strips_trailing_commentary() {
        let raw = "This is the narrative.\n\nLet me know if you'd like more detail!";
        assert_eq!(clean_output(raw), "This is the narrative.");
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn placeholder_word_count_matches_string() {
        // Guard: the empty-day placeholder word_count must match its actual
        // whitespace split. Prior version hardcoded 5 vs the 6-word string
        // "No significant activity on this day."
        let _env_guard = ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        let client = OllamaClient::new("http://unused", std::time::Duration::from_secs(1));
        let r = summarize(
            &client,
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
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }
}

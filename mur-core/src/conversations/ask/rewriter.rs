//! Query rewriting for Phase 3.3 `mur ask --continue` (§5 of spec).
//!
//! One Ollama call per follow-up turn. Canonical LangChain
//! "condense question" prompt — see `CONDENSE_PROMPT`.
//! Failure modes (timeout / empty / etc.) fall back to raw question.
#![allow(dead_code)] // wired by Task 7.

use crate::conversations::ollama::{GenerateOptions, GenerateRequest, OllamaClient};

use super::session::{RewriterStatus, TurnRecord};

/// Canonical LangChain "condense question" prompt. Verbatim from the
/// LangChain docs — widely used across LangChain/LlamaIndex/Haystack.
/// The "return it as is" clause means identity is always a legal output.
pub(crate) const CONDENSE_PROMPT_TEMPLATE: &str = "Given a chat history and the latest user question \
which might reference context in the chat history, formulate a standalone \
question which can be understood without the chat history. Do NOT answer \
the question, just reformulate it if needed and otherwise return it as is.\n\n\
Chat history:\n{history}\n\n\
Latest question: {question}\n\n\
Standalone question:";

/// Max chars of a prior turn's answer to include in the rewriter's `{history}`.
/// Keeps the rewrite prompt bounded; the full answer is not needed to resolve anaphora.
pub(crate) const PRIOR_ANSWER_TRUNCATE_CHARS: usize = 500;

pub struct RewriteInput<'a> {
    pub prior_turns: &'a [TurnRecord],
    pub raw_question: &'a str,
}

pub struct RewriteResult {
    pub rewritten: String,
    pub status: RewriterStatus,
}

/// Render prior turns into the `{history}` substitution for CONDENSE_PROMPT.
/// Each turn → "User: <q>\nAssistant: <a_truncated>\n".
pub(crate) fn render_history(prior_turns: &[TurnRecord]) -> String {
    let mut out = String::new();
    for t in prior_turns {
        out.push_str("User: ");
        out.push_str(&t.question);
        out.push('\n');
        out.push_str("Assistant: ");
        out.push_str(&truncate_chars(&t.answer, PRIOR_ANSWER_TRUNCATE_CHARS));
        out.push('\n');
    }
    out
}

fn truncate_chars(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect::<String>() + "…"
    }
}

/// Decontextualize `raw_question` against `prior_turns`.
///
/// Short-circuits to `Skipped` when `prior_turns` is empty — no LLM call.
/// On Ollama error, returns `FailedFellBackToRaw`. On identity echo
/// (trimmed, case-insensitive, ignoring trailing `?!.`), returns `NoRewriteNeeded`.
pub async fn rewrite(client: &OllamaClient, model: &str, input: RewriteInput<'_>) -> RewriteResult {
    if input.prior_turns.is_empty() {
        return RewriteResult {
            rewritten: input.raw_question.to_string(),
            status: RewriterStatus::Skipped,
        };
    }

    let history = render_history(input.prior_turns);
    let prompt = CONDENSE_PROMPT_TEMPLATE
        .replace("{history}", &history)
        .replace("{question}", input.raw_question);

    let resp = client
        .generate(GenerateRequest {
            model,
            prompt: &prompt,
            system: None,
            stream: false,
            options: GenerateOptions {
                temperature: Some(0.1),
                top_p: Some(0.9),
                num_predict: Some(80),
                stop: vec!["\n".into()],
            },
        })
        .await;

    match resp {
        Err(e) => {
            tracing::warn!("rewriter Ollama error: {e:#}");
            RewriteResult {
                rewritten: input.raw_question.to_string(),
                status: RewriterStatus::FailedFellBackToRaw,
            }
        }
        Ok(r) => {
            let trimmed = r.response.trim().to_string();
            if trimmed.is_empty() {
                tracing::warn!("rewriter returned empty response; falling back to raw");
                return RewriteResult {
                    rewritten: input.raw_question.to_string(),
                    status: RewriterStatus::FailedFellBackToRaw,
                };
            }
            let status = if normalize_for_compare(&trimmed)
                == normalize_for_compare(input.raw_question)
            {
                RewriterStatus::NoRewriteNeeded
            } else {
                RewriterStatus::Rewrote
            };
            RewriteResult {
                rewritten: if status == RewriterStatus::NoRewriteNeeded {
                    input.raw_question.to_string()
                } else {
                    trimmed
                },
                status,
            }
        }
    }
}

/// Lowercase + strip trailing `?!.` + whitespace. Used so a rewriter that
/// echoes back a question with different trailing punctuation still resolves
/// to `NoRewriteNeeded` rather than being mislabelled as `Rewrote`.
fn normalize_for_compare(s: &str) -> String {
    s.trim()
        .trim_end_matches(['?', '!', '.'])
        .trim()
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn trec(id: u32, q: &str, a: &str) -> TurnRecord {
        TurnRecord {
            v: 1,
            turn_id: id,
            ts: chrono::DateTime::parse_from_rfc3339("2026-04-21T15:30:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
            question: q.into(),
            rewritten_question: None,
            hits_used: vec![],
            answer: a.into(),
            citations: vec![],
            degraded_to_mode_b: false,
            rewriter_status: RewriterStatus::Skipped,
            tokens_in: 0,
            tokens_out: 0,
            duration_ms: 0,
        }
    }

    #[test]
    fn render_history_truncates_long_answers() {
        let long_answer = "x".repeat(2000);
        let turns = vec![trec(1, "what?", &long_answer)];
        let rendered = render_history(&turns);
        assert!(rendered.contains("User: what?"));
        // Should contain at most PRIOR_ANSWER_TRUNCATE_CHARS of the answer
        // plus an ellipsis "…".
        assert!(rendered.contains("x".repeat(PRIOR_ANSWER_TRUNCATE_CHARS).as_str()));
        assert!(!rendered.contains("x".repeat(PRIOR_ANSWER_TRUNCATE_CHARS + 1).as_str()));
        assert!(rendered.contains('…'));
    }

    #[test]
    fn render_history_multiple_turns_in_order() {
        let turns = vec![trec(1, "q1", "a1"), trec(2, "q2", "a2")];
        let rendered = render_history(&turns);
        let q1_pos = rendered.find("User: q1").unwrap();
        let q2_pos = rendered.find("User: q2").unwrap();
        assert!(q1_pos < q2_pos);
        assert!(rendered.contains("Assistant: a1"));
        assert!(rendered.contains("Assistant: a2"));
    }

    #[tokio::test]
    async fn empty_prior_turns_returns_identity_without_calling_ollama() {
        // Unreachable endpoint — if we accidentally call Ollama, we'd panic/error.
        let client = OllamaClient::new("http://127.0.0.1:1", Duration::from_millis(100));
        let input = RewriteInput {
            prior_turns: &[],
            raw_question: "what did I ship?",
        };
        let r = rewrite(&client, "qwen3:14b", input).await;
        assert_eq!(r.status, RewriterStatus::Skipped);
        assert_eq!(r.rewritten, "what did I ship?");
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn connection_failure_returns_fallback_to_raw() {
        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
        let client = OllamaClient::new("http://127.0.0.1:1", Duration::from_millis(200));
        let turns = vec![trec(1, "first q", "first a")];
        let input = RewriteInput {
            prior_turns: &turns,
            raw_question: "follow up",
        };
        let r = rewrite(&client, "qwen3:14b", input).await;
        assert_eq!(r.status, RewriterStatus::FailedFellBackToRaw);
        assert_eq!(r.rewritten, "follow up");
    }
}

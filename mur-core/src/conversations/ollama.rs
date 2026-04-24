//! Shared Ollama HTTP client used by summarize and ask modules.
//!
//! Covers both non-streaming (`generate`) and streaming (`generate_stream`)
//! endpoints. MUR_OLLAMA_MOCK=1 env short-circuits to canned responses for
//! deterministic testing; see docs/superpowers/specs/...phase-2... §9.3.

#![allow(dead_code)] // Phase 2A: generate_stream wired in Phase 2B.

use anyhow::{Context, Result, anyhow};
use futures::stream::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use std::pin::Pin;
use std::time::Duration;

/// Embedding-side mock mode. `generate()`/`generate_stream()` still branch
/// on `mock_from_env()` for their canned responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MockMode {
    /// MUR_OLLAMA_MOCK=1 — legacy uniform 0.1 vector. Fine for tests that
    /// only care about code paths.
    All01,
    /// MUR_OLLAMA_MOCK=hash — content-hash-based vector; same text → same
    /// vector, different text → different vector. Required for tests that
    /// assert span-selection picked the right span.
    Hash,
}

pub fn mock_mode() -> Option<MockMode> {
    match std::env::var("MUR_OLLAMA_MOCK").as_deref() {
        Ok("1") => Some(MockMode::All01),
        Ok("hash") => Some(MockMode::Hash),
        _ => None,
    }
}

/// Deterministic fake embedding for tests. Seeded from sha256(text);
/// L2-normalized so cosine similarity is meaningful.
pub fn mock_embed_vector(text: &str, mode: MockMode, dims: usize) -> Vec<f32> {
    match mode {
        MockMode::All01 => vec![0.1; dims],
        MockMode::Hash => {
            use sha2::{Digest, Sha256};
            let mut hasher = Sha256::new();
            hasher.update(text.as_bytes());
            let seed = hasher.finalize(); // 32 bytes
            let mut out = Vec::with_capacity(dims);
            for i in 0..dims {
                let byte_idx = (i * 4) % 32;
                let u = u32::from_le_bytes([
                    seed[byte_idx],
                    seed[(byte_idx + 1) % 32],
                    seed[(byte_idx + 2) % 32],
                    seed[(byte_idx + 3) % 32],
                ]);
                // Mix with position to break the 8-way periodicity from the
                // 32-byte seed being shorter than 4 * dims for dims > 8.
                let mixed = u.wrapping_add((i as u32).wrapping_mul(2_654_435_761));
                let f = (mixed as f32 / u32::MAX as f32) * 2.0 - 1.0;
                out.push(f);
            }
            let norm: f32 = out.iter().map(|x| x * x).sum::<f32>().sqrt();
            if norm > 0.0 {
                for x in out.iter_mut() {
                    *x /= norm;
                }
            }
            out
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct GenerateRequest<'a> {
    pub model: &'a str,
    pub prompt: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<&'a str>,
    pub stream: bool,
    pub options: GenerateOptions,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct GenerateOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub num_predict: Option<u32>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub stop: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GenerateResponse {
    pub response: String,
    pub done: bool,
    pub model: String,
    #[serde(default)]
    pub prompt_eval_count: u64,
    #[serde(default)]
    pub eval_count: u64,
}

pub struct OllamaClient {
    endpoint: String,
    timeout: Duration,
    http: reqwest::Client,
}

impl OllamaClient {
    pub fn new(endpoint: &str, timeout: Duration) -> Self {
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .expect("reqwest client build");
        Self {
            endpoint: endpoint.to_string(),
            timeout,
            http,
        }
    }

    pub fn mock_from_env() -> bool {
        mock_mode().is_some()
    }

    pub async fn generate(&self, req: GenerateRequest<'_>) -> Result<GenerateResponse> {
        if Self::mock_from_env() {
            // Phase 3.5: simulate a slow LLM when the caller opts in via
            // MUR_ABSTRACTIVE_MOCK_FAIL=timeout AND the request looks
            // abstractive. Lets `tokio::time::timeout` fire in tests without
            // a real server.
            let is_abstractive = req
                .system
                .map(|s| s.contains("You compress text for retrieval context"))
                .unwrap_or(false);
            if is_abstractive
                && std::env::var("MUR_ABSTRACTIVE_MOCK_FAIL").as_deref() == Ok("timeout")
            {
                tokio::time::sleep(Duration::from_secs(10)).await;
            }
            return Ok(mock_generate(&req));
        }
        let url = format!("{}/api/generate", self.endpoint.trim_end_matches('/'));
        let resp = self
            .http
            .post(&url)
            .json(&req)
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("ollama {status}: {body}"));
        }
        Ok(resp.json::<GenerateResponse>().await?)
    }

    pub async fn generate_stream(
        &self,
        req: GenerateRequest<'_>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
        if Self::mock_from_env() {
            let full = mock_generate(&req).response;
            let tokens: Vec<String> = full.split_inclusive(' ').map(|s| s.to_string()).collect();
            let stream = futures::stream::iter(tokens.into_iter().map(Ok));
            return Ok(Box::pin(stream));
        }
        let url = format!("{}/api/generate", self.endpoint.trim_end_matches('/'));
        let mut req = req;
        req.stream = true;
        let resp = self
            .http
            .post(&url)
            .json(&req)
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("ollama {status}: {body}"));
        }
        let byte_stream = resp.bytes_stream();
        let token_stream = futures::stream::unfold(
            (byte_stream, String::new(), false),
            |(mut inner, mut buf, done)| async move {
                if done {
                    return None;
                }
                loop {
                    // Emit any complete line already in the buffer.
                    if let Some(nl) = buf.find('\n') {
                        let line: String = buf.drain(..=nl).collect();
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        match serde_json::from_str::<GenerateResponse>(trimmed) {
                            Ok(v) => {
                                if !v.response.is_empty() {
                                    return Some((Ok(v.response), (inner, buf, false)));
                                }
                                // Empty response — keep draining.
                                continue;
                            }
                            Err(e) => {
                                return Some((Err(e.into()), (inner, buf, true)));
                            }
                        }
                    }
                    // Need more bytes.
                    match inner.next().await {
                        Some(Ok(bytes)) => match std::str::from_utf8(&bytes) {
                            Ok(s) => buf.push_str(s),
                            Err(e) => {
                                return Some((Err(e.into()), (inner, buf, true)));
                            }
                        },
                        Some(Err(e)) => {
                            return Some((Err(e.into()), (inner, buf, true)));
                        }
                        None => {
                            // EOF: if anything remains, flush it as a final record.
                            let trimmed = buf.trim();
                            if trimmed.is_empty() {
                                return None;
                            }
                            let result = match serde_json::from_str::<GenerateResponse>(trimmed) {
                                Ok(v) if !v.response.is_empty() => Ok(v.response),
                                Ok(_) => return None,
                                Err(e) => Err(e.into()),
                            };
                            return Some((result, (inner, String::new(), true)));
                        }
                    }
                }
            },
        );
        Ok(Box::pin(token_stream))
    }
}

/// Given a CONDENSE-style prompt with "Latest question: <q>\n\nStandalone question:"
/// extract the raw `<q>` for the identity-echo mock path. Returns the literal
/// raw question on any parse failure (matches "return it as is" fallback).
fn extract_latest_question_from_condense_prompt(prompt: &str) -> String {
    let start_tag = "Latest question: ";
    let Some(start) = prompt.find(start_tag) else {
        return prompt.to_string();
    };
    let rest = &prompt[start + start_tag.len()..];
    let end = rest.find("\n\n").unwrap_or(rest.len());
    rest[..end].trim().to_string()
}

/// Deterministic fake response for tests. Echoes model+prompt hints so each
/// test can assert which call site fired without a real Ollama.
fn mock_generate(req: &GenerateRequest<'_>) -> GenerateResponse {
    let is_abstractive = req
        .system
        .map(|s| s.contains("You compress text for retrieval context"))
        .unwrap_or(false);

    let response = if is_abstractive {
        // Phase 3.5 abstractive mock. Honor MUR_ABSTRACTIVE_MOCK_FAIL for
        // soft-fail tests. Default path: echo a short deterministic summary
        // that is strictly shorter than the input content, so the validator
        // in abstractive::compress_hit accepts it.
        match std::env::var("MUR_ABSTRACTIVE_MOCK_FAIL").as_deref() {
            Ok("empty") => String::new(),
            Ok("not_shorter") => req.prompt.to_string() + " [MOCK PADDING MAKES THIS LONGER]",
            // `timeout` is handled upstream in `OllamaClient::generate` via
            // an actual `tokio::time::sleep`; if we reach here the caller's
            // timeout wasn't lower than the sleep, so produce a normal summary.
            _ => {
                // The prompt body starts after "\n\n". Take first 40 chars of
                // that, then " [mock summary]". Deterministic, strictly
                // shorter than the input for any content ≥ ~56 chars.
                let body = req
                    .prompt
                    .split_once("\n\n")
                    .map(|x| x.1)
                    .unwrap_or(req.prompt);
                let first_40: String = body.chars().take(40).collect();
                format!("{first_40} [mock summary]")
            }
        }
    } else if req
        .prompt
        .contains("Extract the 1-3 most informative spans")
    {
        r#"[{"role":"user","conv_id":"mock","line_hint":1,"text":"mock extractive span"}]"#
            .to_string()
    } else if req.prompt.contains("narrative paragraph") {
        if req.prompt.contains("one week") || req.prompt.contains("one-week") {
            "Mock narrative: this week the developer shipped several fixes and refactors."
                .to_string()
        } else if req.prompt.contains("one month") || req.prompt.contains("one-month") {
            "Mock narrative: this month saw major work on the conversations archive.".to_string()
        } else {
            "Mock narrative: today the developer explored mock compression.".to_string()
        }
    } else if req.prompt.trim_end().ends_with("Standalone question:") {
        // Match only CONDENSE prompts that END with this marker (the real
        // prompt template places it as the final line). `contains` would
        // mis-route any user question that happens to include the phrase.
        extract_latest_question_from_condense_prompt(req.prompt)
    } else if req.prompt.contains("[cit:") {
        "Mock answer about the archive [cit: 2026-04-19 claude-code/mock:L1].".to_string()
    } else {
        format!("mock response for model={}", req.model)
    };
    GenerateResponse {
        response,
        done: true,
        model: req.model.to_string(),
        prompt_eval_count: 10,
        eval_count: 20,
    }
}

#[cfg(test)]
mod tests {
    use super::super::ENV_LOCK;
    use super::*;

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn mock_mode_extractive_returns_valid_json() {
        let _env_guard = ENV_LOCK.lock().unwrap();
        // Given: MUR_OLLAMA_MOCK=1, extractive prompt
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        let client = OllamaClient::new("http://unused", Duration::from_secs(1));
        let req = GenerateRequest {
            model: "qwen3:14b",
            prompt: "Extract the 1-3 most informative spans from this excerpt.",
            system: None,
            stream: false,
            options: GenerateOptions::default(),
        };
        let resp = client.generate(req).await.unwrap();
        assert!(resp.response.contains("mock extractive span"));
        assert!(serde_json::from_str::<serde_json::Value>(&resp.response).is_ok());
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn mock_mode_abstractive_returns_prose() {
        let _env_guard = ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        let client = OllamaClient::new("http://unused", Duration::from_secs(1));
        let req = GenerateRequest {
            model: "qwen3:14b",
            prompt: "Write the narrative paragraph.",
            system: None,
            stream: false,
            options: GenerateOptions::default(),
        };
        let resp = client.generate(req).await.unwrap();
        assert!(resp.response.starts_with("Mock narrative"));
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn real_call_errors_on_unreachable_endpoint() {
        let _env_guard = ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
        // Use a deliberately-unroutable port so we get a fast failure
        let client = OllamaClient::new("http://127.0.0.1:1", Duration::from_millis(500));
        let req = GenerateRequest {
            model: "m",
            prompt: "p",
            system: None,
            stream: false,
            options: GenerateOptions::default(),
        };
        let r = client.generate(req).await;
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn stream_parser_joins_split_lines() {
        // Simulate bytes_stream yielding chunks that split a JSON record mid-line.
        // Chunk A ends with {"response":"hello, chunk B starts with ","done":false...}\n
        // The key point: a single JSON record {"response":"hello","done":false,"model":"m"}
        // is split across two chunks. Without buffering, the parser would fail or drop tokens.
        let chunks: Vec<Result<Vec<u8>, anyhow::Error>> = vec![
            Ok(br#"{"response":"hel"#.to_vec()),
            Ok(br#"lo","done":false,"model":"m"}"#.to_vec()),
            Ok(b"\n".to_vec()),
            Ok(br#"{"response":"world","done":true,"model":"m"}"#.to_vec()),
            Ok(b"\n".to_vec()),
        ];
        let byte_stream = futures::stream::iter(chunks);
        // Replicate the unfold logic from generate_stream.
        let token_stream: Pin<Box<dyn Stream<Item = Result<String>> + Send>> =
            Box::pin(futures::stream::unfold(
                (byte_stream, String::new(), false),
                |(mut inner, mut buf, done)| async move {
                    if done {
                        return None;
                    }
                    loop {
                        if let Some(nl) = buf.find('\n') {
                            let line: String = buf.drain(..=nl).collect();
                            let trimmed = line.trim();
                            if trimmed.is_empty() {
                                continue;
                            }
                            match serde_json::from_str::<GenerateResponse>(trimmed) {
                                Ok(v) => {
                                    if !v.response.is_empty() {
                                        return Some((Ok(v.response), (inner, buf, false)));
                                    }
                                    continue;
                                }
                                Err(e) => {
                                    return Some((Err(anyhow::anyhow!(e)), (inner, buf, true)));
                                }
                            }
                        }
                        match inner.next().await {
                            Some(Ok(bytes)) => match std::str::from_utf8(&bytes) {
                                Ok(s) => buf.push_str(s),
                                Err(e) => {
                                    return Some((Err(anyhow::anyhow!(e)), (inner, buf, true)));
                                }
                            },
                            Some(Err(e)) => {
                                return Some((Err(e), (inner, buf, true)));
                            }
                            None => {
                                let trimmed = buf.trim();
                                if trimmed.is_empty() {
                                    return None;
                                }
                                let result: Result<String> =
                                    match serde_json::from_str::<GenerateResponse>(trimmed) {
                                        Ok(v) if !v.response.is_empty() => Ok(v.response),
                                        Ok(_) => return None,
                                        Err(e) => Err(anyhow::anyhow!(e)),
                                    };
                                return Some((result, (inner, String::new(), true)));
                            }
                        }
                    }
                },
            ));
        // Collect the tokens and verify they match expected order.
        // The test demonstrates that even though the first JSON record is split
        // across chunks A and B, both tokens ("hello" and "world") are correctly extracted.
        let mut tokens = Vec::new();
        futures::pin_mut!(token_stream);
        while let Some(result) = token_stream.next().await {
            match result {
                Ok(token) => tokens.push(token),
                Err(e) => panic!("unexpected error: {}", e),
            }
        }
        assert_eq!(tokens, vec!["hello", "world"]);
    }

    #[test]
    fn mock_mode_from_env_parses_both_variants() {
        let _env_guard = ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        assert!(matches!(mock_mode(), Some(MockMode::All01)));
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "hash") };
        assert!(matches!(mock_mode(), Some(MockMode::Hash)));
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "bogus") };
        assert!(mock_mode().is_none());
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
        assert!(mock_mode().is_none());
    }

    #[test]
    fn mock_embed_vector_all01_is_uniform() {
        let v = mock_embed_vector("anything", MockMode::All01, 16);
        assert_eq!(v.len(), 16);
        assert!(v.iter().all(|x| (*x - 0.1).abs() < 1e-9));
    }

    #[test]
    fn mock_embed_vector_hash_is_deterministic_and_distinct() {
        let a1 = mock_embed_vector("cargo build failed", MockMode::Hash, 128);
        let a2 = mock_embed_vector("cargo build failed", MockMode::Hash, 128);
        let b = mock_embed_vector("kubernetes pod crash", MockMode::Hash, 128);
        assert_eq!(a1, a2, "same text → same vector");
        assert_ne!(a1, b, "different text → different vector");
        let norm_a: f32 = a1.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!(
            (norm_a - 1.0).abs() < 1e-5,
            "not L2-normalized: norm={norm_a}"
        );
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn mock_returns_week_narrative_for_week_prompt() {
        let _env_guard = ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        let client = OllamaClient::new("http://unused", Duration::from_secs(1));
        let req = GenerateRequest {
            model: "qwen3:14b",
            prompt: "You are summarizing one week (2026-W16) into a narrative paragraph.",
            system: None,
            stream: false,
            options: GenerateOptions::default(),
        };
        let resp = client.generate(req).await.unwrap();
        assert!(
            resp.response.to_lowercase().contains("this week"),
            "expected week-specific mock narrative; got: {}",
            resp.response
        );
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn mock_returns_month_narrative_for_month_prompt() {
        let _env_guard = ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        let client = OllamaClient::new("http://unused", Duration::from_secs(1));
        let req = GenerateRequest {
            model: "qwen3:14b",
            prompt: "You are summarizing one month (2026-04) into a narrative paragraph.",
            system: None,
            stream: false,
            options: GenerateOptions::default(),
        };
        let resp = client.generate(req).await.unwrap();
        assert!(
            resp.response.to_lowercase().contains("this month"),
            "expected month-specific mock narrative; got: {}",
            resp.response
        );
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn mock_abstractive_branch_returns_shorter_summary() {
        let _env_guard = ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        unsafe { std::env::remove_var("MUR_ABSTRACTIVE_MOCK_FAIL") };
        let client = OllamaClient::new("http://unused", Duration::from_secs(1));
        let body: String = "fact ".repeat(30);
        let prompt = format!("Summarize the following in ≤64 tokens.\n\n{body}");
        let req = GenerateRequest {
            model: "m",
            prompt: &prompt,
            system: Some(
                "You compress text for retrieval context. Preserve entities, dates, numbers.",
            ),
            stream: false,
            options: GenerateOptions::default(),
        };
        let resp = client.generate(req).await.unwrap();
        assert!(resp.response.contains("[mock summary]"));
        assert!(resp.response.len() < body.len());
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn mock_abstractive_fail_empty_returns_empty() {
        let _env_guard = ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        unsafe { std::env::set_var("MUR_ABSTRACTIVE_MOCK_FAIL", "empty") };
        let client = OllamaClient::new("http://unused", Duration::from_secs(1));
        let req = GenerateRequest {
            model: "m",
            prompt: "Summarize the following in ≤64 tokens.\n\nlong body here",
            system: Some("You compress text for retrieval context."),
            stream: false,
            options: GenerateOptions::default(),
        };
        let resp = client.generate(req).await.unwrap();
        assert_eq!(resp.response, "");
        unsafe { std::env::remove_var("MUR_ABSTRACTIVE_MOCK_FAIL") };
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn mock_returns_identity_for_standalone_question_prompt() {
        let _env_guard = ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        let client = OllamaClient::new("http://unused", Duration::from_secs(1));
        let prompt = "Given a chat history and the latest user question \
                     which might reference context in the chat history, \
                     formulate a standalone question which can be understood \
                     without the chat history. Do NOT answer the question, \
                     just reformulate it if needed and otherwise return it as is.\n\n\
                     Chat history:\nUser: q1\nAssistant: a1\n\n\
                     Latest question: what did I ship yesterday?\n\n\
                     Standalone question:";
        let req = GenerateRequest {
            model: "qwen3:14b",
            prompt,
            system: None,
            stream: false,
            options: GenerateOptions::default(),
        };
        let resp = client.generate(req).await.unwrap();
        assert_eq!(
            resp.response.trim(),
            "what did I ship yesterday?",
            "mock should echo the raw 'Latest question:' as the standalone form"
        );
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }
}

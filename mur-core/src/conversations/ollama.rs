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
        std::env::var("MUR_OLLAMA_MOCK").as_deref() == Ok("1")
    }

    pub async fn generate(&self, req: GenerateRequest<'_>) -> Result<GenerateResponse> {
        if Self::mock_from_env() {
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
        let resp = self.http.post(&url).json(&req).send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("ollama {status}: {body}"));
        }
        let byte_stream = resp.bytes_stream();
        let token_stream = byte_stream
            .map(|chunk| -> Result<Vec<String>> {
                let bytes = chunk?;
                let text = std::str::from_utf8(&bytes)?;
                let mut out = Vec::new();
                for line in text.lines() {
                    if line.trim().is_empty() {
                        continue;
                    }
                    let v: GenerateResponse = serde_json::from_str(line)?;
                    if !v.response.is_empty() {
                        out.push(v.response);
                    }
                }
                Ok(out)
            })
            .flat_map(|res| match res {
                Ok(tokens) => futures::stream::iter(tokens.into_iter().map(Ok).collect::<Vec<_>>()),
                Err(e) => futures::stream::iter(vec![Err(e)]),
            });
        Ok(Box::pin(token_stream))
    }
}

/// Deterministic fake response for tests. Echoes model+prompt hints so each
/// test can assert which call site fired without a real Ollama.
fn mock_generate(req: &GenerateRequest<'_>) -> GenerateResponse {
    let response = if req
        .prompt
        .contains("Extract the 1-3 most informative spans")
    {
        // extractive stage: one valid span echoed as JSON array
        r#"[{"role":"user","conv_id":"mock","line_hint":1,"text":"mock extractive span"}]"#
            .to_string()
    } else if req.prompt.contains("narrative paragraph") {
        "Mock narrative: today the developer explored mock compression.".to_string()
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
    use super::*;

    #[tokio::test]
    async fn mock_mode_extractive_returns_valid_json() {
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
    async fn mock_mode_abstractive_returns_prose() {
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
    async fn real_call_errors_on_unreachable_endpoint() {
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
}
